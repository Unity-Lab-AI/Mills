# VibeRoller changelog (formerly Mills)

Semantic versioning (hub law 14): `versions.json` holds the project's version and every module's.
MAJOR = something stops working the old way (routes, stored files, message format, a removed feature),
MINOR = something new that leaves the rest working, PATCH = a fix or polish. Each change bumps every module it
touches and the project by its largest bump, and adds a section here. Every change of any tracked file is
logged here in the same commit, newest first (hub law 15).

## 1.3.1 — 2026-10-18

### Fixed
- The version checks (R132, R133) counted Windows line endings as a change. `.bat`, `.cmd` and `.ps1`
  files are checked out with CRLF on purpose (`.gitattributes`), so the checks now compare files with line
  endings ignored. No module changed.

## 1.3.0 — 2026-10-18

### Added
- Autostart, so the host is always there for AI: `bridge/autostart-windows.cmd` puts a launcher into the
  Windows Startup folder that opens `start.bat` minimized at every sign-in (`remove` undoes it), and
  `bridge/viberoller.service` does the same on Linux as a systemd user service, at boot with lingering on.
  A second copy is harmless. Gee's PC already has the launcher.
- The host's AI switch and the host tools. `start-viberoller-host.exe` (black heart) and
  `stop-viberoller-host.exe` (knife) switch THIS machine's AI hosting on and off, never the system. Stopping
  pauses only the AI that runs here: Work runs on the host's Claude, the API's answers from it, Unity's
  assistant and Unity in the chat (who says she is paused). Chat, the board, calls, notes and people's own
  AI keys keep working for everyone. Starting brings it back at once, and starts VibeRoller here if it is
  not running. Build them once with `bridge/host-tools/build.cmd`, which draws the icons and compiles both
  tools with the C# compiler that ships with Windows, then puts shortcuts on the Desktop. Linux twins:
  `start-viberoller-host.sh` / `stop-viberoller-host.sh`. `/api/check` reports `aiHost`. Regression check
  R136. Modules: bridge/server.js 1.3.0, bridge/providers.js 1.1.0.

### Changed
- **Mills is now VibeRoller.** The new name shows everywhere people see it: the site, the sign-in card, the
  menu, Settings, the installed app (now "VibeRoller Chat"), the team-chat welcome lines, Claude's and
  Unity's instructions, the hub's messages and the README. The everyone-server takes the name once. Stored
  names stay as they were so nothing breaks: the `mills` account and server id, conversation ids, folders,
  `MILLS_*` settings and the `X-Mills-*` internal headers. Older sections of this changelog keep the old name.
- Modules: bridge/assistant.js 1.0.1, bridge/board-mcp.js 1.0.1, bridge/guild-perms.js 1.1.1,
  bridge/guilds.js 1.1.1, bridge/openai.js 1.0.1, bridge/pc-tools-mcp.js 1.0.1, bridge/run.js 1.0.1,
  bridge/server.js 1.2.1, bridge/team-mcp.js 1.0.1, bridge/team.js 1.0.1, web/app.js 1.0.1,
  web/board.js 1.0.1, web/calls.js 1.0.1, web/guild-settings.js 1.1.1, web/guilds.js 1.0.2,
  web/index.html 1.1.1, web/md.js 1.0.1, web/notes.js 1.0.1, web/polish.css 1.0.1, web/profile.js 1.0.1,
  web/sw.js 1.0.1 (app cache renamed, so installed apps refresh).
- Work runs may now also create Forgejo repos and inspect them (`fj repo create`, `fj repo view`,
  `fj whoami`), and manage git remotes, push, fetch and list remote branches (`git remote`, `git push`,
  `git fetch`, `git ls-remote`). Needed for the VibeRoller repo, because Forgejo does not create repos
  on push. No module changed.

## 1.2.0 — 2026-10-18

### Added
- Work keeps your place. The conversation on screen and your message queue, including a pending "Send now",
  are kept on the hub per person, in your own profile folder. They survive hub restarts, page reloads,
  closing the browser and switching devices.
- Opening Work brings back your previous conversation with its saved-session title, and Claude continues it.
  Queued messages come back too and start running. "New chat" clears the saved conversation.
- New module bridge/work-state.js 1.0.0 (routes /api/work/current and /api/work/queue, with size caps).
- Regression check R135: the conversation and the queue come back unchanged after a hub restart, and belong
  only to their person.

### Changed
- Modules: bridge/server.js 1.2.0, web/index.html 1.1.0.

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
