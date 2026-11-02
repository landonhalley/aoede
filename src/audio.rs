//! Captures Soloist's audio from a PulseAudio null sink and exposes it to songbird.

use std::io::{self, Read, Seek, SeekFrom};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use songbird::input::{core::io::MediaSource, Input, RawAdapter};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// 20ms of interleaved stereo samples, matching Discord's frame size.
const CHUNK_SAMPLES: usize = (SAMPLE_RATE as usize / 50) * CHANNELS as usize;
/// Max buffered chunks (~200ms). Keeps latency low; older audio is dropped when nobody listens.
const CHANNEL_CHUNKS: usize = 10;
/// How long the Discord side waits for audio before emitting a short burst of silence.
const RECV_TIMEOUT: Duration = Duration::from_millis(100);

struct AudioChannel {
    sender: flume::Sender<Vec<f32>>,
    receiver: flume::Receiver<Vec<f32>>,
}

/// Records the monitor of a PulseAudio sink with `parec`, restarting it if it exits.
#[derive(Clone)]
pub struct Capture {
    channel: Arc<AudioChannel>,
}

impl Capture {
    pub fn start(sink: String) -> Capture {
        let (sender, receiver) = flume::bounded(CHANNEL_CHUNKS);
        let channel = Arc::new(AudioChannel { sender, receiver });

        let thread_channel = channel.clone();
        thread::Builder::new()
            .name("pulse-capture".into())
            .spawn(move || loop {
                if let Err(e) = run_parec(&sink, &thread_channel) {
                    tracing::warn!("Audio capture stopped: {e}");
                }
                thread::sleep(Duration::from_secs(1));
            })
            .expect("failed to spawn capture thread");

        Capture { channel }
    }

    /// A fresh songbird input fed by the capture.
    pub fn input(&self) -> Input {
        // Discard audio buffered while nobody was listening.
        self.channel.receiver.drain();

        let source = SinkAudio {
            channel: self.channel.clone(),
            pending: Vec::new(),
            position: 0,
        };
        RawAdapter::new(source, SAMPLE_RATE, CHANNELS).into()
    }
}

fn run_parec(sink: &str, channel: &AudioChannel) -> io::Result<()> {
    let mut child = Command::new("parec")
        .arg(format!("--device={sink}.monitor"))
        .args([
            "--format=float32le",
            "--rate=48000",
            "--channels=2",
            "--raw",
            "--latency-msec=20",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdout = child.stdout.take().expect("parec stdout is piped");
    let mut buf = vec![0u8; CHUNK_SAMPLES * 4];
    let result = loop {
        if let Err(e) = stdout.read_exact(&mut buf) {
            break Err(e);
        }
        let samples = buf
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        // Dropping audio when the buffer is full is intentional: nobody is listening.
        let _ = channel.sender.try_send(samples);
    };

    let _ = child.kill();
    let _ = child.wait();
    result
}

/// Make sure the null sink exists. Returns whether it was already there or got created.
pub fn ensure_sink(sink: &str) -> bool {
    let exists = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.split_whitespace().nth(1) == Some(sink))
        })
        .unwrap_or(false);
    if exists {
        return true;
    }

    let created = Command::new("pactl")
        .args([
            "load-module",
            "module-null-sink",
            &format!("sink_name={sink}"),
            "rate=48000",
            "channels=2",
            &format!("sink_properties=device.description={sink}"),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if created {
        tracing::info!("Created PulseAudio sink '{sink}'");
    }
    created
}

/// songbird media source reading raw interleaved stereo f32 PCM from the capture.
struct SinkAudio {
    channel: Arc<AudioChannel>,
    pending: Vec<f32>,
    position: usize,
}

impl SinkAudio {
    fn write_samples(buf: &mut [u8], samples: &[f32]) -> usize {
        let count = samples.len().min(buf.len() / 4);
        for (chunk, sample) in buf.as_chunks_mut::<4>().0.iter_mut().zip(&samples[..count]) {
            chunk.copy_from_slice(&sample.to_le_bytes());
        }
        count * 4
    }
}

impl Read for SinkAudio {
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
                    let len = buf.len().min(CHUNK_SAMPLES * 4) / 4 * 4;
                    buf[..len].fill(0);
                    return Ok(len);
                }
                Err(flume::RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
    }
}

impl Seek for SinkAudio {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

impl MediaSource for SinkAudio {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio_pair() -> (Arc<AudioChannel>, SinkAudio) {
        let (sender, receiver) = flume::bounded(4);
        let channel = Arc::new(AudioChannel { sender, receiver });
        let audio = SinkAudio {
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
        assert_eq!(read, CHUNK_SAMPLES * 4);
        assert!(buf[..read].iter().all(|&b| b == 0));
    }

    #[test]
    fn rejects_buffers_smaller_than_a_sample() {
        let (_channel, mut audio) = audio_pair();
        assert!(audio.read(&mut [0u8; 3]).is_err());
    }
}
