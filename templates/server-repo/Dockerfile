# This server kind's image: Hallow's server base image plus this repo's
# hallow.toml, plugins and world. The base image's entrypoint applies
# hallow.toml and starts Pumpkin. Build from the repo root:
#   docker build -t example:dev .
#
# Keep the base tag inside hallow.toml's base range; the build fails if it
# isn't.
FROM ghcr.io/xena-studios/hallow-server-base:0.2

# The repo version (git tag) this image was built from, reported to the
# control plane. CI sets it; local builds are "dev".
ARG HALLOW_SERVER_VERSION=dev
ENV HALLOW_SERVER_VERSION=${HALLOW_SERVER_VERSION}

COPY --chown=hallow:hallow hallow.toml /server/hallow.toml
# Pumpkin loads every .wasm in /server/plugins. Build the game's plugin
# (plugin/) into plugins/ before building the image (CI does).
COPY --chown=hallow:hallow plugins/ /server/plugins/
# Uncomment with [maps] in hallow.toml. The base image copies them into the
# game plugin's data folder on start, where hallow-sdk reads them.
# COPY --chown=hallow:hallow maps/ /server/maps/
# Uncomment with world = "world" in hallow.toml. Pumpkin writes to it, so it
# belongs to the server's user.
# COPY --chown=hallow:hallow world/ /server/world/

RUN hallow-manifest check /server --base-version "$(cat /etc/hallow/base-version)"
