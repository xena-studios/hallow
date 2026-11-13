# Hallow proxy image: Vine from our fork. The config is mounted at
# /proxy/vine.toml. Build from the repo root:
#   docker build -f templates/proxy/Dockerfile -t hallow-proxy .

FROM rust:1.98-trixie AS build
WORKDIR /src
COPY upstream/Pumpkin upstream/Pumpkin
COPY upstream/Vine upstream/Vine
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/upstream/Vine/target \
    cd upstream/Vine && cargo build --release --locked \
    && cp target/release/vine /vine

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates netcat-openbsd \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -u 2613 -m -d /proxy hallow
COPY --from=build /vine /usr/local/bin/vine
WORKDIR /proxy
USER hallow
EXPOSE 25565
ENTRYPOINT ["vine"]
HEALTHCHECK --interval=10s --timeout=3s --retries=6 CMD nc -z 127.0.0.1 25565 || exit 1
