# VibeRoller changelog (formerly Mills)

Semantic versioning (hub law 14, with the versioning law in LAWS.md since 3.3.0): `versions.json` holds the project's
version and every module's. MAJOR = an entire refactor / rework or a change to 50 % or more of the codebase, MINOR =
moderate to large features or anything that might break, PATCH = fixes, minor features, small changes; a higher bump
resets the lower numbers to 0, and every merge into develop moves the version up. Each change bumps every module it
touches and the project by its largest bump, and adds a section here. Every change of any tracked file is
logged here in the same commit, newest first (hub law 15).

## 3.3.1 — 2026-10-20

### Changed
- **Downloads have their own place.** Settings has a fourth tab, **Downloads**: Vibe Roller Desktop and Vibe Roller AI
  Host for Windows and Linux, how each one stays up to date (the page and the Desktop app follow the hub by themselves;
  an AI Host updates itself between answers, or by hand with `node ai-node.js --update`), the version the downloads
  contain, and installing the chat app from the browser (moved here from Connections). Work's side bar has a
  **Download** button right above Settings, like the menu; every Download button opens Settings on that tab (the
  separate Download pop-up is gone). The AI Host's description now says what it shares (its logins, local models,
  Unity's engines, never the computer). Check R142 (extended).
- Modules: web/index.html 1.5.1, web/app.js 1.1.1.

## 3.3.0 — 2026-10-20

### Fixed
- **Going live no longer cuts off Gee's AI host.** The hub kept each host's session (what it offers + the session key)
  in memory only. Every restart or deploy forgot it, while the host, still holding its key, kept polling and never said
  hello again: its Claude vanished for everyone until someone restarted the host by hand, usually as Gee was leaving.
  Sessions are now kept in `bridge/ai-host-sessions.json` (gitignored) and restored at start, so a host carries on
  through a hub restart as if nothing happened. This works with the host Gee runs today; nothing to do on his side. If
  a session is missing anyway, the hub asks the host to say hello again (428 for hosts from this version, 401 for older
  ones, which re-hello on it). 1.x hosts, which never say hello, keep polling as before. Check R154.

### Added
- **Heartbeats both ways.** A host's poll is its heartbeat; every poll answer now carries the hub's (time, version,
  commit). The hub logs each host going down / coming back with how long it lasted; a host from this version logs the
  same about the hub, reconnects within 1-5 s, says hello again at once when asked, and looks for an update when the hub's
  version changes. `hub_status` (team tools) lists every AI host: up or down, since when, its version.
- **Hosts never restart in the middle of an answer:** a restart for new code or an update waits until nothing is in
  flight (relayed answers, engine and PC jobs, Work runs) and nothing came for 3 minutes (1 minute for new code).
  `node bridge/ai-node.js --update` asks a running host to update from the hub at its next quiet moment.
- **The page updates itself.** Every signed-in answer names the hub's version (`X-VR-Version`); a page loaded on an older
  version reloads itself as soon as nothing would be lost (no reply streaming, no call, no unsent text, no open editor),
  and says so until then. The desktop app is the same page, so it follows too.
- **The versioning law** (LAWS.md): MAJOR for reworks of half the codebase or more, MINOR for moderate to large features
  or anything that might break, PATCH for fixes and small changes; lower numbers reset; bump before merging into
  develop, and bump again past a teammate who merged the same number. `worktree.js merge` refuses a branch whose version
  is not above develop's and names the right next numbers. Check R41 (extended).
- **The compatibility law** and **going live never cuts anyone off** (LAWS.md): Windows and Linux always; forwards and
  backwards compatible (a feature may be missing on the older side, everything else keeps working); everything updates
  itself.

### Changed
- Modules: bridge/ai-node.js 3.2.0, bridge/ai-nodes.js 2.4.0, bridge/server.js 3.2.0, bridge/team.js 1.1.0,
  bridge/worktree.js 2.2.0, web/index.html 1.5.0.

## 3.2.1 — 2026-10-19

