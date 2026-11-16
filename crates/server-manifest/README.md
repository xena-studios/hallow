# hallow-manifest

Parses and validates a server repo's `hallow.toml` (format in [Servers](../../docs/SERVERS.md#hallowtoml)), and applies it in the server base image.

```text
hallow-manifest check [PATH] [--base-version VERSION]
hallow-manifest apply PATH --pumpkin FILE --plugins DIR [--base-version VERSION]
```

- `check` validates the manifest and the files it names. PATH is the file or the repo folder (default `.`). With `--base-version` it also checks the base range. Server repos run it in their image build and in the [image workflow](../../.github/workflows/server-image.yml).
- `apply` is what the [base image's entrypoint](../../templates/server/entrypoint.sh) runs on every start:
  - merges the manifest into the rendered `pumpkin.toml` in place;
  - copies the plugins into Pumpkin's plugin folder;
  - prints `export` lines for the server's environment, skipping variables the container already sets.

Both exit 1 on an invalid manifest, printing every problem with its field, and 2 on bad arguments:

```text
$ hallow-manifest check
hallow.toml: 2 problems:
  - lobby: a game must name the lobby players return to when it ends
  - players.min: 20 is more than players.max (12)
```

The library (`Manifest::parse`, `check_files`, `check_base`, `pumpkin_config`, `env`) is for the control plane and panel later; it depends only on `hallow-proto`. A test keeps `servers/*` and `templates/server-repo` valid.
