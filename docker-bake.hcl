# Hallow's images. `docker buildx bake` builds the proxy, the server base
# image and the in-tree servers (servers/hub, servers/test-game,
# servers/test-game-lobby, the launch games and their lobbies) from the repo
# root; the dev network in
# deploy/compose uses the same Dockerfiles and tags. Build the servers'
# plugins first: servers/build-plugins.sh.
# `docker buildx bake control-plane` builds the control plane, which the
# two-proxy override (deploy/compose/compose.hallow.yaml) uses.

group "default" {
  targets = [
    "proxy", "server", "hub", "test-game", "test-game-lobby",
    "eggwars", "eggwars-lobby", "skywars", "skywars-teams", "skywars-lobby",
  ]
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

target "eggwars" {
  inherits = ["_server-repo"]
  context  = "servers/eggwars"
  tags     = ["hallow-eggwars:dev"]
}

target "eggwars-lobby" {
  inherits = ["_server-repo"]
  context  = "servers/eggwars-lobby"
  tags     = ["hallow-eggwars-lobby:dev"]
}

# Solo and teams are two kinds from one folder: the teams image takes
# hallow-teams.toml as its manifest.
target "skywars" {
  inherits = ["_server-repo"]
  context  = "servers/skywars"
  tags     = ["hallow-skywars:dev"]
}

target "skywars-teams" {
  inherits = ["_server-repo"]
  context  = "servers/skywars"
  args     = { MANIFEST = "hallow-teams.toml" }
  tags     = ["hallow-skywars-teams:dev"]
}

target "skywars-lobby" {
  inherits = ["_server-repo"]
  context  = "servers/skywars-lobby"
  tags     = ["hallow-skywars-lobby:dev"]
}

target "control-plane" {
  context    = "."
  dockerfile = "deploy/control-plane/Dockerfile"
  tags       = ["hallow-control-plane:dev"]
}
