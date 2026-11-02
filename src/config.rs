use figment::{
    providers::{Env, Format, Toml},
    Error, Figment,
};
use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct Config {
    #[serde(alias = "DISCORD_TOKEN")]
    pub discord_token: String,
    #[serde(alias = "DISCORD_USER_ID")]
    pub discord_user_id: u64,
    /// Force Spotify's autoplay on or off. When unset, the account's own setting is used.
    #[serde(alias = "SPOTIFY_BOT_AUTOPLAY")]
    #[serde(default)]
    pub spotify_bot_autoplay: Option<bool>,
    #[serde(alias = "SPOTIFY_DEVICE_NAME")]
    #[serde(default = "default_spotify_device_name")]
    pub spotify_device_name: String,
}

fn default_spotify_device_name() -> String {
    "Aoede".to_string()
}

#[allow(clippy::result_large_err)]
impl Config {
    pub fn new() -> Result<Self, Error> {
        Figment::new()
            .merge(Toml::file("config.toml"))
            .merge(Env::raw())
            .extract()
    }
}
