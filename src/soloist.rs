//! Runs Spotify Soloist as a child process and talks to its local WebSocket API.

use std::path::PathBuf;
use std::process::{exit, Stdio};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Soloist exits with this code when its build has expired.
const EXIT_EXPIRED: i32 = 10;

#[derive(Clone)]
pub struct SoloistConfig {
    pub path: String,
    pub api_key: String,
    pub device_name: String,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub initial_volume: Option<u8>,
    pub pulse_sink: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Whether Soloist has a Spotify session (it needs pairing once if not).
    LoggedIn(bool),
    /// Whether this is the active Spotify Connect device.
    Active(bool),
    /// Playback status: "idle", "playing", "paused" or "buffering".
    Status(String),
    /// "Artist: Title" of the current item.
    Track(String),
}

/// Handle for sending commands (e.g. "pause") to Soloist.
#[derive(Clone)]
pub struct Soloist {
    commands: mpsc::UnboundedSender<Value>,
}

impl Soloist {
    pub fn send(&self, command: &str) {
        let _ = self
            .commands
            .send(json!({ "type": "command", "command": command }));
    }
}

pub fn start(config: SoloistConfig) -> (Soloist, mpsc::UnboundedReceiver<Event>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();

    tokio::spawn(supervise(config.clone()));
    tokio::spawn(websocket(config.data_dir.join("ws.port"), cmd_rx, event_tx));

    (Soloist { commands: cmd_tx }, event_rx)
}

