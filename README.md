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

## Security

- **First sign-in claims Mills.** The first browser that signs in successfully becomes the only one allowed in, and every other device is refused. To move to a new browser, Gee runs `node bridge/server.js --reset-lock`.
- The password is stored on Gee's PC only, as a scrypt hash (`bridge/auth.json`, never committed). To change it: `node bridge/set-password.js "new password"`.
- After 5 failed sign-ins, sign-in is blocked for 15 minutes. Only the GitHub Pages origin may call the bridge.
- Claude can't run shell commands by default. `allowedTools` / `permissionMode` in `bridge/config.json` control that.
