# panel

The Hallow Panel: a fork of the [Calagopus Panel](https://github.com/calagopus/panel) with Hallow's network, player, punishment and staff features as extensions. See [Panel](../docs/PANEL.md).

| Path | What |
|---|---|
| `calagopus/` | Submodule: [xena-studios/calagopus-panel](https://github.com/xena-studios/calagopus-panel), branch `hallow` (upstream `main` plus our branding and extension) |
| [HALLOW-CHANGES.md](HALLOW-CHANGES.md) | Every change to Calagopus core files, and why |
| [upstream-drafts/](upstream-drafts/) | Issue and pull request text to offer upstream |

## Where the Hallow code lives

All of it is one Calagopus extension, `dev.xenastudios.hallow`, in [`backend-extensions/dev_xenastudios_hallow/`](https://github.com/xena-studios/calagopus-panel/tree/hallow/backend-extensions/dev_xenastudios_hallow) of the fork:

| Part | Path in the extension | Today |
|---|---|---|
| Backend (Rust crate) | `src/lib.rs`, routes in `src/routes/` (the file tree mirrors the URLs) | `GET /api/hallow/health` |
| Frontend (React) | `frontend/src/index.tsx`, pages in `frontend/src/pages/` | Admin sidebar entry **Network** (`/admin/hallow/network`), a placeholder page; placeholder orange theme |
| Database | `migrations/<timestamp>_<name>/up.sql` and `down.sql` | None yet |

What the extension API reaches, from reading Calagopus v1.2.10 and building against it:

- **Routes: yes.** Seven routers: public (`/`), `/api/auth`, `/api/admin` (admin session and permission checks done for you), `/api/client`, `/api/client/servers/{server}`, and two for Wings (`/api/remote…`). Routes show up in the panel's OpenAPI docs.
- **Pages: yes.** New admin, account, server and top-level routes with sidebar entries, slots in existing pages, route interceptors to reorder or replace stock pages, a Mantine theme and CSS, and file overrides as a last resort.
- **Tables: yes.** Each extension has its own SQL migrations, applied on startup after the core's. No separate schema extension is needed; `schema-extension/` is a Rust derive for adding fields to the core's models. Hallow's own tables stay in the `hallow` schema, created by `hallow-db` (see [hallow-db](../crates/db/README.md)). The extension reads and writes them by qualified name (`hallow.accounts`) and does not migrate them. Extension migrations are for tables only the panel uses.
- **Also:** permissions (user, admin, server), settings with an admin form, background tasks, CLI commands, events on core models, email templates, activity logging.
- **Can't:** change the core's own Drizzle migrations, or add an admin sidebar category (only the built-in ones, or an unlabelled group at the bottom). Extensions are compiled in, so adding or changing one means rebuilding the panel; the stock `:latest` image can't, but the `:heavy` image and a source build can.

The Network page (#24) reads the control plane's `GET /v1/proxies`, `GET /v1/servers` and `GET /v1/presence/count` (`crates/proto`). It belongs in a backend route under `/api/admin/hallow/` (`builder.add_admin_api_router` in `lib.rs`), so browsers never reach the control plane directly, and replaces `frontend/src/pages/network/NetworkPage.tsx`.

## Build and run

Needs Rust 1.99 or newer, Node.js 24, pnpm 11 or newer, PostgreSQL 16+ and Valkey or Redis. The frontend is built first, because the backend embeds it.

```sh
git submodule update --init panel/calagopus
cd panel/calagopus

(cd frontend && pnpm install --frozen-lockfile && pnpm build)
SQLX_OFFLINE=true cargo build -p panel-rs     # target/debug/panel-rs
```

Run it against a PostgreSQL and a Valkey; it applies its migrations, then the extension's, on startup:

```sh
docker run -d --name hallowpanel-db -e POSTGRES_USER=panel -e POSTGRES_PASSWORD=panel \
  -e POSTGRES_DB=panel -p 127.0.0.1:55432:5432 postgres:18-alpine
docker run -d --name hallowpanel-cache -p 127.0.0.1:56379:6379 valkey/valkey:9.1-alpine

REDIS_URL=redis://127.0.0.1:56379 \
DATABASE_URL=postgresql://panel:panel@127.0.0.1:55432/panel \
DATABASE_MIGRATE=true APP_PRIMARY=true PORT=18200 \
APP_ENCRYPTION_KEY=change-me \
  ./target/debug/panel-rs
```

Then open <http://127.0.0.1:18200> for the first-run setup, and check the extension with `curl http://127.0.0.1:18200/api/hallow/health`. A cold build takes about 10 minutes; later builds take seconds. For frontend work, `pnpm dev` in `frontend/` serves on port 5173 and proxies the API to `BACKEND_PORT` (default 8000). Upstream's [DEVELOPMENT.md](https://github.com/xena-studios/calagopus-panel/blob/hallow/DEVELOPMENT.md) and [extension docs](https://calagopus.com/docs/panel/extensions/) cover the rest.

`SQLX_OFFLINE=true` builds against the query metadata checked into `.sqlx/`, so no database is needed at build time.

For production, Calagopus publishes Docker images built by its own release workflow. A Hallow image (from a source build, or Calagopus's `:heavy` image with our extension) comes with deployment.

A CI job for this build, [panel.yml](../.github/workflows/panel.yml), runs on every change under `panel/`. It must not become a required check.

## Working on the extension

Inside `panel/calagopus`, on the `hallow` branch:

```sh
cargo fmt && SQLX_OFFLINE=true cargo clippy -p dev_xenastudios_hallow -- -D warnings
(cd frontend && pnpm fmt && pnpm lint && pnpm build:ci)
```

After changing the extension's `Cargo.toml` or `Metadata.toml`, run `./target/debug/panel-rs extensions resync` to regenerate the internal extension list. New migrations: `panel-rs database-migrator create dev.xenastudios.hallow`. Push to the fork, then move the submodule pin here in the same pull request as anything that depends on it.

A change to a core file needs a row in [HALLOW-CHANGES.md](HALLOW-CHANGES.md) and an upstream draft. Calagopus [rejects code written by AI tools](https://github.com/calagopus/panel/blob/main/CONTRIBUTING.md#ai--llm-usage), so a person writes any code we send them.

## Taking upstream updates

We merge upstream `main` into `hallow` at least every two weeks, and before every release ([Panel](../docs/PANEL.md#rules-for-the-fork)):

```sh
cd panel/calagopus
git remote add upstream https://github.com/calagopus/panel   # once
git fetch upstream
git checkout hallow
git merge upstream/main
```

Conflicts in `backend-extensions/internal-list/`, `Cargo.lock` or `frontend/pnpm-lock.yaml`: take upstream's version, then run `panel-rs extensions resync`, `cargo build` and `pnpm install` to regenerate them. Then build, run, check the Network page and `/api/hallow/health`, push `hallow`, and bump the submodule here:

```sh
cd ../..
git add panel/calagopus
git commit -m "chore(panel): merge upstream calagopus/panel <short sha>"
```

If upstream raised the extension API version, raise `panel_version` in the extension's `Metadata.toml` to match. Renovate leaves submodules alone (`renovate.json`), so nothing bumps the pin automatically.
