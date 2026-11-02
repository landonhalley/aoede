use figment::{
    providers::{Env, Format, Toml},
    Error, Figment,
};
use serde::{Deserialize, Deserializer};

#[derive(Deserialize, Clone)]
pub struct Config {
    #[serde(alias = "DISCORD_TOKEN")]
    pub discord_token: String,
    #[serde(alias = "DISCORD_USER_ID")]
    pub discord_user_id: u64,
    /// Spotify Soloist API key from https://developer.spotify.com/dashboard/soloist
    #[serde(alias = "SOLOIST_API_KEY")]
    pub soloist_api_key: String,
    #[serde(alias = "SPOTIFY_DEVICE_NAME")]
    #[serde(default = "default_spotify_device_name")]
    pub spotify_device_name: String,
    /// Path to the Soloist executable.
    #[serde(alias = "SOLOIST_PATH")]
    #[serde(default = "default_soloist_path")]
    pub soloist_path: String,
    /// Initial Spotify volume (0-100).
    #[serde(alias = "SOLOIST_INITIAL_VOLUME")]
    #[serde(default, deserialize_with = "optional_u8")]
    pub soloist_initial_volume: Option<u8>,
    /// Name of the PulseAudio null sink Soloist plays into and Aoede records from.
    #[serde(alias = "PULSE_SINK_NAME")]
    #[serde(default = "default_pulse_sink")]
    pub pulse_sink_name: String,
}

/// Accepts a number, a numeric string, or an empty string (treated as unset),
/// since Docker Compose passes unset optional variables as empty strings.
fn optional_u8<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u8>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrString {
        Num(u8),
        String(String),
    }

    match Option::<NumOrString>::deserialize(deserializer)? {
        None => Ok(None),
        Some(NumOrString::Num(n)) => Ok(Some(n)),
        Some(NumOrString::String(s)) if s.trim().is_empty() => Ok(None),
        Some(NumOrString::String(s)) => s
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| serde::de::Error::custom(format!("expected 0-100, got '{s}'"))),
    }
}

fn default_spotify_device_name() -> String {
    "Aoede".to_string()
}

fn default_soloist_path() -> String {
    "soloist".to_string()
}

fn default_pulse_sink() -> String {
    "aoede".to_string()
}

#[allow(clippy::result_large_err)]
impl Config {
    pub fn new() -> Result<Self, Error> {
        let mut config: Config = Figment::new()
            .merge(Toml::file("config.toml"))
            .merge(Env::raw())
            .extract()?;
        if config.spotify_device_name.trim().is_empty() {
            config.spotify_device_name = default_spotify_device_name();
        }
        if config.soloist_path.trim().is_empty() {
            config.soloist_path = default_soloist_path();
        }
        if config.pulse_sink_name.trim().is_empty() {
            config.pulse_sink_name = default_pulse_sink();
        }
        if let Some(v) = config.soloist_initial_volume {
            config.soloist_initial_volume = Some(v.min(100));
        }
        Ok(config)
    }
}
