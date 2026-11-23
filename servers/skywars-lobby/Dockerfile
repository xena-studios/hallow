# The skywars-lobby server image, made from templates/server-repo. The dev network
# (deploy/compose) and docker-bake.hcl build it on the in-tree base image by
# naming the base below as a build context; on its own it pulls the
# published base. Build from this folder:
#   docker build -t hallow-skywars-lobby:dev .
FROM ghcr.io/xena-studios/hallow-server-base:0.2

# The repo version (git tag) this image was built from, reported to the
# control plane. CI sets it; local builds are "dev".
ARG HALLOW_SERVER_VERSION=dev
ENV HALLOW_SERVER_VERSION=${HALLOW_SERVER_VERSION}

COPY --chown=hallow:hallow hallow.toml /server/hallow.toml
# The lobby plugin (servers/lobby-plugin), built before the image.
COPY --chown=hallow:hallow plugins/ /server/plugins/

RUN hallow-manifest check /server --base-version "$(cat /etc/hallow/base-version)"
