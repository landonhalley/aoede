FROM rust:trixie AS dependencies

RUN apt-get update && apt-get install -y --no-install-recommends cmake && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked

FROM dependencies AS planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM dependencies AS cacher
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

FROM dependencies AS builder
WORKDIR /app
COPY . .
COPY --from=cacher /app/target target
COPY --from=cacher /usr/local/cargo /usr/local/cargo
RUN cargo build --release --bin aoede

FROM debian:trixie-slim AS runtime

# Soloist needs glibc and libatomic, plays through PulseAudio, and Aoede
# records it with parec. Soloist itself is downloaded at startup, since
# Spotify doesn't allow redistributing it and builds expire after 90 days.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl libatomic1 libpulse0 pulseaudio pulseaudio-utils \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/aoede /usr/local/bin/aoede
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

ENV CACHE_DIR=/data

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
