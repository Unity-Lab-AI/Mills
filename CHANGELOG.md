# Mills changelog

Semantic versioning (hub law 14): `versions.json` holds the project's version and every module's.
MAJOR = something stops working the old way (routes, stored files, message format, a removed feature),
MINOR = something new that leaves the rest working, PATCH = a fix or polish. Each change bumps every module it
touches and the project by its largest bump, and adds a section here. Every change of any tracked file is
logged here in the same commit, newest first (hub law 15).

## 1.1.0 — 2026-10-18

### Added
- The Mills server can have a named owner, shown to everyone, and the owner can name co-owners like on any
  other server. The hub's admin accounts keep owner rights there, so the host can always recover it. The
  Mills server can be handed on (Server Settings → Ownership) but never deleted.
- Host-set owners: `bridge/server-owners.json` (gitignored, so names stay off the remotes) gives any server
  its owner and co-owners when the hub starts. Each entry applies once, so later changes in Server Settings
  stick. Unknown accounts are skipped, and the audit log shows the change by "host".
- On this hub: Gee is the Mills server's owner, with Mills and Sponge as co-owners.
- Regression check R134.

### Changed
- Modules: bridge/guild-perms.js 1.1.0, bridge/guilds.js 1.1.0, bridge/server.js 1.1.0,
  web/guild-settings.js 1.1.0, web/guilds.js 1.0.1 (the OWNER tag shows on the Mills server too).
- The regression runner starts its throwaway hub with a pre-made server and a seed file (R113 ignores it).

## 1.0.1 — 2026-10-18

### Added
- Regression check R133: a change against production without a higher project version and its dated section
  at the top of this file fails the suite.

### Changed
- Hub law 15: every change of any tracked file (code, page, README, docs, tests, workflows) is logged here in
  the same commit, and moves the project version (PATCH at least). No module changed, so every module stays
  at its version.
- README: the Versions section names the changelog rule.
- Regression check R111 (the OpenAI-compatible API) failed once with "key none" and passed on the rerun; it now
  prints the hub's answer when making the key fails, so the next time shows the cause.

## 1.0.0 — 2026-10-18

The first versioned release: everything Mills does today, with every module at 1.0.0.

### Added
- Versioning itself: `versions.json` (project + 38 modules), this changelog, the version in Settings (site and
  the Windows / Linux app) and in `/api/check`, and a regression check (R132) that every changed module and the
  project move to a higher version with a changelog section.

### In this release (already shipped before versioning)
- Work: Claude Code in the browser with per-person worktrees, files, editor, past sessions, shared sessions,
  slash commands, usage bars, the OpenAI-compatible API with per-person keys, AI providers, My Forgejo.
- Chat: end-to-end encrypted, Discord-shaped: servers with built-in role levels (Owner, Co-owner, Super Admin,
  Admin, Super Moderator, Moderator, Member, @everyone), Discord's permission model with channel overwrites,
  invites (links, expiry, uses, temporary), password lock, moderation (timeout, mute, kick, ban), pins,
  slowmode, welcome / rules channels, coloured and mentionable / self-assignable roles, groups, private
  messages, calls with theater and screen share, YouTube embeds, markdown with Mermaid and KaTeX, Unity as a bot.
- Profiles (bio, pronouns, status, links, banner, local time, picture) apart from app settings.
- Board, Calendar, My day, Notes, Forgejo issue mirroring, the Unity assistant.
- Mills Chat, the installable app for Windows and Linux, with its own user panel.
- The look-and-feel layer (theme-driven, 58 custom icons) and themed scrollbars.
