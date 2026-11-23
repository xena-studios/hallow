# servers

In-tree server repos, each made from [`templates/server-repo`](../templates/server-repo) and run by the [dev network](../deploy/compose/README.md):

| Folder | Role | What it is |
|---|---|---|
| `hub/` | hub | The hub every player lands in: the game menu, welcome title, tab list and a safe spawn from its plugin (`hub/plugin`), adventure mode, no PvP, no Nether or End. The dev network's `lobby` |
| `test-game-lobby/` | lobby | test-game's lobby, queueing for `test-game` and returning to `hub`. No plugin. Run by the control plane as instances (one always warm) on the dynamic dev network |
| `test-game/` | game | A Red against Blue match on `hallow-sdk` (`test-game/plugin`, one map in `test-game/maps`) when `TEST_GAME_MATCH_SECS` is set: countdown, teams, kits, sidebar, `/forfeit` to spectate, stats, then `draining` so players go back to `test-game-lobby`. The dev network's `game-1` (no matches there), and the dynamic network's game instances |

Real lobbies and games each get their own repo (see [Servers](../docs/SERVERS.md)); these stay here so the network and CI test the server repo path. The hub moves to its own `hub` repo with its plugin later.

## Plugins

A server's own plugin is a crate in its `plugin/` folder, built in this workspace (`hallow-hub`, `hallow-test-game`) to `plugins/<name>.wasm`, which its `hallow.toml` lists under `[[plugins]]` and its Dockerfile copies into the image. The built files aren't in git; build them before the images:

```sh
servers/build-plugins.sh
```

That builds the Vine and server plugins the dev network mounts too. A server repo of its own does the same in its CI's `prepare` step.

Each Dockerfile starts `FROM ghcr.io/xena-studios/hallow-server-base:0.2`. `deploy/compose` and `docker-bake.hcl` replace that name with the base image built from `templates/server` in this tree.
