use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::Duration;

use librespot::connect::{ConnectConfig, Spirc};
use librespot::core::{
    authentication::Credentials,
    cache::Cache,
    config::{DeviceType, SessionConfig},
    session::Session,
    Error,
};
use librespot::oauth::OAuthClientBuilder;
use librespot::playback::{
    audio_backend::{Sink, SinkError, SinkResult},
    config::{Bitrate, PlayerConfig, VolumeCtrl},
    convert::Converter,
    decoder::AudioPacket,
    mixer::{softmixer::SoftMixer, Mixer, MixerConfig},
    player::{Player, PlayerEventChannel},
    NUM_CHANNELS, SAMPLE_RATE,
};
use serenity::prelude::TypeMapKey;
use songbird::input::{core::io::MediaSource, Input, RawAdapter};

/// How long the Spotify player waits for Discord to consume audio before dropping it.
/// Long enough to cover joining a voice channel without losing the start of a track, but
/// prevents librespot from stalling forever if the bot never joins.
const SEND_TIMEOUT: Duration = Duration::from_secs(3);
/// How long the Discord side waits for audio before emitting a short burst of silence.
/// Keeps songbird's mixer responsive while Spotify is paused or buffering.
const RECV_TIMEOUT: Duration = Duration::from_millis(100);
/// 20ms of stereo f32 silence at 44.1kHz.
const SILENCE_SAMPLES: usize = (SAMPLE_RATE as usize / 50) * NUM_CHANNELS as usize;

const OAUTH_SCOPES: &[&str] = &[
    "streaming",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
];

/// Spotify only accepts specific redirect URIs for its own client ID. Using one without a port
/// makes librespot ask for the redirected URL on stdin, which works inside Docker.
const OAUTH_REDIRECT_URI: &str = "http://127.0.0.1/login";

pub fn open_cache(cache_dir: &str) -> Result<Cache, Error> {
    let audio_dir = format!("{cache_dir}/audio");
    // 4 GB
    let audio_limit = 4 * 10u64.pow(9);
    Cache::new(
        Some(cache_dir),
        Some(cache_dir),
        Some(audio_dir.as_str()),
        Some(audio_limit),
    )
}

/// Interactive one-time login. Stores reusable credentials in the cache.
pub async fn login(cache: Cache) -> Result<String, Error> {
    let session_config = SessionConfig::default();
    let client_id = session_config.client_id.clone();

    let token = tokio::task::spawn_blocking(move || {
        OAuthClientBuilder::new(&client_id, OAUTH_REDIRECT_URI, OAUTH_SCOPES.to_vec())
            .build()
            .and_then(|client| client.get_access_token())
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?
    .map_err(|e| Error::unauthenticated(e.to_string()))?;

    let session = Session::new(session_config, Some(cache));
    session
        .connect(Credentials::with_access_token(token.access_token), true)
        .await?;
    let username = session.username();
    session.shutdown();
    Ok(username)
}

/// Shared state between the librespot sink (producer) and songbird input (consumer).
struct AudioChannel {
    sender: flume::Sender<Vec<f32>>,
    receiver: flume::Receiver<Vec<f32>>,
}

/// librespot audio backend that forwards decoded samples to Discord.
pub struct EmittedSink {
    channel: Arc<AudioChannel>,
}

impl Sink for EmittedSink {
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|e| SinkError::OnWrite(e.to_string()))?;
        let samples = converter.f64_to_f32(samples);

        // Dropping audio on timeout is intentional: nobody is listening.
        let _ = self.channel.sender.send_timeout(samples, SEND_TIMEOUT);
        Ok(())
    }
}

/// songbird media source reading raw interleaved stereo f32 PCM from the sink.
pub struct SpotifyAudio {
    channel: Arc<AudioChannel>,
    pending: Vec<f32>,
    position: usize,
}

impl SpotifyAudio {
    fn write_samples(buf: &mut [u8], samples: &[f32]) -> usize {
        let count = samples.len().min(buf.len() / 4);
        for (chunk, sample) in buf.chunks_exact_mut(4).zip(&samples[..count]) {
            chunk.copy_from_slice(&sample.to_le_bytes());
        }
        count * 4
    }
}

