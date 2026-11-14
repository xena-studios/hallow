# Hallow server base image: Pumpkin with the multiversion plugin and Hallow's
# defaults. Every lobby and game image builds on this. Build from the repo root:
#   docker build -f templates/server/Dockerfile -t hallow-server .

FROM rust:1.98-trixie AS build
RUN rustup target add wasm32-wasip2
# The multiversion plugin's build runs Mojang's data generator for older versions (Java 17+).
RUN apt-get update \
    && apt-get install -y --no-install-recommends openjdk-21-jre-headless \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY upstream/Pumpkin upstream/Pumpkin
COPY upstream/pumpkin-java-multiversion upstream/pumpkin-java-multiversion
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/upstream/Pumpkin/target \
    cd upstream/Pumpkin && cargo build --release --locked --bin pumpkin \
    && cp target/release/pumpkin /pumpkin
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/upstream/pumpkin-java-multiversion/target \
    cd upstream/pumpkin-java-multiversion && cargo build --release --target wasm32-wasip2 \
    && cp target/wasm32-wasip2/release/pumpkin_java_multiversion.wasm /multiversion.wasm

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates gettext-base netcat-openbsd \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -u 2613 -m -d /server hallow
COPY --from=build /pumpkin /usr/local/bin/pumpkin
# Off by default: see HALLOW_MULTIVERSION in entrypoint.sh.
COPY --from=build /multiversion.wasm /opt/hallow/plugins/pumpkin_java_multiversion.wasm
COPY templates/server/pumpkin.toml /etc/hallow/pumpkin.toml
COPY templates/server/entrypoint.sh /usr/local/bin/hallow-entrypoint
RUN install -d -o hallow -g hallow /server/plugins
WORKDIR /server
USER hallow
EXPOSE 25565
ENTRYPOINT ["hallow-entrypoint"]
HEALTHCHECK --interval=10s --timeout=3s --retries=6 CMD nc -z 127.0.0.1 25565 || exit 1
