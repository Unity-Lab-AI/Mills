//! Making a program runnable by its name from any new terminal, the same on Windows and Linux, and idempotent:
//!   Linux / macOS: a link `~/.local/bin/<name>` to the program (a stale link or one to an older copy is replaced; a regular
//!                  file that is not ours never is), and, when `~/.local/bin` is not on the PATH, ONE marked block in
//!                  `~/.profile` (and `~/.zprofile` / `~/.bash_profile` when those exist) that puts it there
//!   Windows:       the program's folder added to the USER Path (HKCU\Environment, through PowerShell's
//!                  [Environment]::SetEnvironmentVariable, which also tells running programs); the machine Path is never touched
use std::path::{Path, PathBuf};

/// The comment that marks the block this module appends to a shell profile (found again, so it is never written twice).
pub const MARKER: &str = "# added by VibeRoller AI Host (vr-host)";

/// Where `name` is found on the PATH now, if anywhere.
pub fn on_path(name: &str) -> Option<PathBuf> { crate::proc::which(name) }

fn trim_dir(s: &str) -> &str { s.trim().trim_end_matches(|c| c == '\\' || c == '/') }
/// Does a PATH-style list (`;` on Windows, `:` elsewhere) hold `dir`? Windows: case-insensitive; a trailing slash or
/// backslash never matters.
pub fn path_list_contains(list: &str, dir: &str, windows: bool) -> bool {
    let want = trim_dir(dir);
    if want.is_empty() { return false; }
    list.split(if windows { ';' } else { ':' }).map(trim_dir).filter(|e| !e.is_empty())
        .any(|e| if windows { e.eq_ignore_ascii_case(want) } else { e == want })
}
/// Does this profile text already hold our block?
pub fn profile_block_present(text: &str) -> bool { text.lines().any(|l| l.trim() == MARKER) }
/// The block appended to a profile (starts with a blank line so it never glues onto the last line).
pub fn profile_block() -> String {
    format!("\n{MARKER}\ncase \":$PATH:\" in *\":$HOME/.local/bin:\"*) ;; *) export PATH=\"$HOME/.local/bin:$PATH\" ;; esac\n")
}

/// What is at the link path.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum LinkState { Missing, PointsAtProgram, PointsElsewhere, NotOurs }
/// What to do about it.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum LinkAction { Keep, Create, Replace, Refuse }
/// The decision: nothing there → make the link; ours already → keep; a link to something else (stale, an older copy) →
/// replace; a regular file or folder → leave it alone and say so.
pub fn link_decision(s: LinkState) -> LinkAction {
    match s { LinkState::Missing => LinkAction::Create, LinkState::PointsAtProgram => LinkAction::Keep, LinkState::PointsElsewhere => LinkAction::Replace, LinkState::NotOurs => LinkAction::Refuse }
}
fn same_file(a: &Path, b: &Path) -> bool { a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y) }
/// Looks at the link path (does not follow a regular file; a symlink is compared with `exe`).
pub fn link_state(link: &Path, exe: &Path) -> LinkState {
    match std::fs::symlink_metadata(link) {
        Err(_) => LinkState::Missing,
        Ok(m) if m.file_type().is_symlink() => match std::fs::read_link(link) {
            Ok(t) => { let t = if t.is_absolute() { t } else { link.parent().unwrap_or(Path::new(".")).join(t) }; if same_file(&t, exe) { LinkState::PointsAtProgram } else { LinkState::PointsElsewhere } }
            Err(_) => LinkState::PointsElsewhere,
        },
        Ok(_) => LinkState::NotOurs,
    }
}

