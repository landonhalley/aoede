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
    /// Force Spotify's autoplay on or off. When unset, the account's own setting is used.
    #[serde(alias = "SPOTIFY_BOT_AUTOPLAY")]
    #[serde(default, deserialize_with = "optional_bool")]
    pub spotify_bot_autoplay: Option<bool>,
    #[serde(alias = "SPOTIFY_DEVICE_NAME")]
    #[serde(default = "default_spotify_device_name")]
    pub spotify_device_name: String,
}

/// Accepts a boolean, a "true"/"false" string, or an empty string (treated as unset),
/// since Docker Compose passes unset optional variables as empty strings.
fn optional_bool<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BoolOrString {
        Bool(bool),
        String(String),
    }

    match Option::<BoolOrString>::deserialize(deserializer)? {
        None => Ok(None),
        Some(BoolOrString::Bool(b)) => Ok(Some(b)),
        Some(BoolOrString::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "" => Ok(None),
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            other => Err(serde::de::Error::custom(format!(
                "expected true or false, got '{other}'"
            ))),
        },
    }
}

fn default_spotify_device_name() -> String {
    "Aoede".to_string()
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
        Ok(config)
    }
}
