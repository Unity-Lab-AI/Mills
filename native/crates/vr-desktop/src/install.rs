//! Installing VibeRoller Desktop from its download (`vr-desktop --install`, or Tools → Settings → Install): the program
//! (and the AI host next to it) goes into the standard folder (Windows: %LOCALAPPDATA%\VibeRoller\Desktop, Linux:
//! ~/.local/share/viberoller/desktop) with a Start-menu shortcut / an application-menu entry and ~/.local/bin/viberoller.
//! An earlier install there is updated (the previous program kept as .old); the app's settings are elsewhere (its
//! config folder) and stay. Without installing, the download simply runs where it is (portable).
use std::path::{Path, PathBuf};

fn home() -> PathBuf { std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default() }
pub fn standard() -> PathBuf {
    if cfg!(windows) { std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData").join("Local")).join("VibeRoller").join("Desktop") }
    else { home().join(".local").join("share").join("viberoller").join("desktop") }
}
pub fn installed_here() -> bool { std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).and_then(|d| std::fs::canonicalize(d).ok()) == std::fs::canonicalize(standard()).ok() }

pub fn install() -> Result<String, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let here = me.parent().ok_or("no folder")?.to_path_buf();
    let target = standard();
    if installed_here() { return Ok(format!("VibeRoller Desktop is installed in {} (this copy).", target.display())); }
    std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    // (Windows: WebView2Loader.dll too, which the app loads from its own folder; a loaded one can be renamed, so the
    // running app's copy steps aside as .old like the program does)
    let names: &[&str] = if cfg!(windows) { &["vr-desktop.exe", "WebView2Loader.dll", "vr-host.exe", "vr-hostw.exe", "README.txt", "VERSION.txt"] } else { &["vr-desktop", "vr-host", "vr-hostw", "README.txt", "VERSION.txt"] };
    for n in names {
        let src = here.join(n); if !src.exists() { continue; }
        let dst = target.join(n);
        if dst.exists() { let old = target.join(format!("{n}.old")); let _ = std::fs::remove_file(&old); let _ = std::fs::rename(&dst, &old); }
        std::fs::copy(&src, &dst).map_err(|e| format!("could not copy {n}: {e}"))?;
    }
    let exe = target.join(names[0]);
    shortcuts(&exe)?;
    Ok(format!("Installed into {}, with {}. Start it from there from now on; it updates itself.", target.display(), if cfg!(windows) { "a Start-menu and a Desktop shortcut" } else { "an application-menu entry and ~/.local/bin/viberoller" }))
}

#[cfg(windows)]
fn shortcuts(exe: &Path) -> Result<(), String> {
    // a .lnk through Windows' own scripting host (no extra program)
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let menu = format!("{appdata}\\Microsoft\\Windows\\Start Menu\\Programs\\VibeRoller.lnk");
    let desk = home().join("Desktop").join("VibeRoller.lnk");
    for lnk in [PathBuf::from(menu), desk] {
        let ps = format!("$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.WorkingDirectory='{}';$s.IconLocation='{},0';$s.Save()",
            lnk.display().to_string().replace('\'', "''"), exe.display().to_string().replace('\'', "''"), exe.parent().unwrap_or(exe).display().to_string().replace('\'', "''"), exe.display().to_string().replace('\'', "''"));
        use std::os::windows::process::CommandExt;
        let ok = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &ps]).creation_flags(0x0800_0000).status().map(|s| s.success()).unwrap_or(false);
        if !ok { return Err(format!("could not make the shortcut {}", lnk.display())); }
    }
    // the earlier JavaScript AI host's separate start / stop shortcuts: the app's AI host replaces them
    for dir in [PathBuf::from(format!("{appdata}\\Microsoft\\Windows\\Start Menu\\Programs")), home().join("Desktop")] {
        for name in ["Start VibeRoller AI host.lnk", "Stop VibeRoller AI host.lnk", "Start VibeRoller AI Host.lnk", "Stop VibeRoller AI Host.lnk"] { let _ = std::fs::remove_file(dir.join(name)); }
    }
    Ok(())
}
#[cfg(not(windows))]
fn shortcuts(exe: &Path) -> Result<(), String> {
    let apps = home().join(".local").join("share").join("applications");
    let icons = home().join(".local").join("share").join("icons").join("hicolor").join("128x128").join("apps");
    let _ = std::fs::create_dir_all(&apps); let _ = std::fs::create_dir_all(&icons);
    let _ = std::fs::write(icons.join("viberoller.png"), include_bytes!("../icons/128x128.png"));
    std::fs::write(apps.join("viberoller.desktop"), format!("[Desktop Entry]\nType=Application\nName=VibeRoller\nComment=VibeRoller: chat, Work, the board, and this computer's tools\nExec=\"{}\"\nIcon=viberoller\nTerminal=false\nCategories=Network;Chat;Development;\nStartupWMClass=VibeRoller\n", exe.display())).map_err(|e| e.to_string())?;
    let bin = home().join(".local").join("bin"); let _ = std::fs::create_dir_all(&bin);
    let link = bin.join("viberoller"); let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(exe, &link).map_err(|e| e.to_string())?;
    Ok(())
}
