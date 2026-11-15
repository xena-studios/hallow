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

## Cross-proxy command test

Two netcheck bots on different proxies (`~N` steps wait N seconds). The control plane makes `netstaff` an admin when it logs in (`HALLOW_ADMINS` in `compose.hallow.yaml`), which gives it the staff commands:

```sh
netcheck --proxy 127.0.0.1:25566 --name bob --route "~6,/r reply-cross" --stay 8 &
netcheck --proxy 127.0.0.1:25565 --name netstaff \
  --route "~2,/msg bob hello-cross,/find bob,~4,/alert alert-cross,~1,/gkick bob testing" --stay 2
```

What each bot should print:

- netstaff:
  - `[To bob] hello-cross`
  - `bob is on lobby (proxy-2)`
  - `[From bob] reply-cross`
  - the alert
  - `Kicked bob.`
- bob:
  - `[From netstaff] hello-cross`
  - the alert
  - a kick with the reason, which makes bob's run fail
