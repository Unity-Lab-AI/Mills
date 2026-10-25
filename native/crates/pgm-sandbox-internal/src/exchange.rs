//! The exchange folder of one copy: `<root>/<id>/in` (mapped to /work/in) and `<root>/<id>/out` (/work/out). Paths from
//! outside are relative to an area and never leave it: absolute paths, `..`, drive letters and symlinks (a container
//! can make a symlink in a folder it writes to) are refused.
use std::fs;
use std::path::{Path, PathBuf};

use crate::Error;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Area { In, Out }
impl Area {
    pub fn dir(self) -> &'static str { match self { Area::In => "in", Area::Out => "out" } }
}

/// An area and the clean components of a path: `in/a/b`, `out/x`, `/work/out/x` name their area; a bare `x` is in
/// `default`. Refuses empty, absolute (outside /work/in|out), `..`, NUL, drive letters, over-long paths.
pub fn parse(path: &str, default: Area) -> Result<(Area, Vec<String>), Error> {
    let bad = |why: &str| Error::new(400, format!("That path is not allowed ({why}): use a path inside /work/in or /work/out."));
    if path.is_empty() || path.len() > 1024 || path.contains('\0') { return Err(bad("empty, too long or NUL")); }
    let mut p = path.replace('\\', "/");
    if let Some(rest) = p.strip_prefix("/work/") { p = rest.to_string(); }
    else if p.starts_with('/') { return Err(bad("absolute")); }
    if p.len() > 1 && p.as_bytes()[1] == b':' { return Err(bad("drive letter")); }
    let mut parts: Vec<String> = vec![];
    for c in p.split('/') {
        match c { "" | "." => {}, ".." => return Err(bad("..")), c => parts.push(c.to_string()) }
    }
    let area = match parts.first().map(String::as_str) { Some("in") => { parts.remove(0); Area::In } Some("out") => { parts.remove(0); Area::Out } _ => default };
    if parts.is_empty() { return Err(bad("no file name")); }
    Ok((area, parts))
}

/// The real path for `parts` under `base`, with no symlink on the way. `create`: make the missing folders.
fn walk(base: &Path, parts: &[String], create: bool) -> Result<PathBuf, Error> {
    let mut cur = base.to_path_buf();
    for (i, c) in parts.iter().enumerate() {
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return Err(Error::new(400, "That path goes through a symbolic link: refused.")),
            Ok(m) => { if i + 1 < parts.len() && !m.is_dir() { return Err(Error::new(400, format!("{c} is a file, not a folder."))); } }
            Err(_) => { if i + 1 < parts.len() && create { fs::create_dir(&cur).map_err(|e| Error::new(500, format!("could not make the folder: {e}")))?; open_up(&cur, true); } }
        }
    }
    // belt and braces: the real location is inside the area
    let root = fs::canonicalize(base).map_err(|e| Error::new(500, format!("exchange folder: {e}")))?;
    let parent = cur.parent().unwrap_or(base);
    if let Ok(real) = fs::canonicalize(parent) { if !real.starts_with(&root) { return Err(Error::new(400, "That path leaves the exchange folder: refused.")); } }
    Ok(cur)
}

/// Let the copy's own user (a sub-uid under rootless Podman) use what we made; the id folder above stays 0700.
#[cfg(unix)]
pub fn open_up(p: &Path, dir: bool) { use std::os::unix::fs::PermissionsExt; let _ = fs::set_permissions(p, fs::Permissions::from_mode(if dir { 0o777 } else { 0o666 })); }
#[cfg(not(unix))]
pub fn open_up(_p: &Path, _dir: bool) {}

pub fn write(base: &Path, parts: &[String], bytes: &[u8]) -> Result<u64, Error> {
    let f = walk(base, parts, true)?;
    if let Ok(m) = fs::symlink_metadata(&f) { if m.is_dir() { return Err(Error::new(400, "That is a folder.")); } }
    fs::write(&f, bytes).map_err(|e| Error::new(500, format!("could not write: {e}")))?;
    open_up(&f, false);
    Ok(bytes.len() as u64)
}

