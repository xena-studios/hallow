# Hallow server base image, ghcr.io/xena-studios/hallow-server-base:<version>:
# Pumpkin with the multiversion plugin, Hallow's server plugin, Hallow's
# defaults and hallow-manifest. Every hub, lobby and game image builds on
# this: a server repo's Dockerfile is FROM it plus the repo's hallow.toml,
# plugins and world (see templates/server-repo). The version is in VERSION.
# Build from the repo root:
#   docker build -f templates/server/Dockerfile -t hallow-server-base:dev .

FROM rust:1.98-trixie AS build
RUN rustup target add wasm32-wasip2
# The multiversion plugin's build runs Mojang's data generator for older versions (Java 17+).
RUN apt-get update \
    && apt-get install -y --no-install-recommends openjdk-21-jre-headless \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY upstream/Pumpkin upstream/Pumpkin
COPY upstream/pumpkin-java-multiversion upstream/pumpkin-java-multiversion
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/upstream/Pumpkin/target,sharing=locked \
    cd upstream/Pumpkin && cargo build --release --locked --bin pumpkin \
    && cp target/release/pumpkin /pumpkin
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/upstream/pumpkin-java-multiversion/target,sharing=locked \
    cd upstream/pumpkin-java-multiversion && cargo build --release --target wasm32-wasip2 \
    && cp target/wasm32-wasip2/release/pumpkin_java_multiversion.wasm /multiversion.wasm
# Hallow's server plugin and hallow-manifest build in the workspace, which
# names path dependencies in upstream/Pumpkin (copied above). Its target cache
# has its own id: sharing /src/target with other images' builds lets cargo
# reuse artifacts built from other sources when their timestamps are newer.
COPY Cargo.toml Cargo.lock ./
COPY crates crates
# The in-tree servers' plugins are workspace members too.
COPY servers/hub/plugin servers/hub/plugin
COPY servers/test-game/plugin servers/test-game/plugin
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=hallow-server-plugin-target,target=/src/target,sharing=locked \
    cargo build --release --locked --target wasm32-wasip2 -p hallow-server \
    && cp target/wasm32-wasip2/release/hallow_server.wasm /hallow_server.wasm \
    && cargo build --release --locked -p hallow-manifest \
    && cp target/release/hallow-manifest /hallow-manifest

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates gettext-base netcat-openbsd \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -u 2613 -m -d /server hallow
COPY --from=build /pumpkin /usr/local/bin/pumpkin
# Off by default: see HALLOW_MULTIVERSION in entrypoint.sh.
COPY --from=build /multiversion.wasm /opt/hallow/plugins/pumpkin_java_multiversion.wasm
# Always loaded, but idle unless HALLOW_CONTROL_PLANE (or its config.toml) is
# set: see crates/server-plugin.
COPY --from=build /hallow_server.wasm /opt/hallow/plugins/hallow_server.wasm
# Reads and applies /server/hallow.toml on start; server repos also run its
# check in their image build.
COPY --from=build /hallow-manifest /usr/local/bin/hallow-manifest
COPY templates/server/pumpkin.toml /etc/hallow/pumpkin.toml
# Checked against each server repo's base range (hallow.toml's base).
COPY templates/server/VERSION /etc/hallow/base-version
COPY templates/server/entrypoint.sh /usr/local/bin/hallow-entrypoint
# Readable by any user: Wings runs containers as its own user (not hallow)
# with a read-only root, and the entrypoint copies /server to the volume.
RUN install -d -o hallow -g hallow /server/plugins /server/plugins/data \
    && chmod 0755 /server
WORKDIR /server
USER hallow
EXPOSE 25565
ENTRYPOINT ["hallow-entrypoint"]
HEALTHCHECK --interval=10s --timeout=3s --retries=6 CMD nc -z 127.0.0.1 "${HALLOW_SERVER_PORT:-25565}" || exit 1
