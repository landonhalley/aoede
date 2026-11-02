<p align="center">
  <img width="250" height="250" src="https://raw.githubusercontent.com/codetheweb/aoede/main/.github/logo.png">
</p>

Aoede is a Discord music bot that **directly** streams from **Spotify to Discord**. The only interface is Spotify itself.

**Note**: a Spotify Premium account is currently required. This is a limitation of librespot, the Spotify library Aoede uses. Facebook logins [are not supported](https://github.com/librespot-org/librespot/discussions/635).

![Demo](https://raw.githubusercontent.com/codetheweb/aoede/main/.github/demo.gif)

## 💼 Usecases

- Small servers with friends
- Discord Stages, broadcast music to your audience

## 🏗 Usage

### Notes:
⚠️ Aoede only supports bot tokens. Providing a user token won't work.

Aoede will appear offline until you join a voice channel it has access it.

### Logging in to Spotify

Spotify no longer allows logging in with a username and password, so Aoede signs in with OAuth. You only need to do this once; the credentials are saved in the cache directory (`/data` in Docker, `./cache` otherwise, or whatever `CACHE_DIR` is set to).

1. Run the login command:
	- Docker Compose: `docker compose run --rm -it aoede login`
	- Docker: `docker run --rm -it -v ./aoede:/data aoede login` (after `docker build -t aoede .`)
	- Binary: `./aoede login`
2. Open the printed URL and log in to Spotify.
3. Your browser will be redirected to a `http://127.0.0.1/login?code=...` page that fails to load. That's expected: copy the full URL from the address bar and paste it into the terminal.

Then start Aoede normally. If the saved credentials ever stop working, run the login command again.

### Docker Compose (recommended):

1. Clone this repository and create a `.env` file next to `docker-compose.yml`:
	```env
	DISCORD_TOKEN=
	# Discord user ID of the user you want Aoede to follow
	DISCORD_USER_ID=
	# Optional: force autoplay of similar songs on/off (true/false). Defaults to your Spotify setting
	SPOTIFY_BOT_AUTOPLAY=
	# Optional: name shown in Spotify Connect. Defaults to "Aoede"
	SPOTIFY_DEVICE_NAME=
	```
2. Build the image: `docker compose build`
3. Log in to Spotify once: `docker compose run --rm -it aoede login`
4. Start Aoede: `docker compose up -d`

Spotify credentials and the audio cache are stored in `./aoede`.

### Docker:

```bash
docker build -t aoede .
docker run --rm -it -v ./aoede:/data aoede login
docker run -d --restart always --env-file .env -v ./aoede:/data aoede
```

### Prebuilt Binaries:

Prebuilt binaries for older versions are available on the [releases page](https://github.com/codetheweb/aoede/releases), but they use the old password login and no longer work. Build from source instead (see below), then inside a terminal session:

1. There are two options to make configuration values available to Aoede:
	1. Copy the `config.sample.toml` file to `config.toml` and update as necessary.
	2. Use environment variables (see the Docker Compose section above):
		- On Windows, you can use `setx DISCORD_TOKEN my-token`
		- On Linux / macOS, you can use `export DISCORD_TOKEN=my-token`
2. Log in to Spotify once with `aoede login` (see above).
3. Run the binary.

### Building from source:

Requirements:

- cmake
- A C compiler
- Rust (latest stable)
- Cargo

Run `cargo build --release`. This will produce a binary in `target/release/aoede`. Set the required environment variables (see the Docker Compose section), log in to Spotify, then run the binary.
