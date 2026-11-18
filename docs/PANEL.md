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
| Punishments | Ban, mute, kick, warn; temporary and permanent; reasons from templates; revoke with a reason; network-wide list with filters; full audit log. Later: IP bans, appeals. A ban moves the player to the banned lobby and keeps them there until it ends or is lifted; a kick holds them in limbo for a few seconds, then sends them to a hub ([Bans and kicks](#bans-and-kicks)). |
| Staff | Roles (helper, moderator, admin, developer, owner) and permissions for both network and panel actions; one login for everything. |
| Reports | Player reports with evidence, assignment and status. |
| Games | Game templates and maps, matchmaker settings per game, enable or disable a game. |
| Nodes and servers | Calagopus as is: nodes, long-lived servers, consoles, files, backups, databases. |
| Fleet | Every node's provider, region, kind, monthly cost and state; add, drain or cancel nodes; approve monthly orders; scaling targets, spend caps and dry run. See [Fleet](FLEET.md). |
| Stats | Network and per-game dashboards from PostgreSQL. |

## Staff, roles and ranks

Two kinds of power, kept apart on purpose:

- **Panel staff** are panel users. What they can do in the panel comes from panel roles, the Calagopus role system, which carries the Hallow extension's `hallow.*` admin permissions (`hallow.punishments.read`, `hallow.punishments.manage`, `hallow.ranks.read`, `hallow.ranks.manage` and the rest). Roles such as helper, moderator and admin are panel roles with a set of these.
- **In-game staff power** comes from Hallow ranks (`hallow.ranks`): a prefix, a weight and permission nodes in Vine's format (`hallow.staff.ban`, `hallow.staff.*`, `-hallow.staff.alert`), matched by `hallow_proto::permission`. A player's nodes are sent with their login, so a rank change applies at their next join.

The panel's Ranks page edits Hallow ranks and gives them to players through the control plane (`/v1/ranks`, `/v1/rank-grants`), behind `hallow.ranks.read` and `hallow.ranks.manage`. A panel staff member needs no Minecraft account: punishments they issue or lift name them as a `staff` actor (panel username and user id), shown as the issuer next to in-game staff names. Every panel action also goes in the panel's admin activity log.

## Bans and kicks

Nobody is disconnected by a punishment ([Never dropped](ARCHITECTURE.md#never-dropped)):

| Action | What the player sees |
|---|---|
| Ban (panel or `/ban`) | A title, then the banned lobby (`servers/banned`): the reason, the issuer as this panel shows them (a player's name, a staff member's panel name, `console`), when it was issued, when it ends with a live countdown or "permanent", the punishment id (`#123`) and how to appeal. Chat is off and every command but `/help` and `/appeal` is blocked; `/server`, `/hub`, `/lobby`, `/play` and parties are refused. A banned player who logs in goes straight there |
| Lift a ban, or it ends | "Your ban was lifted" (or ended), and a move to a hub at once, without reconnecting |
| Kick | A title with the reason, then limbo for `HALLOW_KICK_HOLD_SECS` (10), then a hub |

The appeal text and link come from the control plane's `HALLOW_APPEAL_TEXT` (legacy `§` colours allowed) and `HALLOW_APPEAL_URL`. While no banned lobby is up, banned players wait in Vine's limbo with the ban on their action bar, and the control plane starts one.

## Short-lived game instances

The matchmaker creates and deletes game instances through the Wings API. They never become one panel server record each, which would flood its database and UI. The Network page shows them live instead. The Instances page lists them, and opens an instance's console (its output and a command line) through a Wings token the Hallow extension signs for it ([Instances](INSTANCES.md#consoles)).

## Extension reach

Can an extension add database tables, API routes and frontend pages? **Yes to all three:** [Phase 0](PHASE0.md) found it in the source (#9), and the fork confirmed routes and pages by building and running the Hallow extension on v1.2.10 (2026-11-15). The panel runs each extension's migrations on startup; the Hallow extension has none yet, so a table hasn't been added end to end. Extensions add backend routes (public, admin, client, per-server and Wings-facing), admin and server pages with sidebar entries, and their own SQL migrations, plus permissions, settings, background tasks and a theme. So far Hallow needs no hooks in the core, only branding. Details and limits: [panel/README.md](../panel/README.md#where-the-hallow-code-lives).
