# servers

In-tree server repos, each made from [`templates/server-repo`](../templates/server-repo) and run by the [dev network](../deploy/compose/README.md):

| Folder | Role | What it is |
|---|---|---|
| `hub/` | hub | The hub every player lands in: no plugin yet, adventure mode, no PvP, no Nether or End. The dev network's `lobby` |
| `test-game/` | game | A game with no plugin, returning players to `hub`. The dev network's `game-1` |

Real lobbies and games each get their own repo (see [Servers](../docs/SERVERS.md)); these stay here so the network and CI test the server repo path. The hub moves to its own `hub` repo with the hub plugin (issue #57).

Each Dockerfile starts `FROM ghcr.io/xena-studios/hallow-server-base:0.1`. `deploy/compose` and `docker-bake.hcl` replace that name with the base image built from `templates/server` in this tree.
