# VibeRoller changelog (formerly Mills)

Semantic versioning (hub law 14): `versions.json` holds the project's version and every module's.
MAJOR = something stops working the old way (routes, stored files, message format, a removed feature),
MINOR = something new that leaves the rest working, PATCH = a fix or polish. Each change bumps every module it
touches and the project by its largest bump, and adds a section here. Every change of any tracked file is
logged here in the same commit, newest first (hub law 15).

## 2.1.0 — 2026-10-19

**AI hosts become providers.** Nobody's login sits on the server: Gee's PC (or anyone's) runs the VibeRoller AI Host
with its own Claude login, the hub on the server hands it the Work runs and API calls of the people it allows, and relays
the answers back. The hub itself needs no Claude.

### Added
- **Host identity.** Every AI host has a UUID, a secret and a **nickname** the hub issues (adjective-animal, e.g.
  silly-panda; renameable). Settings → Connections → **My AI hosts**: New host, rename, remove, who may use each of its
  providers, and the AI Host download **with that host's config inside** (unzip, run; nothing to type). Registry:
  `bridge/ai-hosts.json` (gitignored). The legacy `aiNodes` config still works (name = id). Check R143.
- **The handshake.** The host says hello with what it offers (user, providers with models, caps and allowed users, Unity's
  engines, computer access) and both sides derive one session key (HKDF of the secret and two nonces; the key id `vr-…`
  shows in Settings). Work jobs and their events travel sealed with it (AES-256-GCM) on top of TLS.
- **Host providers.** What a host offers appears in everyone's AI pickers as `nickname · label (owner)`: its **Claude**
  (Work runs and API answers happen on the host, with its login), its **local models** (OpenAI-compatible endpoints such as
  Ollama, carried to the host and back, no port opened at home). Each provider names who may use it (the host's config,
  overridable by its owner in Settings) and its usage caps (`max5h`, `max7d`, default 90 %): the hub refuses a run past the
  cap until the window resets, and the host refuses too. The usage bars show the windows of whichever Claude you use.
- **Default = Gee's host.** Config `defaultProvider: "host:gee"` makes Gee's online Claude host the default for everyone
  who may use it; `hubClaude: false` tells a hub it has no Claude of its own. Settings → **AI source** picks host → provider
  → model per person (the Work dropdown shows the same, with offline / capped marked). Check R144, R145.
- **Remote Work runs.** Claude Code runs on the host in that machine's own worktree of the project (cloning VibeRoller or a
  sharedwork project from the hub's address when it has no copy: `hubRepoUrl`, `projectsGitBase`), the page says which
  host runs the reply, Stop stops it there, a follow-up resumes the same session there, and the PC / board / team tools of
  the run reach the hub through `/node/job/<id>/internal/<tool>` with a per-run token.
- **Direct computer access.** A host with `pc.folder` answers its owner's PC tools on that folder itself (the native app
  on your own PC), instead of the browser's open folder; the browser remains the fallback.
- **Streaming through a host.** A streamed answer (SSE, NDJSON: a local model, Unity's brain) is forwarded by the host piece
  by piece as its engine produces it (`POST /node/stream`), so words arrive as they are made instead of in one block at the
  end; whole answers (pictures, voice clips) still come in one go.
- The AI Host download now carries the MCP relays and the worktree tool (`common.js`, `worktree.js`, `pc-tools-mcp.js`,
  `board-mcp.js`, `team-mcp.js`); `ai-node.example.json` shows the full 2.0 config.

### Changed
- Modules: bridge/ai-nodes.js 2.0.0 and bridge/ai-node.js 2.0.0 (the protocol above), bridge/providers.js 1.2.0,
  bridge/usage.js 1.1.0 (per-source windows), bridge/server.js 2.1.0, bridge/openai.js 1.1.0 (default provider when the
  hub has no Claude), bridge/downloads.js 1.1.0, bridge/worktree.js 2.0.1 (`VIBEROLLER_HUB_REPO` / `hubRepo`),
  web/index.html 1.3.0 (AI source + My AI hosts panels, host-aware model dropdown). The Windows / Linux app is unchanged
  (Work and Settings → Connections are the site's); its shell list is unchanged.
- `/api/check` adds `effectiveProvider`, `defaultProvider`, `hubClaude`, `pcHost`; `/api/usage` adds `of` (the source).

## 2.0.0 — 2026-10-19

The hub's home is the lab server. This release makes the server layout real (the team's shared accounts, no root), turns
Work into a projects area, gives every computer a way to become an AI host from boot, and puts both applications behind a
Download button. MAJOR because one thing stops working the old way: Work's "shared folder" choice is gone (see Removed).

### Added
- **Projects (the sharedwork area).** Work's header now picks a **project** instead of "my worktree / shared folder". Every
  project is worked on in your own worktree of it (branch `work/<you>`), so people never overlap. **＋ New project…** starts
  an empty repo in the team's shared projects area (`projectsRoot`; `/srv/apps/sharedwork` on the server), **⤓ Pull in a
  repo…** clones an existing one by its https:// or ssh address (your saved Forgejo token signs you in; local paths and
  other git helpers are refused). Layout: `<projects root>/<project>/_repo` (main checkout, integration branch checked out)
  + one worktree per person. VibeRoller itself is the default project. Claude is told which project and worktree it is in,
  and merges with `worktree.js --project <name>` (commit, merge into the integration branch and push, sync); the hub's own
  tooling stays VibeRoller-only. `node bridge/worktree.js projects | new <name> | clone <url> [name]`, and every command
  takes `--project`. Checks R140, R141.
