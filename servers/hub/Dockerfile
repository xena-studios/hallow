# The hub server image, made from templates/server-repo. The dev network
# (deploy/compose) and docker-bake.hcl build it on the in-tree base image by
# naming the base below as a build context; on its own it pulls the
# published base. Build the workspace's WASM plugins into plugins/ first
# (servers/build-plugins.sh), then build from this folder:
#   docker build -t hallow-hub:dev .
FROM ghcr.io/xena-studios/hallow-server-base:0.2

# The repo version (git tag) this image was built from, reported to the
# control plane. CI sets it; local builds are "dev".
ARG HALLOW_SERVER_VERSION=dev
ENV HALLOW_SERVER_VERSION=${HALLOW_SERVER_VERSION}

COPY --chown=hallow:hallow hallow.toml /server/hallow.toml
# The plugins hallow.toml lists, built before the image (they're not in git).
COPY --chown=hallow:hallow plugins/ /server/plugins/

RUN hallow-manifest check /server --base-version "$(cat /etc/hallow/base-version)"
