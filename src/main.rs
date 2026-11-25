mod audio;
mod config;
mod soloist;

use std::env;
use std::path::PathBuf;
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use audio::Capture;
use config::Config;
use figment::error::Kind::MissingField;
use serenity::{
    async_trait,
    client::{Context, EventHandler},
    gateway::ActivityData,
    model::{
        gateway::{GatewayIntents, Ready},
        id::{ChannelId, GuildId, UserId},
        user::OnlineStatus,
        voice::VoiceState,
    },
    prelude::{TypeMap, TypeMapKey},
    Client,
};
use soloist::{Event, Soloist, SoloistConfig};
use songbird::{tracks::TrackHandle, SerenityInit};
use tokio::sync::{mpsc, Mutex};
use tokio::time::Instant;

const DEFAULT_CACHE_DIR: &str = "cache";
/// How many times to try joining a voice channel before giving up.
const JOIN_ATTEMPTS: u32 = 3;
/// Time for Discord to process a leave before rejoining. Rejoining sooner lets the
/// leave's voice state update cancel the new connection attempt.
const REJOIN_DELAY: Duration = Duration::from_millis(1500);
/// How long playback must stay stopped before leaving voice.
const LEAVE_DELAY: Duration = Duration::from_secs(10);

struct Handler {
    /// Soloist events, taken by the first `cache_ready`.
    events: Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
}

struct State {
    config: Config,
    soloist: Soloist,
    capture: Capture,
    /// Serializes joining and leaving voice. Overlapping joins cancel each other in songbird.
    voice: Mutex<()>,
    /// The audio track currently streaming to Discord.
    track: Mutex<Option<TrackHandle>>,
    /// Whether Aoede is the active Spotify Connect device. Soloist also reports what
    /// the account plays on other devices, which Aoede must leave alone.
    active: AtomicBool,
}

struct StateKey;
impl TypeMapKey for StateKey {
    type Value = Arc<State>;
}

async fn state(ctx: &Context) -> Arc<State> {
    ctx.data.read().await.get::<StateKey>().unwrap().clone()
}

fn user_id(state: &State) -> UserId {
    UserId::new(state.config.discord_user_id)
}

/// Find the voice channel the followed user is currently in.
fn find_user_channel(ctx: &Context, user_id: UserId) -> Option<(GuildId, ChannelId)> {
    ctx.cache.guilds().into_iter().find_map(|guild_id| {
        let guild = ctx.cache.guild(guild_id)?;
        let channel_id = guild.voice_states.get(&user_id)?.channel_id?;
        Some((guild_id, channel_id))
    })
}

/// Join the given channel (if not already there) and stream Spotify into it.
async fn join_and_play(ctx: &Context, state: &State, guild_id: GuildId, channel_id: ChannelId) {
    let _voice = state.voice.lock().await;
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.");

    // Only one stream exists, so leave any other guild first.
    for other in ctx.cache.guilds() {
        if other != guild_id && manager.get(other).is_some() {
            let _ = manager.remove(other).await;
        }
    }

    for attempt in 1..=JOIN_ATTEMPTS {
        if let Some(call) = manager.get(guild_id) {
            let connected = {
                let call = call.lock().await;
                call.current_connection().is_some()
                    && call.current_channel() == Some(channel_id.into())
            };
            // Calls are removed whenever playback stops, so a completed connection
            // to this channel is already streaming.
            if connected {
                // Restart the stream if the track ended or errored.
                let alive = match state.track.lock().await.as_ref() {
                    Some(track) => track.get_info().await.is_ok(),
                    None => false,
                };
                if !alive {
                    tracing::info!("Restarting audio stream");
                    let mut call = call.lock().await;
                    *state.track.lock().await = Some(call.play_only_input(state.capture.input()));
                }
                return;
            }
            // A half-finished or failed attempt: start from a clean slate, and give
            // Discord time to process the leave so it doesn't cancel the next join.
            let _ = manager.remove(guild_id).await;
            tokio::time::sleep(REJOIN_DELAY).await;
        }

        match manager.join(guild_id, channel_id).await {
            Ok(call) => {
                let mut call = call.lock().await;
                call.set_bitrate(songbird::driver::Bitrate::Auto);
                *state.track.lock().await = Some(call.play_only_input(state.capture.input()));
                tracing::info!("Joined voice channel {channel_id}");
                return;
            }
            Err(e) => tracing::warn!(
                "Could not join voice channel (attempt {attempt}/{JOIN_ATTEMPTS}): {e}"
            ),
        }
    }

    tracing::error!("Giving up joining voice channel {channel_id}");
    let _ = manager.remove(guild_id).await;
}

