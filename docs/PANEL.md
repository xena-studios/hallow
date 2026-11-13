# Hallow Panel

One panel runs the whole network: nodes, servers, databases, players, ranks, staff and bans. It is a fork of the [Calagopus Panel](https://github.com/calagopus/panel) (MIT, Rust with Axum, React), with Hallow's features built as Calagopus extensions so upstream updates stay easy to take.

## Why a Calagopus fork

- Calagopus already manages nodes, server containers, consoles, files, backups and databases through Wings.
- It has first-class extension systems on both sides: `backend-extensions/` (Rust crates), `schema-extension/`, and `frontend/extensions/`.
- MIT licence: we can fork and change it freely.
- It is very active (21 commits in the month before 2026-11-13), so a heavy fork would fall behind. Extensions sit on top.

## Rules for the fork

1. Hallow features are extensions, not edits to core files.
2. A core change is only a hook an extension needs, or branding. Each one is listed in `panel/HALLOW-CHANGES.md` and offered upstream as a pull request.
3. We merge upstream at least every two weeks, and before every release.

## What Hallow adds

| Area | Features |
|---|---|
| Network | Live view of proxies, lobbies, game instances, queues and player counts, from the control plane. Drain or restart a proxy. |
| Players | Search by name, UUID or IP; profile with ranks, stats, sessions, alts, punishment history, chat and command logs. |
| Punishments | Ban, mute, kick, warn; temporary and permanent; IP bans; appeals; reasons from templates; full audit log. |
| Staff | Roles (helper, moderator, admin, developer, owner) and permissions for both network and panel actions; one login for everything. |
| Reports | Player reports with evidence, assignment and status. |
| Games | Game templates and maps, matchmaker settings per game, enable or disable a game. |
| Nodes and servers | Calagopus as is: nodes, long-lived servers, consoles, files, backups, databases. |
| Stats | Network and per-game dashboards from PostgreSQL. |

## Short-lived game instances

The matchmaker creates and deletes game instances through the Wings API. They never become one panel server record each, which would flood its database and UI. The Network page shows them live instead.

## Open question

How far does the extension API reach: can an extension add database tables, API routes and frontend pages? Phase 0 checks. If not, those gaps are the hooks we add to the core.
