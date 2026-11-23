# Hallow's images. `docker buildx bake` builds the proxy, the server base
# image and the in-tree servers (servers/hub, servers/test-game,
# servers/test-game-lobby, servers/banned) from the repo root; the dev network in
# deploy/compose uses the same Dockerfiles and tags. Build the servers'
# plugins first: servers/build-plugins.sh.
# `docker buildx bake control-plane` builds the control plane, which the
# two-proxy override (deploy/compose/compose.hallow.yaml) uses.

group "default" {
  targets = ["proxy", "server", "hub", "test-game", "test-game-lobby", "banned"]
}

target "proxy" {
  context    = "."
  dockerfile = "templates/proxy/Dockerfile"
  tags       = ["hallow-proxy:dev"]
}

# The server base image, published as ghcr.io/xena-studios/hallow-server-base.
target "server" {
  context    = "."
  dockerfile = "templates/server/Dockerfile"
  tags       = ["hallow-server-base:dev"]
}

# Server repos build FROM the published base; here the base target stands in
# for it under the same name.
target "_server-repo" {
  contexts = {
    "ghcr.io/xena-studios/hallow-server-base:0.2" = "target:server"
  }
}

target "hub" {
  inherits = ["_server-repo"]
  context  = "servers/hub"
  tags     = ["hallow-hub:dev"]
}

target "test-game" {
  inherits = ["_server-repo"]
  context  = "servers/test-game"
  tags     = ["hallow-test-game:dev"]
}

target "test-game-lobby" {
  inherits = ["_server-repo"]
  context  = "servers/test-game-lobby"
  tags     = ["hallow-test-game-lobby:dev"]
}

target "banned" {
  inherits = ["_server-repo"]
  context  = "servers/banned"
  tags     = ["hallow-banned:dev"]
}

target "control-plane" {
  context    = "."
  dockerfile = "deploy/control-plane/Dockerfile"
  tags       = ["hallow-control-plane:dev"]
}
