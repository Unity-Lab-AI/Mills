# Mills

A private workspace for Alfreddo and Sponge: talk to Claude in the browser and have it build and edit files on your own PC. There's nothing to install on your side.

**Web app:** https://unity-lab-ai.github.io/Mills/ (deployed from `main` by GitHub Actions)

| Person | Address | Bridge window on Gee's PC |
|--------|---------|---------------------------|
| Alfreddo | https://unity-lab-ai.github.io/Mills/ | `bridge/start.bat` |
| Sponge | https://unity-lab-ai.github.io/Mills/?user=sponge | `bridge/start-sponge.bat` |

Each person has their own **profile**: their own password, device lock, Claude conversation, tunnel and Archive folder. The bridges run side by side in separate windows and never see each other's state.

## How it works

```
User's browser (GitHub Pages UI) ──password──▶ Cloudflare tunnel ──▶ Mills bridge on Gee's PC (one per profile)
   │  holds the folder they opened on their PC                             └─ runs Claude Code (Gee's account) in the Mills folder
   ◀── Claude's file edits (list/read/write/delete/mkdir) are relayed back to their browser and applied there
```

## Gee: running it

1. Double-click **`bridge/start.bat`** (Alfreddo) and/or **`bridge/start-sponge.bat`** (Sponge) and leave each window open while that person works.
   - The first time a profile runs, it asks you to set that person's Mills password.
   - Each bridge starts its own Cloudflare tunnel and posts the address to its own gist on your GitHub account (`Mills bridge address` for Alfreddo, `Mills bridge address (sponge)` for Sponge), where the web app finds it automatically.
2. Requires Node.js, Claude Code (logged in), `gh` (logged in), and `cloudflared` (path set in `bridge/config.json`).
3. Restarting a bridge is safe: passwords live in `auth.json` and are only ever written by `set-password.js` (the launcher asks for one only when that file is missing), the device lock and sign-in sessions live in `state.json`, so nobody is signed out or asked for a new password when you close and reopen a bridge window. Only the tunnel address changes, and the web app picks that up by itself.
4. The models offered in the web app's dropdown come from `models` in `bridge/config.json` (`id` = what `claude --model` accepts, `label` = what the person sees). Edit that list and restart the bridges to change it.

### Adding another person

1. Create `bridge/profiles/<name>/config.json` with a free port and a display name, e.g. `{ "port": 7779, "user": "Red" }` (`<name>` = lowercase letters, digits, dashes).
2. Set their password: `node bridge/set-password.js --profile <name> "their password"` (or just run `bridge/start.bat <name>`, which asks on first run).
3. Run `bridge/start.bat <name>` (copy `start-sponge.bat` if you want a double-clickable launcher).
4. Give them `https://unity-lab-ai.github.io/Mills/?user=<name>` and the password. Their saved conversations land in `Mills/Archive/<name>/`.

Profile state lives in `bridge/profiles/<name>/` (`auth.json`, `state.json`, `.mcp-runtime.json` are gitignored; `config.json` is committed). Everything in `bridge/config.json` except `port` and `gistId` applies to every profile unless the profile's own `config.json` overrides it. For example, a profile can set its own default Claude model with `"model": "claude-opus-5-5"` (Sponge's profile does; an empty or missing `model` uses Claude Code's default). That default is what the web app's "Default" option means; a pick in the dropdown overrides it. Config is read when the bridge starts, so restart that profile's window after changing it.

## Alfreddo / Sponge: using it

1. Open your address from the table above in **Chrome or Edge** (on Linux: Chrome, Chromium or Brave; Firefox cannot open folders) and sign in with your password.
2. Click **Open folder on my PC** and pick the folder Claude should work in.
3. Chat with Claude. It creates and edits files in that folder, and you can also edit them yourself in the editor panel.
4. **Save to Archive** stores the current conversation; once saved, it updates itself after every reply. **Archive** lists saved conversations: **Open** one to continue it (Claude resumes with full memory of it), **Rename** it (or click the title above the chat), or **Delete** it (click twice to confirm). Saved conversations live on Gee's PC in `Mills/Archive/` (Alfreddo) or `Mills/Archive/<name>/` (other profiles) and are never committed.
5. **Theme** opens a dropdown of 23 UI colour themes (Burple, Cyberpunk, Dracula, Nord, Solarized, …) plus the Mills default. The choice is remembered in that browser.
6. The **model dropdown** in the header picks which Claude model answers you (Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5, or your bridge's default). The choice is stored by your bridge, so it applies from your next message (if Claude is mid-reply, from the one after), to every new chat, and it survives bridge restarts. Each "Done" line shows which model answered.

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

`git push origin` sends each branch to GitHub and Forgejo in one go. Check that both pushes report success. Never commit `bridge/auth.json`, `bridge/state.json`, `bridge/profiles/*/auth.json`, `bridge/profiles/*/state.json`, `Archive/` or `.claude/` (all are kept out by `.gitignore`).

## Security

- **First sign-in claims a profile.** The first browser that signs in to a profile successfully becomes the only one allowed into it, and every other device is refused. To move someone to a new browser, Gee runs `node bridge/server.js --reset-lock` (Alfreddo) or `node bridge/server.js --profile sponge --reset-lock` (Sponge).
- Passwords are stored on Gee's PC only, as scrypt hashes (`bridge/auth.json` / `bridge/profiles/<name>/auth.json`, never committed). To change one: `node bridge/set-password.js "new password"` or `node bridge/set-password.js --profile <name> "new password"`.
- After 5 failed sign-ins, sign-in is blocked for 15 minutes. Only the GitHub Pages origin may call the bridge.
- Claude can't run shell commands by default. `allowedTools` / `permissionMode` in `bridge/config.json` control that.
