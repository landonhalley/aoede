mod config;
mod player;

use std::env;
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use config::Config;
use figment::error::Kind::MissingField;
use librespot::metadata::audio::UniqueFields;
use librespot::playback::config::Bitrate;
use librespot::playback::player::PlayerEvent;
use player::{SpotifyPlayer, SpotifyPlayerKey};
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
    prelude::TypeMapKey,
    Client,
};
use songbird::SerenityInit;
use tokio::sync::Mutex;

const DEFAULT_CACHE_DIR: &str = "cache";

struct Handler {
    events_started: AtomicBool,
}

pub struct ConfigKey;
impl TypeMapKey for ConfigKey {
    type Value = Config;
}

async fn get_state(ctx: &Context) -> (Arc<Mutex<SpotifyPlayer>>, Config) {
    let data = ctx.data.read().await;
    (
        data.get::<SpotifyPlayerKey>().unwrap().clone(),
        data.get::<ConfigKey>().unwrap().clone(),
    )
}

/// Find the voice channel the followed user is currently in.
fn find_user_channel(ctx: &Context, user_id: UserId) -> Option<(GuildId, ChannelId)> {
    ctx.cache.guilds().into_iter().find_map(|guild_id| {
        let guild = ctx.cache.guild(guild_id)?;
        let channel_id = guild.voice_states.get(&user_id)?.channel_id?;
        Some((guild_id, channel_id))
    })
}

async fn enable_connect(ctx: &Context, player: &Mutex<SpotifyPlayer>) {
    ctx.online();
    if let Err(e) = player.lock().await.enable_connect().await {
        tracing::error!("Could not start Spotify Connect: {e}");
    }
}

/// Join the given channel (if not already there) and stream Spotify into it.
async fn join_and_play(
    ctx: &Context,
    player: &Mutex<SpotifyPlayer>,
    guild_id: GuildId,
    channel_id: ChannelId,
) {
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.");

    // Only one stream exists, so leave any other guild first.
    for other in ctx.cache.guilds() {
        if other != guild_id && manager.get(other).is_some() {
            let _ = manager.remove(other).await;
        }
    }

    // Calls are removed whenever playback stops, so an existing call in this
    // channel is already streaming.
    if let Some(call) = manager.get(guild_id) {
        if call.lock().await.current_channel() == Some(channel_id.into()) {
            return;
        }
    }

    let call = match manager.join(guild_id, channel_id).await {
        Ok(call) => call,
        Err(e) => {
            tracing::error!("Could not join voice channel: {e}");
            return;
        }
    };

    let input = player.lock().await.input();
    let mut call = call.lock().await;
    call.set_bitrate(songbird::driver::Bitrate::Auto);
    call.play_only_input(input);
}

async fn leave_all(ctx: &Context) {
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.");
    for guild_id in ctx.cache.guilds() {
        if manager.get(guild_id).is_some() {
            let _ = manager.remove(guild_id).await;
        }
    }
}

fn track_description(item: &librespot::metadata::audio::AudioItem) -> String {
    let artist = match &item.unique_fields {
        UniqueFields::Track { artists, .. } => artists.first().map(|a| a.name.clone()),
        UniqueFields::Local { artists, .. } => artists.clone(),
        UniqueFields::Episode { show_name, .. } => Some(show_name.clone()),
    };
    match artist {
        Some(artist) => format!("{}: {}", artist, item.name),
        None => item.name.clone(),
    }
}

