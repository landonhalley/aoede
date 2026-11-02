#!/bin/sh
set -eu

DATA_DIR="${CACHE_DIR:-/data}"
BIN_DIR="$DATA_DIR/soloist-bin"
mkdir -p "$BIN_DIR"

# Download the current Soloist build. Builds expire after 90 days, so fetch a
# fresh one on every start and fall back to the last one if the download fails.
case "$(uname -m)" in
    x86_64) ARCH=x86_64 ;;
    aarch64 | arm64) ARCH=arm64 ;;
    armv7l) ARCH=arm32 ;;
    *) echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

TMP_DIR="$(mktemp -d)"
if curl --fail --silent --show-error --location \
        "https://soloist-builds.spotifycdn.com/soloist_release_${ARCH}.tar.gz" \
        -o "$TMP_DIR/soloist.tar.gz" \
    && tar -xzf "$TMP_DIR/soloist.tar.gz" -C "$TMP_DIR" soloist; then
    install -m 755 "$TMP_DIR/soloist" "$BIN_DIR/soloist"
else
    echo "Couldn't download Soloist, using the previously downloaded build." >&2
fi
rm -rf "$TMP_DIR"

if [ ! -x "$BIN_DIR/soloist" ]; then
    echo "Soloist is not available." >&2
    exit 1
fi
export SOLOIST_PATH="$BIN_DIR/soloist"
"$SOLOIST_PATH" --version

# Headless PulseAudio with a null sink that Soloist plays into and Aoede records from.
export XDG_RUNTIME_DIR=/tmp/aoede-runtime
mkdir -p "$XDG_RUNTIME_DIR"
SINK="${PULSE_SINK_NAME:-aoede}"
pulseaudio --daemonize=yes --exit-idle-time=-1 --disallow-exit --use-pid-file=no \
    --log-target=stderr -n \
    -L "module-null-sink sink_name=$SINK rate=48000 channels=2 sink_properties=device.description=Aoede" \
    -L "module-native-protocol-unix auth-anonymous=1" \
    -L "module-always-sink"

exec /usr/local/bin/aoede "$@"
