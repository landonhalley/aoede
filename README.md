<p align="center">
  <img width="250" height="250" src="https://raw.githubusercontent.com/codetheweb/aoede/main/.github/logo.png">
</p>

Aoede is a Discord music bot that **directly** streams from **Spotify to Discord**. The only interface is Spotify itself.

Aoede plays through [Spotify Soloist](https://developer.spotify.com/documentation/soloist), Spotify's official headless Spotify Connect player. Earlier versions used librespot, which Spotify has been [locking out](https://github.com/librespot-org/librespot/issues/1649).

**Note**: setting up Soloist requires a Spotify Premium account and a free Spotify for Developers account.

![Demo](https://raw.githubusercontent.com/codetheweb/aoede/main/.github/demo.gif)

## 💼 Usecases

- Small servers with friends
- Discord Stages, broadcast music to your audience

## 🏗 Usage

### Notes:
⚠️ Aoede only supports bot tokens. Providing a user token won't work.

Aoede will appear offline until you join a voice channel it has access to. It plays while you're in voice and pauses Spotify when you leave.

### 1. Get a Soloist API key

1. Sign in to the [Spotify for Developers dashboard](https://developer.spotify.com/dashboard) with your Premium account.
2. Open the [Spotify Soloist API Key](https://developer.spotify.com/dashboard/soloist) page and generate a key.
3. Keep it secret: it belongs to your account. Aoede needs it as `SOLOIST_API_KEY`.

### 2. Pair with Spotify (first start only)

On its first start Aoede waits to be paired. Open Spotify on a phone or computer **on the same network** as the machine running Aoede, open the device picker and select **Aoede** (or your `SPOTIFY_DEVICE_NAME`). Soloist saves the session in the data volume, so this only happens once. After that, select Aoede in Spotify whenever you're in a voice channel.

Pairing uses local network discovery, which is why the Docker examples use `network_mode: host`.

### Portainer

A multi-arch image (amd64 and arm64) is published to `ghcr.io/landonhalley/aoede:latest`.

1. In Portainer go to Stacks > Add stack > Web editor and paste in [`portainer-stack.yml`](portainer-stack.yml).
2. Fill in `DISCORD_TOKEN`, `DISCORD_USER_ID` and `SOLOIST_API_KEY`, then deploy.
3. Pair it from the Spotify app (see above). The container logs say when it's waiting.

To update, use "Pull and redeploy". Restarting the container also downloads the latest Soloist build.

### Docker Compose

1. Clone this repository and create a `.env` file next to `docker-compose.yml`:
	```env
	DISCORD_TOKEN=
	# Discord user ID of the user you want Aoede to follow
	DISCORD_USER_ID=
	# Spotify Soloist API key
	SOLOIST_API_KEY=
	# Optional: name shown in Spotify Connect. Defaults to "Aoede"
	SPOTIFY_DEVICE_NAME=
	```
2. Start it: `docker compose up -d --build`
3. Pair it from the Spotify app (see above).

Pairing, cache and the downloaded Soloist build are stored in `./aoede`.

### Docker

```bash
docker build -t aoede .
docker run -d --restart unless-stopped --network host --env-file .env -v ./aoede:/data aoede
```

### Configuration

| Variable | Required | Description |
| --- | --- | --- |
| `DISCORD_TOKEN` | yes | Discord bot token |
| `DISCORD_USER_ID` | yes | Discord user ID of the user Aoede follows |
| `SOLOIST_API_KEY` | yes | Spotify Soloist API key |
| `SPOTIFY_DEVICE_NAME` | no | Spotify Connect device name (default `Aoede`) |
| `SOLOIST_INITIAL_VOLUME` | no | Initial Spotify volume, 0-100 |
| `CACHE_DIR` | no | Data directory (`/data` in Docker, `./cache` otherwise) |
| `SOLOIST_PATH` | no | Path to the `soloist` executable (set automatically in Docker) |
| `PULSE_SINK_NAME` | no | PulseAudio sink used to capture audio (default `aoede`) |

### Running without Docker

Linux only. You need:

- Soloist from the [getting started guide](https://developer.spotify.com/documentation/soloist/tutorials/getting-started) on your `PATH` (or set `SOLOIST_PATH`). Builds expire after 90 days, so download a new one when it tells you to.
- PulseAudio, or PipeWire with `pipewire-pulse`, plus `pactl` and `parec` (`pulseaudio-utils`).

Aoede creates an `aoede` null sink and routes only Soloist into it, so your normal audio output isn't affected. Configure it with `config.toml` (see `config.sample.toml`) or environment variables, then run `cargo run --release`.

### Building from source

Requirements: Rust (latest stable), Cargo, cmake and a C compiler. Run `cargo build --release`.
