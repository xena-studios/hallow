# Hallow server base image, ghcr.io/xena-studios/hallow-server-base:<version>:
# Pumpkin, Hallow's server plugin, Hallow's
# defaults and hallow-manifest. Every hub, lobby and game image builds on
# this: a server repo's Dockerfile is FROM it plus the repo's hallow.toml,
# plugins and world (see templates/server-repo). The version is in VERSION.
# Servers only see the current Minecraft version: the proxy translates older
# clients (pumpkin-java-multiversion, linked into Vine), so the image carries
# no multiversion plugin.
# Build from the repo root:
#   docker build -f templates/server/Dockerfile -t hallow-server-base:dev .

FROM rust:1.100-trixie AS build
RUN rustup target add wasm32-wasip2
WORKDIR /src
COPY upstream/Pumpkin upstream/Pumpkin
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/upstream/Pumpkin/target,sharing=locked \
    cd upstream/Pumpkin && cargo build --release --locked --bin pumpkin \
    && cp target/release/pumpkin /pumpkin
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
