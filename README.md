# Mills

Alfreddo's private workspace: talk to Claude in the browser and have it build and edit files on his own PC. There's nothing to install on his side.

**Web app:** https://unity-lab-ai.github.io/Mills/ (deployed from `main` by GitHub Actions)

## How it works

```
Alfreddo's browser (GitHub Pages UI) ──password──▶ Cloudflare tunnel ──▶ Mills bridge on Gee's PC
   │  holds the folder he opened on his PC                                  └─ runs Claude Code (Gee's account) in the Mills folder
   ◀── Claude's file edits (list/read/write/delete/mkdir) are relayed back to his browser and applied there
```

## Gee: running it

1. Double-click **`bridge/start.bat`** and leave the window open while Alfreddo works.
   - The first time, it asks you to set the Mills password.
   - It starts a Cloudflare tunnel and posts the address to a gist (`Mills bridge address`) on your GitHub account, where the web app finds it automatically.
2. Requires Node.js, Claude Code (logged in), `gh` (logged in), and `cloudflared` (path set in `bridge/config.json`).

## Alfreddo: using it

1. Open https://unity-lab-ai.github.io/Mills/ in **Chrome or Edge** and sign in with the password.
2. Click **Open folder on my PC** and pick the folder Claude should work in.
3. Chat with Claude. It creates and edits files in that folder, and you can also edit them yourself in the editor panel.
4. **Save to Archive** stores the current conversation; once saved, it updates itself after every reply. **Archive** lists saved conversations: **Open** one to continue it (Claude resumes with full memory of it), **Rename** it (or click the title above the chat), or **Delete** it (click twice to confirm). Saved conversations live on Gee's PC in `Mills/Archive/` and are never committed.
5. **Theme** opens a dropdown of 23 UI colour themes (Burple, Cyberpunk, Dracula, Nord, Solarized, …) plus the Mills default. The choice is remembered in that browser.

## Publishing updates

Mills is mirrored to two remotes: GitHub (`Unity-Lab-AI/Mills`, which serves the Pages web app) and Forgejo (`git.unityailab.com/GFourteen/Mills`). One-time setup on each clone so every push goes to both:

```
git remote set-url --add --push origin https://github.com/Unity-Lab-AI/Mills.git
git remote set-url --add --push origin git@git.unityailab.com:GFourteen/Mills.git
git remote add forgejo git@git.unityailab.com:GFourteen/Mills.git
```

`git remote -v` should then list both push URLs under `origin`. Work follows Git Flow:

1. Branch `feature/<name>` from `develop`, commit, then `git push -u origin feature/<name>`.
2. Merge the feature into `develop` (`git merge --no-ff`) and `git push origin develop`.
3. Merge `develop` into `main` and `git push origin main` (this redeploys the web app).

`git push origin` sends each branch to GitHub and Forgejo in one go. Check that both pushes report success. Never commit `bridge/auth.json`, `bridge/state.json`, `Archive/` or `.claude/` (all are kept out by `.gitignore`).

## Security

- **First sign-in claims Mills.** The first browser that signs in successfully becomes the only one allowed in, and every other device is refused. To move to a new browser, Gee runs `node bridge/server.js --reset-lock`.
- The password is stored on Gee's PC only, as a scrypt hash (`bridge/auth.json`, never committed). To change it: `node bridge/set-password.js "new password"`.
- After 5 failed sign-ins, sign-in is blocked for 15 minutes. Only the GitHub Pages origin may call the bridge.
- Claude can't run shell commands by default. `allowedTools` / `permissionMode` in `bridge/config.json` control that.
