# The skywars server images: solo (hallow.toml, the default) and teams
# (MANIFEST=hallow-teams.toml), from the same plugin and maps. The dev
# network (deploy/compose) and docker-bake.hcl build them on the in-tree
# base image by naming the base below as a build context; on its own it
# pulls the published base. Build the workspace's WASM plugins into
# plugins/ first (servers/build-plugins.sh), then build from this folder:
#   docker build -t hallow-skywars:dev .
#   docker build --build-arg MANIFEST=hallow-teams.toml -t hallow-skywars-teams:dev .
FROM ghcr.io/xena-studios/hallow-server-base:0.2

# The repo version (git tag) this image was built from, reported to the
# control plane. CI sets it; local builds are "dev".
ARG HALLOW_SERVER_VERSION=dev
ENV HALLOW_SERVER_VERSION=${HALLOW_SERVER_VERSION}
# Which kind this image is.
ARG MANIFEST=hallow.toml

COPY --chown=hallow:hallow ${MANIFEST} /server/hallow.toml
# The plugins hallow.toml lists, built before the image (they're not in git).
COPY --chown=hallow:hallow plugins/ /server/plugins/
# The maps: where hallow.toml's [maps] names them, and in the plugin's data
# folder, where the plugin reads them.
COPY --chown=hallow:hallow maps/ /server/maps/
COPY --chown=hallow:hallow maps/ /server/plugins/data/hallow_skywars/maps/

RUN hallow-manifest check /server --base-version "$(cat /etc/hallow/base-version)"