async fn handle_player_events(ctx: Context) {
    let (player, config) = get_state(&ctx).await;
    let user_id = UserId::new(config.discord_user_id);
    let mut events = player.lock().await.event_channel();

    let mut now_playing: Option<String> = None;
    let mut is_playing = false;

    while let Some(event) = events.recv().await {
        match event {
            PlayerEvent::Playing { .. } => {
                is_playing = true;
                match find_user_channel(&ctx, user_id) {
                    Some((guild_id, channel_id)) => {
                        join_and_play(&ctx, &player, guild_id, channel_id).await
                    }
                    None => tracing::warn!("Could not find user in a voice channel."),
                }
                ctx.set_presence(
                    now_playing.clone().map(ActivityData::listening),
                    OnlineStatus::Online,
                );
            }

            PlayerEvent::TrackChanged { audio_item } => {
                now_playing = Some(track_description(&audio_item));
                if is_playing {
                    ctx.set_presence(
                        now_playing.clone().map(ActivityData::listening),
                        OnlineStatus::Online,
                    );
                }
            }

            PlayerEvent::Paused { .. } => {
                is_playing = false;
                ctx.set_presence(None, OnlineStatus::Online);
            }

            PlayerEvent::Stopped { .. } => {
                is_playing = false;
                ctx.set_presence(None, OnlineStatus::Online);
                leave_all(&ctx).await;
            }

            _ => {}
        }
    }

    tracing::error!("Spotify player event channel closed.");
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        println!("Ready!");
        println!("Invite me with https://discord.com/api/oauth2/authorize?client_id={}&permissions=36700160&scope=bot", ready.user.id);
    }

    async fn cache_ready(&self, ctx: Context, _guilds: Vec<GuildId>) {
        let (player, config) = get_state(&ctx).await;

        if !self.events_started.swap(true, Ordering::SeqCst) {
            tokio::spawn(handle_player_events(ctx.clone()));
        }

        // Handle case when user is in VC when bot starts
        if find_user_channel(&ctx, UserId::new(config.discord_user_id)).is_some() {
            enable_connect(&ctx, &player).await;
        } else {
            ctx.invisible();
        }
    }

    async fn voice_state_update(&self, ctx: Context, old: Option<VoiceState>, new: VoiceState) {
        let (player, config) = get_state(&ctx).await;

        if new.user_id != UserId::new(config.discord_user_id) {
            return;
        }

        let old_channel = old.and_then(|state| state.channel_id);

        match (old_channel, new.channel_id) {
            // User joined a voice channel
            (None, Some(_)) => enable_connect(&ctx, &player).await,

            // User left voice
            (Some(_), None) => {
                ctx.invisible();
                player.lock().await.disable_connect();
                leave_all(&ctx).await;
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
                    join_and_play(&ctx, &player, guild_id, new_channel).await;
                }
            }

            // Mute/deafen changes etc.
            _ => {}
        }
    }
}

/// Interactive Spotify OAuth login. Returns whether it succeeded.
async fn run_login(cache: librespot::core::cache::Cache, cache_dir: &str) -> bool {
    println!("Open the URL below and log in to Spotify. Your browser will then fail to load");
    println!(
        "a page on 127.0.0.1; copy that page's full URL from the address bar and paste it here.\n"
    );
    match player::login(cache).await {
        Ok(username) => {
            println!("\nLogged in as '{username}'. Credentials saved to '{cache_dir}'.");
            true
        }
        Err(e) => {
            println!("\nLogin failed: {e}");
            println!("If nothing could be pasted, make sure the container has stdin_open: true and tty: true.");
            false
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

    let cache_dir = env::var("CACHE_DIR").unwrap_or_else(|_| DEFAULT_CACHE_DIR.to_string());
    let cache = match player::open_cache(&cache_dir) {
        Ok(cache) => cache,
        Err(e) => {
            println!("Couldn't open cache directory '{cache_dir}': {e}");
            exit(2);
        }
    };

    if env::args().nth(1).as_deref() == Some("login") {
        let ok = run_login(cache, &cache_dir).await;
        exit(if ok { 0 } else { 1 });
    }

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

    if cache.credentials().is_none() {
        println!("No Spotify credentials found in '{cache_dir}', starting login.");
        println!("Paste the URL into this container's console: in Portainer use Containers > aoede > Attach,");
        println!("with Docker use `docker attach <container>` (the container needs stdin_open and tty).\n");
        if !run_login(cache.clone(), &cache_dir).await {
            exit(1);
        }
    }

    let player = match SpotifyPlayer::new(
        cache,
        Bitrate::Bitrate320,
        config.spotify_bot_autoplay,
        config.spotify_device_name.clone(),
    ) {
        Ok(player) => player,
        Err(e) => {
            println!("Couldn't create Spotify player: {e}");
            exit(2);
        }
    };

    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;

    let mut client = Client::builder(&config.discord_token, intents)
        .event_handler(Handler {
            events_started: AtomicBool::new(false),
        })
        .type_map_insert::<SpotifyPlayerKey>(Arc::new(Mutex::new(player)))
        .type_map_insert::<ConfigKey>(config)
        .register_songbird()
        .await
        .expect("Err creating client");

    if let Err(why) = client.start().await {
        println!("Client ended: {why:?}");
    }
}