- **The private front door.** `deploy/nginx-portal.conf`: `portal.unityailab.com` (and `portal.git.unityailab.com`, which the
  lab's wildcard certificate already covers) shows the hub's sign-in only to connections that come from the server itself
  (an SSH tunnel used as a SOCKS proxy, a VPN that ends there); everyone else gets a real 404 page, *These are not the
  droids you are looking for.* The page the hub serves now always talks to the hub that served it, whatever name reached it.
- **VibeRollerAIHost, the boot-time service.** `bridge/host-tools/AiHostService.cs` is a Windows service that runs this PC's
  AI node from boot (before anyone signs in), restarts it, logs to `ai-node.log`; `install-ai-host-service.cmd` registers it
  (asks for administrator rights once) and lets signed-in users start and stop it; `build.cmd` builds it with the heart and
  the knife and offers the install. The heart (`start-viberoller-ai-host.exe`) starts the service, the knife
  (`stop-viberoller-ai-host.exe`) stops it; without the service they fall back to the pause switch as before. Linux:
  `viberoller-ai-host.service` + `install-ai-host-service.sh`, and the heart / knife scripts drive it. Check R136 (extended).
- **Download.** A Download button above Settings in the menu (and a download icon in the app's user panel) opens the two
  applications for Windows and Linux, built by the hub from its own code so they point back at this hub
  (`bridge/downloads.js` 1.0.0, a dependency-free zip writer): **Vibe Roller Desktop** (VibeRoller Chat in its own window:
  the browser's own install card, plus a launcher for Chrome / Edge / Brave with a Start-menu / application-menu entry) and
  **Vibe Roller AI Host** (the node, the service, the heart and the knife, a README with the steps, an example config that
  already names this hub). Signed-in people only. Check R142.
- **The deploy kit for the real server layout** (`deploy/README.md`): `setup-server.sh` now runs as the hub's account without
  root (user-local Node 22 + Claude Code, the live checkout, settings with a fresh node secret, the systemd user services,
  the nginx files in `deploy-ready/`, the printed root steps); `autodeploy.sh` on a user timer fast-forwards every push to
  `main` within a minute (syntax check, graceful restart), so no deploy key, runner or secret is needed; `pack-data … ovh`
  copies the archive over by scp; `restore-data.sh` runs as the account and shares the files with the group. Check R139.

### Changed
- `bridge/config.local.json` merges nested objects one level deep, so `assistant.via` can be set alone (bridge/common.js 1.2.0).
- `/api/check` answers `project` + `projects` (the list) instead of `workspace`; `/api/workspace` takes `{ project }` (an old
  `{ workspace }` still lands in VibeRoller); new `/api/projects` (GET list, POST new / pull in) and `/api/downloads`.
- Modules: bridge/server.js 2.0.0, bridge/worktree.js 2.0.0, bridge/common.js 1.2.0, bridge/downloads.js 1.0.0 (new),
  web/index.html 1.2.0, web/app.js 1.1.0, web/polish.css 1.0.2. The Windows / Linux app (VibeRoller Chat) gains the Download
  entry in its user panel; its shell list is unchanged (the page, app.js and polish.css load network-first).
- README: the address is the portal; the Work section describes projects; the host section describes the VibeRollerAIHost
  service. `.forgejo/workflows/deploy.yml` is the optional path now.

### Removed
- Work's **"shared folder"** choice (Claude working in the hub's own checkout). Everyone works in their own worktree of a
  project; a stored "shared" choice simply means VibeRoller. The old `start-viberoller-host.exe` / `stop-viberoller-host.exe`
  names are replaced by `start-viberoller-ai-host.exe` / `stop-viberoller-ai-host.exe` (the build removes the old ones).

## 1.5.1 — 2026-10-19

### Changed
- Work runs may run `ssh ovh …`: the host PC's SSH alias for the lab server (Gee's key, set up on that PC only), so
  the move to the server can be finished from a Work session instead of by hand. No module changed.

## 1.5.0 — 2026-10-18

### Added
- **The move kit is complete** (`deploy/README.md`): `deploy/setup-server.sh` does every root step on the server in
  one go (service user, live checkout, settings, systemd service, a deploy key locked to one command, the nginx site)
  and prints what is left for people; `deploy/pack-data.cmd` / `.sh` pack exactly the hub's data on the old host with
  the hub stopped; `deploy/restore-data.sh` puts it on the server, refusing anything that is not hub data and never
  overwriting existing accounts. Checks R139.
- **No AI host, everything still reachable.** When the machine lending Unity's engines is off or paused, the hub counts
  its node offline within a minute (`aiNodeOfflineMs`), refuses new engine calls at once instead of waiting, fails a
  job no node picks up within 40 s, and Unity says "my AI host is offline" plainly in her section and in the chat.
  Chat, the board, notes, Work's saved conversation, sign-in and people's own AI keys never wait on it. Checks R138.
- **Gee's PC after the move runs the AI node**: `bridge/start-node.bat` / `.sh`, `bridge/ai-node.example.json`; the
  autostart launcher and the heart start the node when `bridge/ai-node.json` exists (the hub otherwise); the node has
  a single-instance lock (`bridge/ai-node.lock`), so a second copy just closes.
- `node bridge/worktree.js repair [--repo <host folder>]`: after the host folder is renamed or moved (it became
  `VibeRoller` today), every person's worktree pointer is fixed; a Work run also heals its own worktree by itself.

### Fixed
- The GitHub Pages workflow ran on Forgejo too (and failed there); it now runs on GitHub only.
- Gee's autostart launcher pointed at the old `Mills` folder after the rename.

### Changed
- Modules: bridge/ai-node.js 1.1.0, bridge/ai-nodes.js 1.1.0, bridge/assistant.js 1.1.1, bridge/server.js 1.4.1,
  bridge/worktree.js 1.1.0. The deploy workflow now makes one SSH call to the key's fixed command.

## 1.4.0 — 2026-10-18

### Added
- **The new repo:** https://git.unityailab.com/GFourteen/VibeRoller (Forgejo), created through Forgejo's API
  and filled with main, develop and the work branches. Every release now goes to it as well as to the
  original Forgejo repo and GitHub (the go-live script pushes to every remote).
- **The server kit** (`deploy/`, hand-off in `deploy/README.md`): a Forgejo workflow that deploys every push to
  `main` (fast-forward the live checkout, check the scripts, graceful restart), off until the repo variable
  `VIBEROLLER_DEPLOY` is `on`; a locked-down systemd service (its own user, writes only under `/srv/viberoller`,
  no privileges); an nginx site (TLS, streaming unbuffered, WebSocket-ready, `/internal/` blocked, forged
  forwarding headers dropped); the server's own settings; the steps for the data move with Gee's approval.
- **Server mode:** `bridge/config.local.json` (gitignored) is laid over `bridge/config.json`; `publicUrl` gives
  the hub a fixed address (its page learns it, that origin may call the API), `behindProxy` treats nginx-forwarded
  requests as coming from outside.
- **AI nodes:** `bridge/ai-node.js` lends a machine's local engines (Unity's Ollama brain, image, voice, speech)
  to a hub running elsewhere, connecting out over HTTPS with its own name + secret; the hub (`bridge/ai-nodes.js`,
  `assistant.via = "node"`) sends only engine names and paths. The knife pauses a node. `/api/check` lists nodes.
  The throwaway test hub now runs every Unity check through a real node. Checks R137.

### Changed
- Modules: bridge/ai-node.js 1.0.0 (new), bridge/ai-nodes.js 1.0.0 (new), bridge/assistant.js 1.1.0,
  bridge/common.js 1.1.0, bridge/server.js 1.4.0.
- Hub law 10 now names the third remote (`viberoller`).

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
