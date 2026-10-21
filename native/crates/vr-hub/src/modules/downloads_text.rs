//! The Download panel's READMEs and launchers (moved from bridge/downloads.js, word for word).

pub fn app_readme(win: bool, hub: &str) -> String {
    let os = if win { "Windows" } else { "Linux" };
    format!("VibeRoller ({os}), the native app
==================================
VibeRoller in its own window: chat, Work, the board, notes, Files and Unity, signed in with your VibeRoller account
({hub}), plus what only an app on your computer can do: the Tools window has your projects folder (no permission
prompts), a real terminal, build & run buttons, git with your Forgejo token, a local API proxy (tools on this computer
use VibeRoller's AI through http://127.0.0.1:<port>/v1, OpenAI or Anthropic format, with your key), and the switch to
share this computer's AI with the team (Settings → My AI hosts → \"Run on this PC\" sets it up).
Pre-built: nothing to install first{}.

  - Run {}: it works right from this folder (portable).
  - To install it ({}): Tools → Settings →
    \"Install on this computer\", or run {}. An earlier install is updated.
  - It updates itself from {hub} (releases signed with VibeRoller's key), and the window always shows the current
    VibeRoller.
  - vr-host{} next to it is the AI host the app switches on and off (it runs on its own, also when the app is closed).
",
        if win { " (Windows 10 / 11 has the WebView2 it uses)" } else { " (needs WebKitGTK, which desktop Linux has: libwebkit2gtk-4.1-0)" },
        if win { "vr-desktop.exe" } else { "./vr-desktop" },
        if win { "%LOCALAPPDATA%\\VibeRoller\\Desktop, with Start-menu and Desktop shortcuts" } else { "~/.local/share/viberoller/desktop, an application-menu entry and ~/.local/bin/viberoller" },
        if win { "vr-desktop.exe --install" } else { "./vr-desktop --install" },
        if win { ".exe" } else { "" })
}
pub fn host_readme(win: bool, hub: &str, nickname: Option<&str>) -> String {
    let os = if win { "Windows" } else { "Linux" }; let exe = if win { "vr-host.exe" } else { "./vr-host" };
    format!("VibeRoller AI Host ({os}), native
==================================
Makes this computer an AI host for VibeRoller at {hub}: it lends its AI (its Claude / ChatGPT login, local models,
Unity's engines) without sharing the computer itself. It connects OUT to the server; no port opens here.
Pre-built: one program, nothing else to install (no Node.js).

  - {exe} install          installs it ({}) or updates an earlier install
                             (the JavaScript host's folder too: it takes over its ai-node.json and settings).
    {exe} install --boot {}   … and starts it at boot ({}).
  - {exe}                  the heart & knife control app: whether it runs, is connected, in use, up to date; start /
                             stop / restart / pause / update / log / every setting / start at boot.
  - {exe} status | start | stop | set <setting> <value> | settings | log   for scripts.
  - {}
  - It updates itself from the hub (signed with VibeRoller's release key and your host's token), between answers.
",
        if win { "%LOCALAPPDATA%\\VibeRoller\\AIHost" } else { "~/.local/share/viberoller/ai-host" },
        if win { "service" } else { "user" },
        if win { "the VibeRollerAIHost service; or sign-in" } else { "a user service; or service (sudo) for the system" },
        match nickname { Some(n) => format!("ai-node.json here is your host \"{n}\" (keep it private: the token is its key to the hub)."), None => "Make your host in VibeRoller → Settings → My AI hosts, then download its own package (its ai-node.json inside).".into() })
}
pub const DESKTOP_CMD: &str = "@echo off
rem Vibe Roller Desktop: VibeRoller Chat in its own window, and its own installer + updater (README.txt).
rem Double-click: finds an earlier install and updates it, or asks: install or portable. Options: -Install -Portable -Update -Status
powershell -NoProfile -ExecutionPolicy Bypass -File \"%~dp0vr-desktop.ps1\" %*
if errorlevel 1 pause
";
pub fn desktop_readme_win(hub: &str) -> String { format!("Vibe Roller Desktop (Windows)
=============================
VibeRoller Chat in its own window: servers, channels, groups, private messages, calls and Unity, signed in with your
VibeRoller account. Your VibeRoller: {hub}

This folder is the installer, the app and the updater in one:
  - Double-click \"VibeRoller Desktop.cmd\". If VibeRoller Desktop is already installed (any earlier version, wherever
    it was), it is updated to this one and opens. If not, it asks: Install (into %LOCALAPPDATA%\\VibeRoller\\Desktop,
    with Start-menu and Desktop shortcuts) or Portable (runs from this folder, installs nothing).
  - Installed, it keeps itself up to date: at every start it asks {hub} for the current package and updates itself
    when something changed. The window itself always shows the current VibeRoller (it is the hub's own page).
  - Options (from a command prompt): -Install, -Portable, -Update, -Status.
Needs Chrome, Edge or Brave (one of them is almost always there).

Also possible: open {hub}/index.html?app=1 in Chrome, Edge or Brave and use the install icon in the address bar.
") }
pub fn desktop_readme_linux(hub: &str) -> String { format!("Vibe Roller Desktop (Linux)
===========================
VibeRoller Chat in its own window: servers, channels, groups, private messages, calls and Unity, signed in with your
VibeRoller account. Your VibeRoller: {hub}

This folder is the installer, the app and the updater in one:
  - sh viberoller-desktop.sh   If VibeRoller Desktop is already installed (any earlier version), it is updated to this
    one and opens. If not, it asks: Install (into ~/.local/share/viberoller/desktop, with an application-menu entry and
    ~/.local/bin/viberoller-desktop) or Portable (runs from this folder, installs nothing).
  - Installed, it keeps itself up to date: at every start it asks {hub} for the current package and updates itself
    when something changed. The window itself always shows the current VibeRoller (it is the hub's own page).
  - Options: --install, --portable, --update, --status.
Needs Chrome, Chromium, Brave or Edge, and curl (for updates).

Also possible: open {hub}/index.html?app=1 in Chrome, Chromium, Brave or Edge and use the install icon in the address bar.
") }
pub const HOST_CMD: &str = "@echo off
rem Vibe Roller AI Host: installs, runs and updates this PC's AI host (README.txt). Options: --install --portable --update --status --yes
where node >nul 2>nul || (echo Node.js is required: https://nodejs.org & pause & exit /b 1)
node \"%~dp0vr-setup.js\" %*
if errorlevel 1 pause
";
pub const HOST_SH: &str = "#!/usr/bin/env sh
# Vibe Roller AI Host: installs, runs and updates this machine's AI host (README.txt). Options: --install --portable --update --status --yes
command -v node >/dev/null 2>&1 || { echo \"Node.js is required: https://nodejs.org\"; exit 1; }
exec node \"$(cd \"$(dirname \"$0\")\" && pwd)/vr-setup.js\" \"$@\"
";
pub fn js_host_readme_win(hub: &str) -> String { format!("Vibe Roller AI Host (Windows)
=============================
Makes this PC an AI host for VibeRoller at {hub}: it lends its AI (its Claude / ChatGPT login, local models, Unity's
engines) without sharing the computer itself. It connects OUT to the server; no port opens here.

This folder is the installer, the host and the updater in one. Needs Node.js (https://nodejs.org).
  - Double-click \"VibeRoller AI Host.cmd\".
      An AI host is already installed here (any earlier version: the standard folder, the VibeRollerAIHost service's
      folder, C:\\VibeRollerAIHost …) → it is updated to this package; its ai-node.json, logs and settings stay, and the
      running host switches to the new version at its next quiet moment (it never cuts off an answer).
      A host that runs from a VibeRoller git checkout is left alone: it updates itself.
      Nothing installed → it asks: Install (into %LOCALAPPDATA%\\VibeRoller\\AIHost) or Portable (runs from this folder).
  - Your own host's package (VibeRoller → Settings → Connections → My AI hosts → download) has its ai-node.json inside.
    This general one has ai-node.example.json: copy it to ai-node.json and fill in hub, id and token.
  - The control app, \"heart & knife\" (it replaces the separate heart and knife): after installing it opens by itself;
    later \"VibeRoller AI Host.cmd\" in the install folder, or the \"VibeRoller AI Host\" shortcut. It shows whether the
    host runs, is connected, is in use and is up to date; [s] start, [x] stop, [r] restart, [p] pause / resume,
    [u] update now, [l] log, [c] every setting (hub, token, updates, reconnect, who may use each provider: the team
    group and / or named users, usage caps, engines, the security switches), [b] start at boot: the service (from
    boot, administrator once), at sign-in, or off. For scripts: node vr-host.js status | start | stop | set … .
  - The service and the shortcut: host-tools\\build.cmd builds them with the C# compiler that comes with Windows
    ([b] in the control app does it for you).
  - Updates: the host updates its code from the hub by itself, between answers. By hand: [u] in the control app,
    \"VibeRoller AI Host.cmd\" from a newer download, or node ai-node.js --update in the install folder.
  - Options (from a command prompt): --install, --portable, --update, --status, --yes.
") }
pub fn js_host_readme_linux(hub: &str) -> String { format!("Vibe Roller AI Host (Linux)
===========================
Makes this machine an AI host for VibeRoller at {hub}: it lends its AI (its Claude / ChatGPT login, local models,
Unity's engines) without sharing the computer itself. It connects OUT to the server; no port opens here.

This folder is the installer, the host and the updater in one. Needs Node.js.
  - sh viberoller-ai-host.sh
      An AI host is already installed here (any earlier version: the standard folder, the viberoller-ai-host service's
      folder, ~/VibeRollerAIHost …) → it is updated to this package; its ai-node.json, logs and settings stay, and the
      running host switches to the new version at its next quiet moment (it never cuts off an answer).
      A host that runs from a VibeRoller git checkout is left alone: it updates itself.
      Nothing installed → it asks: Install (into ~/.local/share/viberoller/ai-host) or Portable (runs from this folder).
  - Your own host's package (VibeRoller → Settings → Connections → My AI hosts → download) has its ai-node.json inside.
    This general one has ai-node.example.json: cp it to ai-node.json and fill in hub, id and token.
  - The control app, \"heart & knife\": after installing it opens by itself; later viberoller-ai-host.sh in the install
    folder (or sh host-tools/viberoller-host-control.sh). It shows whether the host runs, is connected, is in use and
    is up to date; [s] start, [x] stop, [r] restart, [p] pause / resume, [u] update now, [l] log, [c] every setting
    (hub, token, updates, reconnect, who may use each provider: the team group and / or named users, usage caps,
    engines, the security switches), [b] start at boot: the system service (sudo once), a user service, or off.
    For scripts: node vr-host.js status | start | stop | set … .
  - Updates: the host updates its code from the hub by itself, between answers. By hand: [u] in the control app,
    viberoller-ai-host.sh from a newer download, or node ai-node.js --update in the install folder.
  - Options: --install, --portable, --update, --status, --yes.
") }
