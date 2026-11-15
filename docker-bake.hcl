# Hallow's images. `docker buildx bake` builds the proxy and server from the
# repo root; the dev network in deploy/compose uses the same Dockerfiles and
# tags. `docker buildx bake control-plane` builds the control plane, which the
# two-proxy override (deploy/compose/compose.hallow.yaml) uses.

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

target "control-plane" {
  context    = "."
  dockerfile = "deploy/control-plane/Dockerfile"
  tags       = ["hallow-control-plane:dev"]
}