/// Makes `name` run from any new terminal. Returns one line saying what was done ("already on PATH …", "linked …",
/// "added … to your user PATH …"). Safe to call again and again.
pub fn ensure_on_path(exe: &Path, name: &str) -> Result<String, String> {
    if let Some(found) = on_path(name) { if same_file(&found, exe) { return Ok(format!("already on PATH ({})", found.display())); } }
    ensure_on_path_in(&crate::folders::home(), exe, name)
}
/// The same with the home folder given (tests use a scratch one); the PATH is the process's own.
pub fn ensure_on_path_in(home: &Path, exe: &Path, name: &str) -> Result<String, String> {
    ensure_on_path_with(home, exe, name, &std::env::var("PATH").unwrap_or_default())
}
/// The same with the PATH text given too.
pub fn ensure_on_path_with(home: &Path, exe: &Path, name: &str, path_env: &str) -> Result<String, String> {
    if crate::WIN { return windows_path(exe); }
    let bin = home.join(".local").join("bin");
    let link = bin.join(name);
    let mut said = vec![];
    match link_decision(link_state(&link, exe)) {
        LinkAction::Keep => {}
        LinkAction::Refuse => return Err(format!("{} is a file that is not ours: left alone (move it away to put {name} on your PATH)", link.display())),
        a => {
            std::fs::create_dir_all(&bin).map_err(|e| format!("could not make {}: {e}", bin.display()))?;
            if a == LinkAction::Replace { let _ = std::fs::remove_file(&link); }
            make_link(exe, &link).map_err(|e| format!("could not link {}: {e}", link.display()))?;
            said.push(format!("linked {} → {}", link.display(), exe.display()));
        }
    }
    let bin_s = bin.display().to_string();
    if !path_list_contains(path_env, &bin_s, false) {
        let files = profile_files(home);
        let mut touched = vec![];
        for p in files {
            let old = std::fs::read_to_string(&p).unwrap_or_default();
            if profile_block_present(&old) { continue; }
            let mut text = old; text.push_str(&profile_block());
            std::fs::write(&p, text).map_err(|e| format!("could not write {}: {e}", p.display()))?;
            touched.push(p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        }
        if !touched.is_empty() { said.push(format!("added ~/.local/bin to your PATH in ~/{} (new sign-ins have it; in this terminal: export PATH=\"$HOME/.local/bin:$PATH\")", touched.join(", ~/"))); }
    }
    Ok(if said.is_empty() { format!("already on PATH ({})", link.display()) } else { said.join("; ") })
}
/// ~/.profile always; ~/.zprofile and ~/.bash_profile (bash reads that INSTEAD of .profile when it exists) when they exist.
fn profile_files(home: &Path) -> Vec<PathBuf> {
    let mut v = vec![home.join(".profile")];
    for n in [".bash_profile", ".zprofile"] { if home.join(n).exists() { v.push(home.join(n)); } }
    v
}
#[cfg(unix)]
fn make_link(exe: &Path, link: &Path) -> std::io::Result<()> { std::os::unix::fs::symlink(exe, link) }
#[cfg(not(unix))]
fn make_link(_exe: &Path, _link: &Path) -> std::io::Result<()> { Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no links here")) }

/// Is `name` runnable from a new terminal? (yes / no, and the words for it)
pub fn path_status(exe: &Path, name: &str) -> (bool, String) {
    path_status_in(&crate::folders::home(), exe, name, &std::env::var("PATH").unwrap_or_default(), on_path(name))
}
/// The same with the home folder, the PATH text and where the name is found now given.
pub fn path_status_in(home: &Path, exe: &Path, name: &str, path_env: &str, found: Option<PathBuf>) -> (bool, String) {
    if let Some(p) = &found {
        return if same_file(p, exe) { (true, format!("{name} runs from any terminal ({})", p.display())) } else { (false, format!("{name} on your PATH is {}, not {}", p.display(), exe.display())) };
    }
    if crate::WIN {
        let user = reg_path_value(&crate::proc::run("reg", &["query", r"HKCU\Environment", "/v", "Path"]).1);
        let dir = exe.parent().map(|d| d.display().to_string()).unwrap_or_default();
        return if path_list_contains(&user, &dir, true) { (true, format!("{dir} is in your user PATH (new terminals find {name})")) } else { (false, format!("{name} is not on your PATH")) };
    }
    let link = home.join(".local").join("bin").join(name);
    let in_profile = profile_files(home).iter().any(|p| profile_block_present(&std::fs::read_to_string(p).unwrap_or_default()));
    let _ = path_env;
    if link_state(&link, exe) == LinkState::PointsAtProgram && in_profile { (true, format!("{name} is linked in ~/.local/bin and your profile puts that on the PATH (new sign-ins have it)")) }
    else { (false, format!("{name} is not on your PATH")) }
}
/// The data of a `reg query … /v Path` answer ("    Path    REG_EXPAND_SZ    C:\a;C:\b").
pub fn reg_path_value(out: &str) -> String {
    for l in out.lines() { if let Some(i) = l.find("REG_") { let rest = &l[i..]; return rest.split_once(char::is_whitespace).map(|x| x.1.trim().to_string()).unwrap_or_default(); } }
    String::new()
}
/// The PowerShell that adds `dir` to the user Path unless it is there (case- and trailing-backslash-insensitive); it
/// prints "present" or "added".
pub fn windows_script(dir: &str) -> String {
    let d = trim_dir(dir).replace('\'', "''");
    format!("$d = '{d}'; $p = [Environment]::GetEnvironmentVariable('Path','User'); if ($null -eq $p) {{ $p = '' }}; \
$parts = @($p -split ';' | Where-Object {{ $_ -ne '' }}); $n = $d.TrimEnd('\\','/').ToLowerInvariant(); \
if (@($parts | ForEach-Object {{ $_.TrimEnd('\\','/').ToLowerInvariant() }}) -contains $n) {{ 'present' }} \
else {{ [Environment]::SetEnvironmentVariable('Path', ((@($parts) + $d) -join ';'), 'User'); 'added' }}")
}
fn windows_path(exe: &Path) -> Result<String, String> {
    let dir = exe.parent().ok_or("the program has no folder")?.display().to_string();
    if !crate::WIN { return Err("not Windows".into()); }
    let (ok, out) = crate::proc::run("powershell", &["-NoProfile", "-NonInteractive", "-Command", &windows_script(&dir)]);
    let out = out.trim();
    if ok && out.ends_with("present") { Ok(format!("already on PATH ({dir} is in your user PATH)")) }
    else if ok && out.ends_with("added") { Ok(format!("added {dir} to your user PATH; new terminals have it")) }
    else { Err(format!("could not change your user PATH: {}", out.lines().last().unwrap_or(""))) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("pgm-path-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    #[test]
    fn path_lists() {
        assert!(path_list_contains("/usr/bin:/home/a/.local/bin/:/bin", "/home/a/.local/bin", false));
        assert!(!path_list_contains("/usr/bin:/bin", "/home/a/.local/bin", false));
        assert!(!path_list_contains("/usr/bin:/HOME/A/.local/bin", "/home/a/.local/bin", false));
        assert!(path_list_contains(r"C:\Windows;D:\Data\A\AppData\Local\VibeRoller\AIHost\;C:\x", r"d:\data\a\appdata\local\viberoller\aihost", true));
        assert!(!path_list_contains(r"C:\Windows;C:\x", r"C:\y", true));
        assert!(!path_list_contains("", "/a", false) && !path_list_contains("/a", "", false));
    }
    #[test]
    fn profile_blocks() {
        assert!(!profile_block_present("export A=1\n"));
        assert!(profile_block_present(&format!("export A=1\n{}", profile_block())));
        assert!(profile_block().starts_with('\n') && profile_block().contains(".local/bin"));
    }
    #[test]
    fn link_decisions() {
        assert_eq!(link_decision(LinkState::Missing), LinkAction::Create);
        assert_eq!(link_decision(LinkState::PointsAtProgram), LinkAction::Keep);
        assert_eq!(link_decision(LinkState::PointsElsewhere), LinkAction::Replace);
        assert_eq!(link_decision(LinkState::NotOurs), LinkAction::Refuse);
    }
    #[test]
    fn reads_the_registry_answer() {
        assert_eq!(reg_path_value("\nHKEY_CURRENT_USER\\Environment\n    Path    REG_EXPAND_SZ    C:\\a;C:\\b c\n"), "C:\\a;C:\\b c");
        assert_eq!(reg_path_value("ERROR: not found"), "");
    }
    #[cfg(unix)]
    #[test]
    fn says_whether_it_is_set_up() {
        let home = scratch("status");
        let exe = home.join("vr-host"); std::fs::write(&exe, "x").unwrap();
        assert!(!path_status_in(&home, &exe, "vr-host", "/usr/bin", None).0);
        ensure_on_path_with(&home, &exe, "vr-host", "/usr/bin").unwrap();
        assert!(path_status_in(&home, &exe, "vr-host", "/usr/bin", None).0);
        assert!(path_status_in(&home, &exe, "vr-host", "/usr/bin", Some(exe.clone())).0);
        assert!(!path_status_in(&home, &exe, "vr-host", "/usr/bin", Some(PathBuf::from("/bin/sh"))).0);
        let _ = std::fs::remove_dir_all(&home);
    }
    #[test]
    fn the_windows_script_dedupes_and_quotes() {
        let s = windows_script(r"D:\Data\O'Neil\AIHost\");
        assert!(s.contains(r"'D:\Data\O''Neil\AIHost'") && s.contains("'User'") && s.contains("ToLowerInvariant"));
        assert!(!s.contains("Machine"));
    }
    #[cfg(unix)]
    #[test]
    fn links_and_the_profile_block_once() {
        let home = scratch("home");
        let exe = home.join("ai-host").join("vr-host"); std::fs::create_dir_all(exe.parent().unwrap()).unwrap(); std::fs::write(&exe, "x").unwrap();
        let other = home.join("other-vr-host"); std::fs::write(&other, "y").unwrap();
        let m1 = ensure_on_path_with(&home, &exe, "vr-host", "/usr/bin").unwrap();
        assert!(m1.contains("linked") && m1.contains(".profile"), "{m1}");
        let link = home.join(".local").join("bin").join("vr-host");
        assert_eq!(std::fs::read_link(&link).unwrap(), exe);
        let m2 = ensure_on_path_with(&home, &exe, "vr-host", "/usr/bin").unwrap();
        assert!(m2.starts_with("already on PATH"), "{m2}");
        assert_eq!(std::fs::read_to_string(home.join(".profile")).unwrap().matches(MARKER).count(), 1);
        assert!(!home.join(".zprofile").exists());
        // on the PATH already: no block in a profile that has none
        let home2 = scratch("home2");
        let m3 = ensure_on_path_with(&home2, &exe, "vr-host", &format!("/usr/bin:{}", home2.join(".local").join("bin").display())).unwrap();
        assert!(m3.contains("linked") && !m3.contains("profile") && !home2.join(".profile").exists(), "{m3}");
        // an older link pointing elsewhere is replaced; a zprofile that exists gets the block too
        std::fs::remove_file(&link).unwrap(); std::os::unix::fs::symlink(&other, &link).unwrap();
        std::fs::write(home.join(".zprofile"), "# mine\n").unwrap();
        let m4 = ensure_on_path_with(&home, &exe, "vr-host", "/usr/bin").unwrap();
        assert!(m4.contains("linked"), "{m4}");
        assert_eq!(std::fs::read_link(&link).unwrap(), exe);
        assert_eq!(std::fs::read_to_string(home.join(".zprofile")).unwrap().matches(MARKER).count(), 1);
        assert_eq!(std::fs::read_to_string(home.join(".profile")).unwrap().matches(MARKER).count(), 1);
        // a regular file that is not ours stays
        std::fs::remove_file(&link).unwrap(); std::fs::write(&link, "not ours").unwrap();
        assert!(ensure_on_path_with(&home, &exe, "vr-host", "/usr/bin").unwrap_err().contains("not ours"));
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "not ours");
        let _ = std::fs::remove_dir_all(&home); let _ = std::fs::remove_dir_all(&home2);
    }
}