async fn leave_all(ctx: &Context, state: &State) {
    let _voice = state.voice.lock().await;
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.");
    for guild_id in ctx.cache.guilds() {
        if manager.get(guild_id).is_some() {
            let _ = manager.remove(guild_id).await;
        }
    }
}

fn set_listening(ctx: &Context, track: Option<&String>) {
    ctx.set_presence(
        track.map(|t| ActivityData::listening(t.clone())),
        OnlineStatus::Online,
    );
}

/// Aoede is the active device and Spotify is playing: stream to the followed user.
async fn start_streaming(ctx: &Context, state: &State, now_playing: Option<&String>) {
    match find_user_channel(ctx, user_id(state)) {
        Some((guild_id, channel_id)) => {
            join_and_play(ctx, state, guild_id, channel_id).await;
            set_listening(ctx, now_playing);
        }
        None => {
            tracing::warn!("You're not in a voice channel, pausing playback on Aoede.");
            state.soloist.send("pause");
        }
    }
}

async fn handle_soloist_events(ctx: Context, mut events: mpsc::UnboundedReceiver<Event>) {
    let state = state(&ctx).await;

    let mut now_playing: Option<String> = None;
    // Soloist's playback status, which can describe another device's playback.
    let mut status_playing = false;
    let mut active = false;
    // Soloist briefly reports idle/inactive while switching tracks or taking over
    // playback, so leaving voice waits to see if playback really stopped.
    let mut leave_at: Option<Instant> = None;

    loop {
        let event = tokio::select! {
            event = events.recv() => match event {
                Some(event) => event,
                None => break,
            },
            () = sleep_until_or_forever(leave_at) => {
                leave_at = None;
                tracing::info!("Playback stopped, leaving voice");
                leave_all(&ctx, &state).await;
                continue;
            }
        };
        tracing::debug!("Soloist event: {event:?}");

        match event {
            Event::LoggedIn(false) => {
                tracing::warn!(
                    "Soloist is not paired yet. Open Spotify on a device on the same network as \
                     Aoede and select \"{}\" from the device list.",
                    state.config.spotify_device_name
                );
            }
            Event::LoggedIn(true) => tracing::info!("Soloist is paired with Spotify"),

            Event::Status(status) => {
                status_playing = status == "playing";
                match status.as_str() {
                    "playing" if active => {
                        leave_at = None;
                        start_streaming(&ctx, &state, now_playing.as_ref()).await;
                    }
                    "paused" if active => set_listening(&ctx, None),
                    "idle" if active => {
                        set_listening(&ctx, None);
                        leave_at.get_or_insert(Instant::now() + LEAVE_DELAY);
                    }
                    _ => {}
                }
            }

            Event::Active(is_active) => {
                let was_active = active;
                active = is_active;
                state.active.store(is_active, Ordering::Relaxed);
                if is_active && !was_active && status_playing {
                    leave_at = None;
                    start_streaming(&ctx, &state, now_playing.as_ref()).await;
                } else if !is_active && was_active {
                    // Playback moved to another device.
                    set_listening(&ctx, None);
                    leave_at.get_or_insert(Instant::now() + LEAVE_DELAY);
                }
            }

            Event::Track(track) => {
                now_playing = Some(track);
                if active && status_playing {
                    set_listening(&ctx, now_playing.as_ref());
                }
            }
        }
    }

    tracing::error!("Soloist event channel closed.");
}

