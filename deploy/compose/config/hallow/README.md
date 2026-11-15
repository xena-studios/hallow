# Two-proxy dev config (dev only)

Used by `../../compose.hallow.yaml`. Everything here is a **dev-only default**
checked into git; never reuse it on a network reachable from outside.

- `control-plane.token`: the bearer token for the control plane, the
  `hallow_vine` plugin and the registrar.
- `forwarding.secret`: Vine's forwarding secret, the same default as
  `HALLOW_VINE_SECRET` in `compose.yaml`. If you set `HALLOW_VINE_SECRET`,
  change this file too.
- `vine.toml`: both proxies' Vine config, with no static servers.
- `proxy-1.toml`, `proxy-2.toml`: each proxy's `hallow_vine` config.
- `registrar.sh`: registers the Pumpkin servers with the control plane until
  the server plugin does it.
