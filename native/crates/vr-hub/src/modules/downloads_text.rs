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