async fn sleep_until_or_forever(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        println!("Ready!");
        println!("Invite me with https://discord.com/api/oauth2/authorize?client_id={}&permissions=36700160&scope=bot", ready.user.id);
    }

    async fn cache_ready(&self, ctx: Context, _guilds: Vec<GuildId>) {
        let state = state(&ctx).await;

        if let Some(events) = self.events.lock().await.take() {
            tokio::spawn(handle_soloist_events(ctx.clone(), events));
        }

        // Appear offline until the followed user is in voice.
        if find_user_channel(&ctx, user_id(&state)).is_none() {
            ctx.invisible();
        }
    }

    async fn voice_state_update(&self, ctx: Context, old: Option<VoiceState>, new: VoiceState) {
        let state = state(&ctx).await;

        if new.user_id == ctx.cache.current_user().id {
            tracing::info!(
                "Bot voice state: channel {:?}",
                new.channel_id.map(|c| c.get())
            );
            return;
        }
        if new.user_id != user_id(&state) {
            return;
        }

        let old_channel = old.and_then(|state| state.channel_id);

        match (old_channel, new.channel_id) {
            // User joined a voice channel
            (None, Some(_)) => ctx.online(),

            // User left voice: stop playing
            (Some(_), None) => {
                ctx.invisible();
                // Only stop playback that is actually on Aoede.
                if state.active.load(Ordering::Relaxed) {
                    state.soloist.send("pause");
                }
                leave_all(&ctx, &state).await;
            }

            // User moved channels: follow them if we're currently streaming
            (Some(old_channel), Some(new_channel)) if old_channel != new_channel => {
                let manager = songbird::get(&ctx)
                    .await
                    .expect("Songbird Voice client placed in at initialization.");
                let in_call = ctx
                    .cache
                    .guilds()
                    .into_iter()
                    .any(|guild_id| manager.get(guild_id).is_some());

                if let (true, Some(guild_id)) = (in_call, new.guild_id) {
                    join_and_play(&ctx, &state, guild_id, new_channel).await;
                }
            }

            // Mute/deafen changes etc.
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,serenity=warn,songbird=warn".into()),
        )
        .init();

    let config = match Config::new() {
        Ok(config) => config,
        Err(error) => {
            println!("Couldn't read config");
            if let MissingField(f) = error.kind {
                println!("Missing field: '{}'", f.to_uppercase());
                exit(1)
            }
            println!("Error: {error:?}");
            exit(2)
        }
    };

    let cache_dir =
        PathBuf::from(env::var("CACHE_DIR").unwrap_or_else(|_| DEFAULT_CACHE_DIR.into()));
    let data_dir = cache_dir.join("soloist");
    let soloist_cache_dir = cache_dir.join("soloist-cache");
    for dir in [&data_dir, &soloist_cache_dir] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            println!("Couldn't create '{}': {e}", dir.display());
            exit(2);
        }
    }

    if !audio::ensure_sink(&config.pulse_sink_name) {
        println!(
            "Couldn't find or create the PulseAudio sink '{}'. Is PulseAudio (or PipeWire with \
             pipewire-pulse) running, and are `pactl` and `parec` installed?",
            config.pulse_sink_name
        );
        exit(2);
    }
    let capture = Capture::start(config.pulse_sink_name.clone());

    let (soloist, events) = soloist::start(SoloistConfig {
        path: config.soloist_path.clone(),
        api_key: config.soloist_api_key.clone(),
        device_name: config.spotify_device_name.clone(),
        data_dir,
        cache_dir: soloist_cache_dir,
        initial_volume: config.soloist_initial_volume,
        pulse_sink: config.pulse_sink_name.clone(),
    });

    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;
    let token = config.discord_token.clone();

    let mut data = TypeMap::new();
    data.insert::<StateKey>(Arc::new(State {
        config,
        soloist,
        capture,
        voice: Mutex::new(()),
        track: Mutex::new(None),
        active: AtomicBool::new(false),
    }));

    let mut client = Client::builder(&token, intents)
        .event_handler(Handler {
            events: Mutex::new(Some(events)),
        })
        .type_map(data)
        .register_songbird()
        .await
        .expect("Err creating client");

    if let Err(why) = client.start().await {
        println!("Client ended: {why:?}");
        exit(1);
    }
}
