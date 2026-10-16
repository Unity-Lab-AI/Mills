# Mills

A private workspace for a small team: talk to Claude in the browser and have it build and edit files on your own PC, and talk to each other in an end-to-end encrypted chat room. There's nothing to install on your side.

**Web app:** https://unity-lab-ai.github.io/Mills/ (deployed from `main` by GitHub Actions). Everyone uses the same address and signs in with **username + password**.

Accounts are created by an admin (see Accounts). The default account is `mills`; accounts flagged `admin` get an extra **Admin** section.

After signing in you land on the **main menu**: your username in the middle, and the **Work** and **Chat** buttons (plus **Admin** for admin accounts only). Every section has a **← Menu** button at the top left.

## How it works

```
Browser (GitHub Pages UI) ──username+password──▶ Cloudflare tunnel ──▶ Mills hub on the host PC (one process, every account)
   │  Work: holds the folder they opened on their PC                         ├─ runs Claude Code (the host's account) in the Mills folder, per account
   │  Chat: encrypts/decrypts messages locally                               ├─ relays Claude's file edits to that person's browser
   ◀── file edits (list/read/write/delete/mkdir) applied in the browser      └─ stores chat ciphertext only
```

## Host: running it

1. Double-click **`bridge/start.bat`** and leave the window open. One window serves every account.
   - The first time, it asks for the `mills` account password if none is set.
   - It starts a Cloudflare tunnel and posts the address to the `Mills bridge address` gist on the GitHub account `gh` is logged in to, where the web app finds it automatically.
2. Requires Node.js, Claude Code (logged in), `gh` (logged in), and `cloudflared` (path set in `bridge/config.json`).
3. Restarting is safe: passwords live in `auth.json` files and are only written by `set-password.js` or the Admin section; device locks live in each account's `state.json`; sign-in sessions live in `bridge/hub-state.json`. Nobody is signed out or asked for a new password when the hub restarts.
4. The models offered in the Work model dropdown come from `models` in `bridge/config.json` (`id` = what `claude --model` accepts, `label` = what the person sees).

### Restarts after code or config changes (Claude does this itself)

The window runs `bridge/run.js`, a small supervisor that owns the Cloudflare tunnel and runs `bridge/server.js` (the hub) underneath it. After Claude changes hub code or config it runs:

```
node bridge/restart.js
```

The hub restarts itself the moment nobody has a Claude reply in progress, under the same tunnel, so the address does not change and the web app shows "Mills is restarting" for a second and carries on. `node bridge/restart.js --hard` closes the window and relaunches `start.bat` (new tunnel address, found again automatically) for a wedged hub; `--hard --delay 30` does that 30 s later from a detached helper so the Claude run that asked for it can finish its reply; `--stop` closes it without relaunching. Progress of a hard restart is written to `bridge/restart.log`. Admins can also press **Restart hub** in the Admin section.

### Accounts

Admins manage accounts from **Admin** in the web app: create an account (a 20-character password is generated and shown once), give someone a new password, reset a device lock, sign someone out, stop their Claude run, delete an account (its folder moves to `bridge/profiles-deleted/`, nothing is destroyed). Admin can also clear the chat history.

On disk, the `mills` account is `bridge/auth.json` + `bridge/state.json` + `Mills/Archive/`; every other account is `bridge/profiles/<name>/` (`config.json`: `{ "user": "<display name>", "admin": true|false, "model": "<default model>" }`; the whole `bridge/profiles/` folder is gitignored, so names, admin flags and password hashes never leave the host) + `Mills/Archive/<name>/`. From the command line: `node bridge/set-password.js --profile <name> "password"` (no `--profile` = the `mills` account). `node bridge/master-logins.js` gives every account a new generated password and writes the whole list (username, password, display name, admin) to `Mills-master-logins.txt` on the host's Desktop — outside the Mills folder on purpose, so Claude runs in the hub never see it; device locks and sign-ins are untouched. An account whose `config.json` goes missing keeps signing in with its password (display name = username, not admin) and the hub window prints a warning, so recreate the file to restore it. Everything in `bridge/config.json` applies to every account unless the account's own `config.json` overrides it; an account's `model` is what the Work dropdown's "Default" option means.

## Using it