pub fn read(base: &Path, parts: &[String], max: u64) -> Result<Vec<u8>, std::io::Error> {
    let f = walk(base, parts, false).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.msg))?;
    let m = fs::symlink_metadata(&f)?;
    if !m.is_file() { return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "that is not a file")); }
    if m.len() > max { return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("that file is {} bytes; the limit is {max}", m.len()))); }
    fs::read(&f)
}

/// Every regular file under `base` (relative path, size), at most `limit`, no symlinks followed.
pub fn list(base: &Path, limit: usize) -> Vec<(String, u64)> {
    let mut out = vec![];
    fn go(dir: &Path, rel: &str, out: &mut Vec<(String, u64)>, limit: usize, depth: usize) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        let mut es: Vec<_> = rd.flatten().collect(); es.sort_by_key(|e| e.file_name());
        for e in es {
            if out.len() >= limit { return; }
            let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
            let name = format!("{rel}{}", e.file_name().to_string_lossy());
            if m.is_dir() && depth < 8 { go(&e.path(), &format!("{name}/"), out, limit, depth + 1); } else if m.is_file() { out.push((name, m.len())); }
        }
    }
    go(base, "", &mut out, limit, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("pgm-sbx-ex-{n}-{}", std::process::id())); let _ = fs::remove_dir_all(&d); fs::create_dir_all(&d).unwrap(); d }
    #[test]
    fn paths_are_clean_or_refused() {
        assert_eq!(parse("a.bin", Area::In).unwrap(), (Area::In, vec!["a.bin".to_string()]));
        assert_eq!(parse("out/r/x.txt", Area::In).unwrap(), (Area::Out, vec!["r".to_string(), "x.txt".to_string()]));
        assert_eq!(parse("/work/in/a", Area::Out).unwrap(), (Area::In, vec!["a".to_string()]));
        assert_eq!(parse("./a//b", Area::In).unwrap().1, vec!["a".to_string(), "b".to_string()]);
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "in/../../x", "a\\..\\x", "C:\\x", "C:/x", "a\0b", "in", "out/", "/work/../etc"] { assert!(parse(bad, Area::In).is_err(), "{bad:?} must be refused"); }
    }
    #[cfg(unix)]
    #[test]
    fn a_symlink_made_by_the_copy_is_not_followed() {
        let d = tmp("sym"); fs::create_dir_all(d.join("out")).unwrap();
        fs::write(d.join("secret"), "host secret").unwrap();
        std::os::unix::fs::symlink(d.join("secret"), d.join("out/link")).unwrap();
        std::os::unix::fs::symlink(&d, d.join("out/dirlink")).unwrap();
        assert!(read(&d.join("out"), &["link".to_string()], 1000).is_err());
        assert!(read(&d.join("out"), &["dirlink".to_string(), "secret".to_string()], 1000).is_err());
        assert!(write(&d.join("out"), &["dirlink".to_string(), "x".to_string()], b"x").is_err());
        assert!(write(&d.join("out"), &["link".to_string()], b"x").is_err() || fs::read_to_string(d.join("secret")).unwrap() == "host secret");
        assert_eq!(fs::read_to_string(d.join("secret")).unwrap(), "host secret");
        let _ = fs::remove_dir_all(&d);
    }
    #[test]
    fn write_read_list_round_trip() {
        let d = tmp("rt"); fs::create_dir_all(d.join("in")).unwrap();
        assert_eq!(write(&d.join("in"), &["a".to_string(), "b.txt".to_string()], b"hello").unwrap(), 5);
        assert_eq!(read(&d.join("in"), &["a".to_string(), "b.txt".to_string()], 100).unwrap(), b"hello");
        assert!(read(&d.join("in"), &["a".to_string(), "b.txt".to_string()], 3).is_err());
        assert_eq!(list(&d.join("in"), 10), vec![("a/b.txt".to_string(), 5)]);
        let _ = fs::remove_dir_all(&d);
    }
}
