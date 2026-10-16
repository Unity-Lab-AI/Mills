# Mills

A private workspace for Alfreddo and Sponge (plus Gee as admin): talk to Claude in the browser and have it build and edit files on your own PC, and talk to each other in an end-to-end encrypted chat room. There's nothing to install on your side.

**Web app:** https://unity-lab-ai.github.io/Mills/ (deployed from `main` by GitHub Actions). Everyone uses the same address and signs in with **username + password**.

| Person | Username | Notes |
|--------|----------|-------|
| Alfreddo | `mills` | |
| Sponge | `sponge` | default model Opus 5.5 |
| Gee | `gee` | admin: extra **Admin** section |

After signing in you land on the **main menu**: your username in the middle, and the **Work** and **Chat** buttons (plus **Admin** for Gee only). Every section has a **← Menu** button at the top left.

## How it works

```
Browser (GitHub Pages UI) ──username+password──▶ Cloudflare tunnel ──▶ Mills hub on Gee's PC (one process, every account)
   │  Work: holds the folder they opened on their PC                         ├─ runs Claude Code (Gee's account) in the Mills folder, per account
   │  Chat: encrypts/decrypts messages locally                               ├─ relays Claude's file edits to that person's browser
   ◀── file edits (list/read/write/delete/mkdir) applied in the browser      └─ stores chat ciphertext only
```

## Gee: running it

1. Double-click **`bridge/start.bat`** and leave the window open. One window serves everyone (`start-sponge.bat` still works and just runs the same thing).
   - The first time, it asks for the `mills` account password if none is set.
   - It starts a Cloudflare tunnel and posts the address to the `Mills bridge address` gist on your GitHub account, where the web app finds it automatically.
2. Requires Node.js, Claude Code (logged in), `gh` (logged in), and `cloudflared` (path set in `bridge/config.json`).
3. Restarting is safe: passwords live in `auth.json` files and are only written by `set-password.js` or the Admin section; device locks live in each account's `state.json`; sign-in sessions live in `bridge/hub-state.json`. Nobody is signed out or asked for a new password when the hub restarts.
4. The models offered in the Work model dropdown come from `models` in `bridge/config.json` (`id` = what `claude --model` accepts, `label` = what the person sees).

### Restarts after code or config changes (Claude does this itself)

The window runs `bridge/run.js`, a small supervisor that owns the Cloudflare tunnel and runs `bridge/server.js` (the hub) underneath it. After Claude changes hub code or config it runs:

```
node bridge/restart.js
```

The hub restarts itself the moment nobody has a Claude reply in progress, under the same tunnel, so the address does not change and the web app shows "Mills is restarting" for a second and carries on. `node bridge/restart.js --hard` closes the window and relaunches `start.bat` (new tunnel address, found again automatically) for a wedged hub; `--hard --delay 30` does that 30 s later from a detached helper so the Claude run that asked for it can finish its reply; `--stop` closes it without relaunching. Progress of a hard restart is written to `bridge/restart.log`. Gee can also press **Restart hub** in the Admin section.

### Accounts

Gee manages accounts from **Admin** in the web app: create an account (a 20-character password is generated and shown once), give someone a new password, reset a device lock, sign someone out, stop their Claude run, delete an account (its folder moves to `bridge/profiles-deleted/`, nothing is destroyed). Admin can also clear the chat history.

On disk, the `mills` account is `bridge/auth.json` + `bridge/state.json` + `Mills/Archive/`; every other account is `bridge/profiles/<name>/` (`config.json` committed: `{ "user": "<display name>", "admin": true|false, "model": "<default model>" }`; `auth.json`, `state.json`, `.mcp-runtime.json` gitignored) + `Mills/Archive/<name>/`. From the command line: `node bridge/set-password.js --profile <name> "password"` (no `--profile` = the `mills` account). Everything in `bridge/config.json` applies to every account unless the account's own `config.json` overrides it; an account's `model` is what the Work dropdown's "Default" option means.

