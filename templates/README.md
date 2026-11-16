# templates

- `server/`: the server base image every hub, lobby and game builds on, published as `ghcr.io/xena-studios/hallow-server-base:<version>` (version in `server/VERSION`). It has:
  - Pumpkin;
  - the multiversion plugin, off by default;
  - Hallow's server plugin;
  - `hallow-manifest`;
  - Hallow's default `pumpkin.toml`: telemetry and update checks off, Vine forwarding on.

  Its entrypoint applies the server repo's `/server/hallow.toml` and starts Pumpkin. Build it with `docker buildx bake server`.
- `server-repo/`: the contents of the `hallow-server-template` repo, the starting point for a new server kind: `hallow.toml`, a Dockerfile `FROM` the base image, and a workflow that calls the reusable [image workflow](../.github/workflows/server-image.yml).
- `proxy/`: the Vine proxy image.

Lobbies and games themselves live in their own server repos; see [Servers](../docs/SERVERS.md). [`servers/`](../servers) has the in-tree examples the dev network runs.