### Fixed
- `LAWS.md` is published: the whitelist `.gitignore` (only listed top-level files go to the remotes) kept it out of
  3.2.0, so the server's Work runs did not get the worktree law. Check R150 now makes sure it is not ignored.
- The auto-deploy runs from a copy of itself (`deploy-state/autodeploy.run.sh`): a deploy rewrites `autodeploy.sh`
  while it runs, and sh reads a script as it goes (the 3.2.0 deploy ran half old, half new and skipped its log line).
  No module changed.

## 3.2.0 — 2026-10-19

The stability pass (board T-114). Sponge: worktrees that stay fresh, Forgejo used as intended, older AI hosts accepted,
everything updating itself, nothing breaking, data always kept and a bad version rolled back by itself.

### Added
- **The worktree law** (`LAWS.md`, a tracked laws file every Work run now reads next to the host's own `CLAUDE.md`):
  never build on a stale worktree. `node bridge/worktree.js check <user>` fetches Forgejo and says FRESH or STALE (exit
  3): behind Forgejo, or on the wrong branch. `update <user>` fast-forwards a clean one, merges Forgejo's integration
  branch into your own commits, or renews it. `renew <user>` makes a NEW worktree from Forgejo and carries over your
  commits, uncommitted changes and untracked files, then deletes just the old one. If something does not come over, the
  old one stays next to it with the reason. Its state is kept under `refs/viberoller/renewed/` either way, so nothing is
  ever lost. The hub checks worktrees in the background (when a run starts, on a project switch, every 5 minutes for
  anyone active) and fast-forwards a clean one by itself. Each run's instructions carry the latest finding. Check R150.
- **Forgejo is the source of truth.** `merge` merges onto Forgejo's develop in a scratch checkout and pushes FIRST; `live`
  merges Forgejo's develop into Forgejo's main and pushes it FIRST. A failed push changes nothing anywhere (it says to
  connect your Forgejo account if that is why). The server's auto-deploy then ships it. Local integration branches are
  brought level with Forgejo whenever the tool runs.
- **Deploys roll themselves back.** `deploy/autodeploy.sh` snapshots the hub's data before every deploy (the last 10, in
  `snapshots/`), then checks the hub really comes up on the new commit (`/api/status` now reports `version` + `commit`
  locally). A version that does not come up within 2 minutes of its restart (or never answers) is put back to the last
  good commit by itself. The team chat is told on the next start, and that commit is skipped until main moves on. A
  version whose scripts do not parse never starts. Data is never touched. Check R151.
- **AI hosts update themselves from the hub** (`GET /node/update`: the host's files with their sha256, signed with that
  host's own secret; `/node/update/file/<name>`). A host from this version on checks after every hello and every 10
  minutes, between answers only. It downloads what changed, verifies it, starts the new version once with
  `--selftest`, swaps it in with a backup and restarts on it. After 3 starts that never reach the hub it goes back by
  itself. A host running from a git checkout (Gee's PC) fast-forwards the checkout to the hub's commit instead.
  Launchers are never swapped while running. `"autoUpdate": false` in `ai-node.json` switches it off. Check R152.

### Changed
- **Older AI hosts keep working.** A host that does not say it can relay a login (every host before this version) is
  asked once with a relay job to an address no host forwards. A 3.x host refuses it (nothing leaves that machine); a
  2.x host does not know relays, and its Claude then runs Work the way it did then, instead of every run failing. Gee's
  3.0 host keeps working exactly as now; updating it once brings the self-updater, after which it never needs a hand.
- `start-node.bat` / `start-node.sh` start the host again on exit 3 (new code or an update) and 10 s after a crash; they
  used to close, so a host restarting itself for new code stayed off until someone started it.
- **Sign-in limits are per address.** The lockout used to be per username, so five wrong guesses from anywhere locked
  a teammate out; and flooding made-up usernames wiped every counter. Now: 5 failures for one username from one address
  or 20 from one address in total → that address waits 15 minutes; at most 3 sign-ins in flight per address; counters
  expire one by one. Pre-sign-in bodies are capped (16 KB for sign-in; too large → 413). `deploy/nginx-portal.conf` adds
  matching nginx rate and connection limits (applied when the site file is installed again; the hub's own limits work
  without it).
- The hub keeps running after an unexpected error in a callback (logged), and system errors are answered without
  server paths. The internal secret is compared in constant time.
- `worktree.js` run from inside a worktree reads the main checkout's `config.local.json` (it looked for worktrees in
  the wrong folder on the server).
- The regression runner drops a Work run's Forgejo login from its own environment (it leaked into the test hub's runs).
- Modules: bridge/ai-node.js 3.1.0, bridge/ai-nodes.js 2.3.0, bridge/server.js 3.1.0, bridge/worktree.js 2.1.0.

