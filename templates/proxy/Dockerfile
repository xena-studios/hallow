# Hallow proxy image: Vine from our fork, which translates clients older than
# the servers' version with pumpkin-java-multiversion (linked as a library).
# The config is mounted at /proxy/vine.toml. Build from the repo root:
#   docker build -f templates/proxy/Dockerfile -t hallow-proxy .

FROM rust:1.100-trixie AS build
# The translation's build runs Mojang's data generator for older versions (Java 17+).
RUN apt-get update \
    && apt-get install -y --no-install-recommends openjdk-21-jre-headless \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY upstream/Pumpkin upstream/Pumpkin
COPY upstream/pumpkin-java-multiversion upstream/pumpkin-java-multiversion
COPY upstream/Vine upstream/Vine
# The translation's generated datapacks are cached too, so a Vine change doesn't
# download and run Mojang's server jars again.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/upstream/Vine/target,sharing=locked \
    --mount=type=cache,id=multiversion-datapacks,target=/src/upstream/pumpkin-java-multiversion/assets/datapacks,sharing=locked \
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