1. Open https://unity-lab-ai.github.io/Mills/ in **Chrome or Edge** (on Linux: Chrome, Chromium or Brave; Firefox cannot open folders) and sign in with your username and password.
2. **Work** is the Claude workspace. Click **Open folder on my PC** and pick the folder Claude should work in, then chat with Claude. It creates and edits files in that folder, and you can edit them yourself in the editor panel.
   - The message box never locks: while Claude is replying, **Queue** lines a message up for right after this reply (queued messages show dashed; ✕ removes one), and the second button is **Stop** when the box is empty or **Send now** when it has text, which stops the current reply and sends your text immediately, ahead of the queue.
   - **Save to Archive** stores the current conversation; once saved, it updates itself after every reply. **Archive** lists saved conversations: **Open** (Claude resumes with full memory of it), **Rename**, **Delete** (click twice). Saved conversations live on the host PC under `Mills/Archive/` and are never committed.
   - The **model dropdown** picks which Claude model answers you (Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5, or your account's default). It applies from your next message (after the current reply), to every new chat, and survives restarts. Each "Done" line shows which model answered.
   - **My worktree / Shared folder** (top bar) picks where Claude works when you ask it to change Mills itself. **My worktree** (the default) is your own git copy of Mills on the host, `Mills-worktrees/<username>` next to the Mills folder, on branch `work/<username>`, created from `develop` the first time you send a message, so several people can have Claude change Mills at the same time without touching each other's changes. Ask Claude to "merge into develop" when your change is done; going live (develop → main + hub restart) is a separate step that only happens when asked. **Shared folder** is the host's Mills folder itself, the running hub's own checkout that everyone shares. Switching starts a new chat. The host can run `node bridge/worktree.js status | ensure <username> | sync <username> | merge <username>`; `"worktreeRoot"` in `bridge/config.json` moves the worktrees folder.
   - **Theme** opens 23 UI colour themes plus the Mills default, remembered in that browser.
3. **Chat** is a room shared by every account. Messages are end-to-end encrypted (see Security). A red badge on the Chat button counts new messages while you are elsewhere. The member list shows each person's key fingerprint.
   - **Unity (Claude) is in the room too.** She says hi when someone signs in. Otherwise she stays quiet until someone names her (`@unity …`, a message starting with `unity`, or `@claude …`), then reads what was said since she last spoke and answers in the room in the lab's Unity persona. It works in the host's Mills folder, so it can also work on Mills itself when asked. The hub won't restart while Claude or anyone else is in the middle of a reply. Set `"roomAgent": false` in `bridge/config.json` to remove it, `"roomAgentModel"` to choose its model, and `"roomAgentName"` to rename it (default Unity).

## Publishing updates

The app is published from `main` (GitHub Pages deploys `web/`). Work follows Git Flow: `feature/<name>` from `develop`, merged `--no-ff` into `develop`, then `develop` into `main`. The Pages workflow fills in the gist owner from the `MILLS_GIST_OWNER` repository variable (Settings → Secrets and variables → Actions → Variables), so no account handle lives in the source. Never commit `auth.json` / `state.json`, `bridge/profiles/`, `bridge/hub-state.json`, `Archive/`, `Chat/`, logs or `.claude/` (all kept out by `.gitignore`).

## Security

- **First sign-in claims an account.** The first browser that signs in to an account becomes the only one allowed into it. To move someone to a new browser, an admin uses **Reset lock** in Admin.
- Passwords are stored on the host PC only, as scrypt hashes. After 5 failed sign-ins for a username, that username is blocked for 15 minutes. Only the GitHub Pages origin may call the hub. The Admin endpoints check the account's `admin` flag on every request.
- **Chat encryption.** Each account has an ECDH P-256 key pair created in its browser. The private key stays in that browser and is also backed up on the hub encrypted with a key derived from the account password (PBKDF2, 310,000 rounds), so it follows the account to a new browser. Every message is encrypted with a fresh AES-256-GCM key, which is wrapped separately for each member. The hub (and its disk, `Mills/Chat/`) only ever holds public keys, the locked backups and ciphertext, and each reader is sent only their own wrapped key. Limits worth knowing: the hub sees your password for a moment when you sign in, so someone who controls the host PC *while you sign in* could unlock your backup; compare fingerprints in the member list once with each other to rule out a swapped key. A password reset done by Admin on someone who then signs in from a new browser starts a fresh key: older messages stay unreadable in that new browser. **Exception: Claude.** Its private key is kept on the hub (`Mills/Chat/agent-key.json`) so it can read and reply, which means the host PC can read every message sent while Claude is a member.
- Claude can't run shell commands by default. `allowedTools` / `permissionMode` in `bridge/config.json` control that.
