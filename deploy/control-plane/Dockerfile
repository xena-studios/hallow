# Hallow control plane image. Build from the repo root:
#   docker build -f deploy/control-plane/Dockerfile -t hallow-control-plane .
# Settings are environment variables; see crates/control-plane/src/main.rs.

FROM rust:1.98-trixie AS build
WORKDIR /src
# The workspace manifest names path dependencies in upstream/Pumpkin, so cargo
# needs it to resolve the workspace even though the control plane doesn't use it.
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY upstream/Pumpkin upstream/Pumpkin
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p hallow-control-plane --bin control-plane \
    && cp target/release/control-plane /control-plane

FROM debian:trixie-slim
# curl is for the health check (and the dev registrar in compose.hallow.yaml).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -u 2613 -m -d /control-plane hallow
COPY --from=build /control-plane /usr/local/bin/control-plane
WORKDIR /control-plane
USER hallow
EXPOSE 8080
ENTRYPOINT ["control-plane"]
HEALTHCHECK --interval=5s --timeout=3s --retries=6 CMD curl -fsS -o /dev/null http://127.0.0.1:8080/healthz || exit 1