/// Keep Soloist running, restarting it if it exits.
async fn supervise(config: SoloistConfig) {
    let port_file = config.data_dir.join("ws.port");
    loop {
        // A stale port file from a crashed run would point at a dead port.
        let _ = tokio::fs::remove_file(&port_file).await;

        let mut cmd = Command::new(&config.path);
        cmd.arg("--device-name")
            .arg(&config.device_name)
            .arg("--data-dir")
            .arg(&config.data_dir)
            .arg("--cache-dir")
            .arg(&config.cache_dir)
            .args(["--ws", "127.0.0.1:0"])
            .args(["--pipewire-device", &config.pulse_sink])
            // Route audio into our sink without touching the system default.
            .env("PULSE_SINK", &config.pulse_sink)
            .arg("--api-key")
            .arg(&config.api_key)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if let Some(volume) = config.initial_volume {
            cmd.args(["--initial-volume", &volume.to_string()]);
        }

        let status = match cmd.spawn() {
            Ok(mut child) => child.wait().await,
            Err(e) => {
                tracing::error!("Could not start Soloist at '{}': {e}", config.path);
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
        };

        match status.map(|s| s.code()) {
            Ok(Some(EXIT_EXPIRED)) => {
                // Exiting lets the container restart and download a fresh build.
                tracing::error!(
                    "This Soloist build has expired. Restart Aoede to download a new one."
                );
                exit(EXIT_EXPIRED);
            }
            Ok(code) => tracing::warn!("Soloist exited ({code:?}), restarting in 5s"),
            Err(e) => tracing::warn!("Soloist failed ({e}), restarting in 5s"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Connect to Soloist's WebSocket API, forwarding commands and translating events.
async fn websocket(
    port_file: PathBuf,
    mut commands: mpsc::UnboundedReceiver<Value>,
    events: mpsc::UnboundedSender<Event>,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        let Ok(port) = tokio::fs::read_to_string(&port_file).await else {
            continue;
        };
        let url = format!("ws://127.0.0.1:{}", port.trim());
        let (stream, _) = match tokio_tungstenite::connect_async(&url).await {
            Ok(conn) => conn,
            Err(e) => {
                tracing::debug!("Soloist WebSocket not ready: {e}");
                continue;
            }
        };
        tracing::info!("Connected to Soloist");
        let (mut write, mut read) = stream.split();

        loop {
            tokio::select! {
                Some(command) = commands.recv() => {
                    if write.send(Message::text(command.to_string())).await.is_err() {
                        break;
                    }
                }
                message = read.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<Value>(&text) {
                            Ok(value) => {
                                for event in parse_event(&value) {
                                    let _ = events.send(event);
                                }
                            }
                            Err(e) => tracing::warn!("Bad message from Soloist: {e}"),
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                },
            }
        }
        tracing::warn!("Lost connection to Soloist, reconnecting");
    }
}

fn parse_event(value: &Value) -> Vec<Event> {
    let mut out = Vec::new();
    let active = || value["is_active"].as_bool().map(Event::Active);
    match value["type"].as_str().unwrap_or_default() {
        "auth_state" => {
            if let Some(logged_in) = value["logged_in"].as_bool() {
                out.push(Event::LoggedIn(logged_in));
            }
            out.extend(active());
        }
        "playback_state" => {
            out.extend(active());
            if let Some(track) = describe_item(&value["item"]) {
                out.push(Event::Track(track));
            }
            if let Some(status) = value["status"].as_str() {
                out.push(Event::Status(status.to_string()));
            }
        }
        "playback_changed" => {
            if let Some(status) = value["status"].as_str() {
                out.push(Event::Status(status.to_string()));
            }
        }
        "track_changed" => {
            if let Some(track) = describe_item(&value["item"]) {
                out.push(Event::Track(track));
            }
        }
        "device_changed" => out.extend(active()),
        "error" => tracing::warn!("Soloist error: {}", value["message"]),
        _ => {}
    }
    out
}

/// "Artist: Title" from a Soloist entity.
fn describe_item(item: &Value) -> Option<String> {
    let name = item["decorations"]["identity"]["name"].as_str()?;
    let artist = item["decorations"]["creators"]
        .as_array()
        .and_then(|creators| creators.first())
        .and_then(|c| c["entity"]["decorations"]["identity"]["name"].as_str());
    Some(match artist {
        Some(artist) => format!("{artist}: {name}"),
        None => name.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_playback_state() {
        let value = json!({
            "type": "playback_state",
            "status": "playing",
            "is_active": true,
            "item": {
                "uri": "spotify:track:x",
                "decorations": {
                    "identity": { "name": "Song" },
                    "creators": [
                        { "entity": { "decorations": { "identity": { "name": "Band" } } } },
                        { "entity": { "decorations": { "identity": { "name": "Other" } } } }
                    ]
                }
            }
        });
        assert_eq!(
            parse_event(&value),
            vec![
                Event::Active(true),
                Event::Track("Band: Song".into()),
                Event::Status("playing".into()),
            ]
        );
    }

    #[test]
    fn parses_granular_events() {
        assert_eq!(
            parse_event(&json!({"type": "auth_state", "logged_in": false, "is_active": false})),
            vec![Event::LoggedIn(false), Event::Active(false)]
        );
        assert_eq!(
            parse_event(&json!({"type": "playback_changed", "status": "paused"})),
            vec![Event::Status("paused".into())]
        );
        assert_eq!(
            parse_event(&json!({"type": "device_changed", "is_active": false})),
            vec![Event::Active(false)]
        );
        assert_eq!(
            parse_event(
                &json!({"type": "track_changed", "item": {"decorations": {"identity": {"name": "Ep"}}}})
            ),
            vec![Event::Track("Ep".into())]
        );
        assert!(parse_event(&json!({"type": "position_sync"})).is_empty());
    }

    /// Runs the real Soloist binary. Set SOLOIST_TEST_PATH to enable:
    /// `SOLOIST_TEST_PATH=/path/to/soloist cargo test -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn connects_to_real_soloist() {
        let Ok(path) = std::env::var("SOLOIST_TEST_PATH") else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("aoede-test-{}", std::process::id()));
        let (_soloist, mut events) = start(SoloistConfig {
            path,
            api_key: "test-key".into(),
            device_name: "Aoede Test".into(),
            data_dir: dir.join("data"),
            cache_dir: dir.join("cache"),
            initial_volume: None,
            pulse_sink: "aoede".into(),
        });
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("no event from Soloist within 20s");
        assert_eq!(event, Some(Event::LoggedIn(false)));
        let _ = std::fs::remove_dir_all(dir);
    }
}