## Using it

1. Open https://unity-lab-ai.github.io/Mills/ in **Chrome or Edge** (on Linux: Chrome, Chromium or Brave; Firefox cannot open folders) and sign in with your username and password.
2. **Work** is the Claude workspace. Click **Open folder on my PC** and pick the folder Claude should work in, then chat with Claude. It creates and edits files in that folder, and you can edit them yourself in the editor panel.
   - The message box never locks: while Claude is replying, **Queue** lines a message up for right after this reply (queued messages show dashed; ✕ removes one), and the second button is **Stop** when the box is empty or **Send now** when it has text, which stops the current reply and sends your text immediately, ahead of the queue.
   - **Save to Archive** stores the current conversation; once saved, it updates itself after every reply. **Archive** lists saved conversations: **Open** (Claude resumes with full memory of it), **Rename**, **Delete** (click twice). Saved conversations live on Gee's PC under `Mills/Archive/` and are never committed.
   - The **model dropdown** picks which Claude model answers you (Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5, or your account's default). It applies from your next message (after the current reply), to every new chat, and survives restarts. Each "Done" line shows which model answered.
   - **Theme** opens 23 UI colour themes plus the Mills default, remembered in that browser.
3. **Chat** is a room shared by every account. Messages are end-to-end encrypted (see Security). A red badge on the Chat button counts new messages while you are elsewhere. The member list shows each person's key fingerprint.

## Publishing updates

Mills is mirrored to two remotes: GitHub (`Unity-Lab-AI/Mills`, which serves the Pages web app) and Forgejo (`git.unityailab.com/GFourteen/Mills`). One-time setup on each clone so every push goes to both:

```
git remote set-url --add --push origin https://github.com/Unity-Lab-AI/Mills.git
git remote set-url --add --push origin https://git.unityailab.com/GFourteen/Mills.git
git remote add forgejo https://git.unityailab.com/GFourteen/Mills.git
```

`git remote -v` should then list both push URLs under `origin`. Work follows Git Flow:

1. Branch `feature/<name>` from `develop`, commit, then `git push -u origin feature/<name>`.
2. Merge the feature into `develop` (`git merge --no-ff`) and `git push origin develop`.
3. Merge `develop` into `main` and `git push origin main` (this redeploys the web app).

`git push origin` sends each branch to GitHub and Forgejo in one go. Check that both pushes report success. Never commit `auth.json` / `state.json` files, `bridge/hub-state.json`, `Archive/`, `Chat/` or `.claude/` (all are kept out by `.gitignore`).

## Security

- **First sign-in claims an account.** The first browser that signs in to an account becomes the only one allowed into it. To move someone to a new browser, Gee uses **Reset lock** in Admin.
- Passwords are stored on Gee's PC only, as scrypt hashes. After 5 failed sign-ins for a username, that username is blocked for 15 minutes. Only the GitHub Pages origin may call the hub. The Admin endpoints check the account's `admin` flag on every request.
- **Chat encryption.** Each account has an ECDH P-256 key pair created in its browser. The private key stays in that browser and is also backed up on the hub encrypted with a key derived from the account password (PBKDF2, 310,000 rounds), so it follows the account to a new browser. Every message is encrypted with a fresh AES-256-GCM key, which is wrapped separately for each member. The hub (and its disk, `Mills/Chat/`) only ever holds public keys, the locked backups and ciphertext, and each reader is sent only their own wrapped key. Limits worth knowing: the hub sees your password for a moment when you sign in, so someone who controls Gee's PC *while you sign in* could unlock your backup; compare fingerprints in the member list once with each other to rule out a swapped key. A password reset done by Admin on someone who then signs in from a new browser starts a fresh key: older messages stay unreadable in that new browser.
- Claude can't run shell commands by default. `allowedTools` / `permissionMode` in `bridge/config.json` control that.
