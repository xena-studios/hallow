# Hallow's images. `docker buildx bake` builds both from the repo root; the dev
# network in deploy/compose uses the same Dockerfiles and tags.

group "default" {
  targets = ["proxy", "server"]
}

target "proxy" {
  context    = "."
  dockerfile = "templates/proxy/Dockerfile"
  tags       = ["hallow-proxy:dev"]
}

target "server" {
  context    = "."
  dockerfile = "templates/server/Dockerfile"
  tags       = ["hallow-server:dev"]
}