impl Read for SpotifyAudio {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read buffer too small to hold one f32 sample",
            ));
        }

        loop {
            if self.position < self.pending.len() {
                let written = Self::write_samples(buf, &self.pending[self.position..]);
                self.position += written / 4;
                return Ok(written);
            }

            match self.channel.receiver.recv_timeout(RECV_TIMEOUT) {
                Ok(samples) => {
                    self.pending = samples;
                    self.position = 0;
                }
                // Returning 0 would end the track, so emit silence instead.
                Err(flume::RecvTimeoutError::Timeout) => {
                    let len = buf.len().min(SILENCE_SAMPLES * 4) / 4 * 4;
                    buf[..len].fill(0);
                    return Ok(len);
                }
                Err(flume::RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
    }
}

impl Seek for SpotifyAudio {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

impl MediaSource for SpotifyAudio {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

pub struct SpotifyPlayer {
    session_config: SessionConfig,
    connect_config: ConnectConfig,
    cache: Cache,
    session: Session,
    player: Arc<Player>,
    mixer: Arc<SoftMixer>,
    spirc: Option<Spirc>,
    audio: Arc<AudioChannel>,
}

pub struct SpotifyPlayerKey;

impl TypeMapKey for SpotifyPlayerKey {
    type Value = Arc<tokio::sync::Mutex<SpotifyPlayer>>;
}

impl SpotifyPlayer {
    pub fn new(
        cache: Cache,
        bitrate: Bitrate,
        autoplay: Option<bool>,
        device_name: String,
    ) -> Result<SpotifyPlayer, Error> {
        let session_config = SessionConfig {
            autoplay,
            ..SessionConfig::default()
        };
        let connect_config = ConnectConfig {
            name: device_name,
            device_type: DeviceType::AudioDongle,
            ..ConnectConfig::default()
        };

        let session = Session::new(session_config.clone(), Some(cache.clone()));

        let (sender, receiver) = flume::bounded(4);
        let audio = Arc::new(AudioChannel { sender, receiver });

        let mixer = Arc::new(SoftMixer::open(MixerConfig {
            volume_ctrl: VolumeCtrl::Linear,
            ..MixerConfig::default()
        })?);

        let player_config = PlayerConfig {
            bitrate,
            ..PlayerConfig::default()
        };
        let sink_channel = audio.clone();
        let player = Player::new(
            player_config,
            session.clone(),
            mixer.get_soft_volume(),
            move || {
                Box::new(EmittedSink {
                    channel: sink_channel,
                })
            },
        );

        Ok(SpotifyPlayer {
            session_config,
            connect_config,
            cache,
            session,
            player,
            mixer,
            spirc: None,
            audio,
        })
    }

    pub fn has_credentials(&self) -> bool {
        self.cache.credentials().is_some()
    }

    pub fn event_channel(&self) -> PlayerEventChannel {
        self.player.get_player_event_channel()
    }

    /// A fresh songbird input fed by the Spotify player.
    pub fn input(&self) -> Input {
        let source = SpotifyAudio {
            channel: self.audio.clone(),
            pending: Vec::new(),
            position: 0,
        };
        RawAdapter::new(source, SAMPLE_RATE, NUM_CHANNELS as u32).into()
    }

    /// Make the bot visible as a Spotify Connect device. Does nothing if already enabled.
    pub async fn enable_connect(&mut self) -> Result<(), Error> {
        if self.spirc.is_some() {
            return Ok(());
        }

        let credentials = self.cache.credentials().ok_or_else(|| {
            Error::unauthenticated("no cached Spotify credentials, run `aoede login` first")
        })?;

        // A session can only be connected once, so each Connect device gets a new one.
        if self.session.is_invalid() || !self.session.username().is_empty() {
            self.session.shutdown();
            self.session = Session::new(self.session_config.clone(), Some(self.cache.clone()));
            self.player.set_session(self.session.clone());
        }

        let (spirc, task) = Spirc::new(
            self.connect_config.clone(),
            self.session.clone(),
            credentials,
            self.player.clone(),
            self.mixer.clone(),
        )
        .await?;

        tokio::spawn(task);
        self.spirc = Some(spirc);
        Ok(())
    }

    pub fn disable_connect(&mut self) {
        if let Some(spirc) = self.spirc.take() {
            if let Err(e) = spirc.shutdown() {
                tracing::warn!("Failed to shut down Spotify Connect: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio_pair() -> (Arc<AudioChannel>, SpotifyAudio) {
        let (sender, receiver) = flume::bounded(4);
        let channel = Arc::new(AudioChannel { sender, receiver });
        let audio = SpotifyAudio {
            channel: channel.clone(),
            pending: Vec::new(),
            position: 0,
        };
        (channel, audio)
    }

    #[test]
    fn reads_samples_across_small_buffers() {
        let (channel, mut audio) = audio_pair();
        channel.sender.send(vec![1.0, -1.0, 0.5]).unwrap();

        let mut buf = [0u8; 8];
        assert_eq!(audio.read(&mut buf).unwrap(), 8);
        assert_eq!(f32::from_le_bytes(buf[0..4].try_into().unwrap()), 1.0);
        assert_eq!(f32::from_le_bytes(buf[4..8].try_into().unwrap()), -1.0);

        assert_eq!(audio.read(&mut buf).unwrap(), 4);
        assert_eq!(f32::from_le_bytes(buf[0..4].try_into().unwrap()), 0.5);
    }

    #[test]
    fn emits_silence_when_no_audio_arrives() {
        let (_channel, mut audio) = audio_pair();
        let mut buf = [0xffu8; 64 * 1024];
        let read = audio.read(&mut buf).unwrap();
        assert_eq!(read, SILENCE_SAMPLES * 4);
        assert!(buf[..read].iter().all(|&b| b == 0));
    }

    #[test]
    fn rejects_buffers_smaller_than_a_sample() {
        let (_channel, mut audio) = audio_pair();
        assert!(audio.read(&mut [0u8; 3]).is_err());
    }
}
