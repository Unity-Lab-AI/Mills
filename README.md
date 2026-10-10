# Mills

Alfreddo's private workspace for working with Claude. Chat with Claude, and have it build and edit files on your own PC from a web page.

**Web app:** https://unity-lab-ai.github.io/Mills/ (deployed from `main` by GitHub Actions)

## How it works

```
Browser (GitHub Pages UI)  ──password──▶  Mills bridge (bridge/server.js, on your PC, 127.0.0.1:7777)
                                              ├─ runs Claude Code in your workspace folder
                                              └─ reads/writes files in that folder only
```

The web page is only the interface. All the work happens on your machine through the bridge, which only listens on `localhost` and can't be reached from the internet.

## Setup (one time)

1. Install **Node.js 18+**: https://nodejs.org
2. Install **Claude Code** and log in once:
   ```
   npm install -g @anthropic-ai/claude-code
   claude
   ```
3. Clone this repo and double-click **`bridge/start.bat`** (Mac/Linux: `bridge/start.sh`). The first time it runs, it asks for the password you were given and stores only a hash of it locally (`bridge/auth.json`, never committed).
4. Open https://unity-lab-ai.github.io/Mills/ and sign in with the password you were given.
   (You can also use http://localhost:7777, which the bridge serves itself.)

## Security

- **First sign-in claims the bridge.** The first browser to sign in successfully becomes the only one allowed in. Any other device is refused, even if it has the password. To move to a new browser, run `node bridge/server.js --reset-lock` and then sign in again.
- The password is stored only as a scrypt hash (`bridge/auth.json`). To change it, run `node bridge/set-password.js "new password"`.
- After 5 failed attempts, sign-in is blocked for 15 minutes.
- Claude and the file editor can only reach the workspace folder (default `~/Mills-Workspace`). To change it, edit `bridge/config.json`.
- `permissionMode` and `allowedTools` in `bridge/config.json` control what Claude may do without asking. By default it can edit files and run shell commands inside the workspace.