## 3.1.1 — 2026-10-19

### Fixed
- **AI hosts no longer drop off every minute.** When the server's checkout held commits that were not on Forgejo yet, the
  auto-deploy "deployed" the same commit every minute and restarted the hub each time, so every AI host (turned on with
  the heart) lost its line with "hub answered 502". `deploy/autodeploy.sh` now restarts only when Forgejo's `main` really
  has something new. The 3.1.0 commits that were made on the server are now on every remote. Check R149.

### Changed
- No module versions moved (deploy script and regression runner only).

## 3.1.0 — 2026-10-19

### Added
- **Direct messages are their own place in the chat.** The rail starts with a DM button (its own icon, with an unread
  count) above every server. It opens the Direct messages view: the one-to-one DMs, then the group DMs as DM channels
  ("New group DM"), and the key fingerprints. A server shows only its channels; groups are no longer listed inside it.
  Opening a DM or group DM from anywhere (a profile card, Unity in the member list) switches to that view, and the DM
  button reopens the last one. The rail also counts unread messages per server. Check R148.

### Fixed
- **Accurate 5h / 7d.** The exact numbers Anthropic sends with every call through an AI host's Claude (both the 5-hour
  and the 7-day window) now reach the usage bar: they are kept in `bridge/usage.json`, so a hub restart or deploy no
  longer forgets them, and Claude Code's own usage events, which often carry no number, no longer wipe them. A missing
  number counts as unknown instead of 0 %. The team tools' hub_status reads the windows of the AI the person actually
  uses (it read the hub's old login before). Nothing changes on the AI host. Check R147.

### Changed
- Modules: bridge/usage.js 1.2.0, bridge/ai-nodes.js 2.2.0, bridge/server.js 3.0.1, web/guilds.js 1.1.0,
  web/index.html 1.4.0, web/guild-settings.js 1.1.2.

## 3.0.0 — 2026-10-19

**An AI host shares its AI, never its computer.** MAJOR because a host stops doing what it did: it no longer runs Work
(Claude Code, files, tools) on its own machine.

### Changed
- **Work happens on the server, in sharedwork.** With a host's Claude chosen, Claude Code runs on the hub in the person's own
  worktree; only its model calls travel to the host (`/llm/<token>` on the hub, local only, a 12-hour token per person and
  provider), and the host adds its own login and forwards them to Anthropic. The login never leaves the host; no file on the
  host is read or written. Usage and caps come from Anthropic's own usage headers. Check R144 (rewritten).
- **The host refuses work and file jobs** unless its config says `"allowWork": true`; a host's `claude-cli` provider is
  shared as a login relay. Modules: bridge/ai-node.js 3.0.0, bridge/ai-nodes.js 2.1.0, bridge/server.js 3.0.0,
  bridge/providers.js 1.3.0.

### Added
- **ChatGPT / Codex shared by a host** (`codex-relay`, the Codex CLI's ChatGPT login): answers the API and Unity through
  the Responses API, streamed. Work stays Claude. Settings show it as "AI host, ChatGPT / Codex". Check R145 (extended).
- `bridge/host-relay.js` 1.0.0: the login relays (Claude → api.anthropic.com/v1/messages, Codex →
  chatgpt.com/backend-api/codex/responses, nothing else), token refresh through the provider's own CLI, usage headers.
- The host restarts itself on new code or config (within 20 s, between answers), so updates reach Gee's PC by themselves.
- `node bridge/handoff.js probe` (one tiny real call per shared login, no token printed), `service` (starts the boot-service
  installer: one administrator click at that PC), `restart-host`. Module: bridge/handoff.js 1.2.0.
- The AI Host download carries host-relay.js; a new host's config offers Claude and Codex relays (bridge/downloads.js 1.1.1).
- Gee's PC: shares Claude (fable-5.1, opus-5.5, sonnet-5.5, haiku-5.5; 90 % caps) and ChatGPT / Codex (gpt-6.1-sol) with
  mills, sponge and gee. Probed live: both answered "OK".

### Removed
- Remote Work runs on a host by default (still possible only with `allowWork: true` in that host's own config).
- The board: finished AI cards closed (T-45 … T-112); the Local / Cloud switch (T-109) is superseded.

## 2.2.1 — 2026-10-19

### Added
- `node bridge/handoff.js tools` builds the heart (`start-viberoller-ai-host.exe`), the knife (`stop-viberoller-ai-host.exe`)
  and the service program on a Windows host and puts "Start / Stop VibeRoller AI Host" on its Desktop, so a Work run can do
  it for the host's owner. Used on Gee's PC for the move. Module: bridge/handoff.js 1.1.0. (Project bumped by a MINOR
  change in a PATCH number because the move was already under way; noted here so it is not mistaken for a fix.)

## 2.2.0 — 2026-10-19

### Added
- **The hand-off: the move with nothing for Gee to do.** `node bridge/handoff.js pack --to ovh` makes the running hub pack
  its own data (accounts with their password hashes and device locks, chat + key, board, notes, pictures, saved sessions,
  settings; a tar.gz written by the hub itself), carry it to the server over ssh and run the restore there; meanwhile this
  hub pauses writes (`bridge/.maintenance`: sign-ins, chat, board and notes answer "VibeRoller has moved to …", the Work
  run doing the move keeps going). `node bridge/handoff.js switch` then hands off: `bridge/.handoff` makes `run.js`
  (start.bat, the autostart launcher, the service) run the **AI host** here instead of the hub, from the next graceful
  restart and at every start, with no tunnel. `undo` brings the hub back. A Work run may run it (allowed tool). Check R146.
  Modules: bridge/handoff.js 1.0.0 (new), bridge/run.js 1.1.0, bridge/server.js 2.2.0.
- Gee's PC's AI host config now offers his Claude to the group (mills, sponge, gee) besides Unity's engines, so once handed
  off his PC is the team's Claude with his login staying there.

### Changed
- **The portal is a normal address.** Sponge: "All of this is actually way too much for gee or mills to directly handle with
  this hyper security shit, I think instead of 404, people who go there with no username and password cant do anything."
  Both nginx site files drop the inside-only rule and the droids 404: anyone can reach the sign-in page at
  https://portal.unityailab.com, and the sign-in (username + password + device lock) is the protection. No SSH tunnel or
  proxy is needed any more. README, deploy/README and check R139 follow.

## 2.1.1 — 2026-10-19

### Fixed
- `deploy/setup-server.sh` sets `core.fileMode false` on the live checkout, so marking the deploy script executable on the
  server no longer counts as a local change that blocks the auto-deploy (it blocked the 2.1.0 deploy once; the server
  was fixed by hand). No module changed.

### Changed
- The portal is live: nginx sites installed, `portal.unityailab.com` has its DNS record and certificate, and the server
  hub's `publicUrl` is `https://portal.unityailab.com`.
- Both nginx site files now open `/node/` to everyone (an AI host connects from its home and answers only to its own id +
  secret) while everything else stays inside-only, and `viberoller.git.unityailab.com` follows the same rules as the
  portal, so there is no second, public sign-in page. (Needs the root reload once more.)

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
