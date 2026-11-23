# Hallow player website image. Build from the repo root:
#   docker build -f web/Dockerfile -t hallow-web .
# Settings are environment variables; see web/README.md.

FROM rust:1.100-trixie AS build
WORKDIR /src
# The workspace manifest names path dependencies in upstream/Pumpkin, so cargo
# needs it to resolve the workspace even though the site doesn't use it.
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY web web
COPY upstream/Pumpkin upstream/Pumpkin
# The in-tree servers' plugins are workspace members too.
COPY servers/hub/plugin servers/hub/plugin
COPY servers/test-game/plugin servers/test-game/plugin
# The target cache is shared by every build on this machine, worktrees and
# other checkouts included, and cargo trusts an artifact newer than its
# sources. Touching the .rs files (and the stylesheet the binary embeds)
# makes cargo rebuild the workspace's own crates from what was copied here;
# crates.io dependencies stay cached.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=hallow-web-target,target=/src/target,sharing=locked \
    find crates servers web -name '*.rs' -exec touch {} + \
    && touch web/src/style.css \
    && cargo build --release --locked -p hallow-web --bin hallow-web \
    && cp target/release/hallow-web /hallow-web

FROM debian:trixie-slim
# curl is for the health check.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -u 2614 -m -d /hallow-web hallow
COPY --from=build /hallow-web /usr/local/bin/hallow-web
WORKDIR /hallow-web
USER hallow
EXPOSE 8080
ENTRYPOINT ["hallow-web"]
HEALTHCHECK --interval=10s --timeout=3s --retries=6 CMD curl -fsS -o /dev/null http://127.0.0.1:8080/healthz || exit 1
