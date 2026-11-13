# upstream

Projects Hallow builds on, as git submodules pinned to commits that build together. Vine and multiversion depend on Pumpkin by the relative path `../Pumpkin`, so the directory names here matter.

| Directory | Project | Pinned to |
|---|---|---|
| `Pumpkin/` | [Pumpkin-MC/Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) | `main` |
| `Vine/` | [xena-studios/Vine](https://github.com/xena-studios/Vine), our fork of [Pumpkin-MC/Vine](https://github.com/Pumpkin-MC/Vine) | `hallow`: upstream plus fixes we've offered back |
| `pumpkin-java-multiversion/` | [Pumpkin-MC/pumpkin-java-multiversion](https://github.com/Pumpkin-MC/pumpkin-java-multiversion) | `main` |
| `wings/` | [calagopus/wings](https://github.com/calagopus/wings) | `main` |

## Clone and build

```sh
git clone --recurse-submodules https://github.com/xena-studios/hallow
cd hallow/upstream
(cd Vine && cargo build --release)
(cd pumpkin-java-multiversion && cargo build --release --target wasm32-wasip2)
(cd Pumpkin && cargo build --release)
```

Needs stable Rust with the `wasm32-wasip2` target. Wings needs Rust 1.99 or newer.

## Updating

Move a pin only after the [validation matrix](../docs/VALIDATION.md) passes on the new commits. Every change we make to a fork is offered upstream; see [Upstream](../docs/UPSTREAM.md#contributions).
