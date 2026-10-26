//! The safety floor (linked-machines.md 5.2, 7.6, R340) and the default deny list.
//!
//! The floor is the machine's own state: the token file, the device key, the scope file and the audit log. No tool
//! may read, write, delete or list them, under ANY scope including `unrestricted`, so a run can neither raise its own
//! scope nor read the device key. A net / `fetch` call never sends them (a file as a body, or a known secret value in
//! the payload). The floor is checked on the lexical path AND the real path (so a symlink to the token file is
//! caught), by the evaluator before anything else.
//!
//! Limits, said plainly: a hard link made by someone else to a floor file is a different name for the same inode and
//! is not detected (the state dir is 0700 / owner-only ACL, so only the owner can make one); a shell command can name
//! a floor path (the shell is advisory, 5.5), which is why the state dir is also owner-only on disk.

use super::paths::{parse_abs, real_of, Flavor, NormPath};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Floor {
    flavor: Flavor,
    entries: Vec<NormPath>,
    secrets: Vec<Vec<u8>>,
}

impl Floor {
    /// The four protected paths of this machine (native flavor; the real form of each is protected too).
    pub fn new(
        token_file: impl AsRef<Path>,
        device_key: impl AsRef<Path>,
        scope_file: impl AsRef<Path>,
        audit_log: impl AsRef<Path>,
    ) -> Floor {
        let mut f = Floor::empty();
        for p in [token_file.as_ref(), device_key.as_ref(), scope_file.as_ref(), audit_log.as_ref()] {
            f.add_native(p);
        }
        f
    }

    /// Nothing protected (a scope parsed on its own, before the machine gives its floor).
    pub fn empty() -> Floor {
        Floor { flavor: Flavor::native(), entries: vec![], secrets: vec![] }
    }

    /// Pure constructor for tests and for the Windows rules on Linux: paths are parsed under `flavor`, no file
    /// system is asked.
    pub fn with_flavor(flavor: Flavor, paths: &[&str]) -> Floor {
        let mut f = Floor { flavor, entries: vec![], secrets: vec![] };
        for p in paths {
            if let Ok(n) = parse_abs(flavor, p) {
                f.entries.push(n);
            }
        }
        f
    }

    fn add_native(&mut self, p: &Path) {
        if let Some(s) = p.to_str() {
            if let Ok(n) = parse_abs(self.flavor, s) {
                if let Ok((real, _)) = real_of(&n) {
                    if real != n {
                        self.entries.push(real);
                    }
                }
                self.entries.push(n);
            }
        }
    }

    /// One more protected path (a directory protects everything below it).
    pub fn with_extra(mut self, p: impl AsRef<Path>) -> Floor {
        self.add_native(p.as_ref());
        self
    }

    /// A secret value (the device token itself) that a net payload must never contain.
    pub fn with_secret(mut self, secret: &[u8]) -> Floor {
        if !secret.is_empty() {
            self.secrets.push(secret.to_vec());
        }
        self
    }

    pub fn flavor(&self) -> Flavor {
        self.flavor
    }

    /// Is this (already normalised) path a floor path, below one, or a rotated sibling of one (`audit.jsonl.1`)?
    pub fn protects(&self, p: &NormPath) -> bool {
        self.entries.iter().any(|e| {
            if p.starts_with(e) {
                return true;
            }
            match (p.parent(), e.parent(), e.file_name(), p.file_name()) {
                (Some(pp), Some(ep), Some(en), Some(pn)) if pp == ep => {
                    let (pn, en) = if self.flavor == Flavor::Windows { (pn.to_lowercase(), en.to_lowercase()) } else { (pn.to_string(), en.to_string()) };
                    pn.len() > en.len() && pn.starts_with(&en) && pn.as_bytes()[en.len()] == b'.'
                }
                _ => false,
            }
        })
    }

    /// May this local file be sent by a net op (as a body, an upload, a header file)?
    pub fn file_sendable(&self, p: &NormPath) -> bool {
        !self.protects(p)
    }

    /// May this outgoing payload (URL, headers, body) be sent? Refused when it carries a registered secret.
    pub fn payload_sendable(&self, bytes: &[u8]) -> bool {
        !self.secrets.iter().any(|s| bytes.windows(s.len()).any(|w| w == s.as_slice()))
    }
}

/// The default deny globs for `full`, `folders` and their variants (7.6). `unrestricted` and `custom` do not use them.
pub fn default_deny_globs() -> Vec<&'static str> {
    vec![
        "**/.env",
        "**/.env.*",
        "**/*.pem",
        "**/id_*",
        "**/*.kdbx",
        "**/hub.json",
        "**/.ssh",
        "**/.ssh/**",
        "**/.aws",
        "**/.aws/**",
        "**/.gnupg",
        "**/.gnupg/**",
    ]
}

/// The default deny paths that depend on the person's home (`~/.ssh`, the OS credential stores, browser profiles).
pub fn default_deny_paths(flavor: Flavor, home: &str) -> Vec<String> {
    let sep = if flavor == Flavor::Windows { "\\" } else { "/" };
    let h = home.trim_end_matches(['/', '\\']);
    let j = |rest: &str| format!("{h}{sep}{}", rest.replace('/', sep));
    match flavor {
        Flavor::Posix => vec![
            j(".ssh"),
            j(".aws"),
            j(".gnupg"),
            j(".local/share/keyrings"),
            j(".mozilla"),
            j(".config/google-chrome"),
            j(".config/chromium"),
        ],
        Flavor::Windows => vec![
            j(".ssh"),
            j(".aws"),
            j(".gnupg"),
            j("AppData/Roaming/Microsoft/Credentials"),
            j("AppData/Local/Microsoft/Credentials"),
            j("AppData/Local/Google/Chrome/User Data"),
            j("AppData/Roaming/Mozilla"),
        ],
    }
}

/// Glob match over path components, from the root. `**` = any number of components (also none), `*` = any run of
/// characters inside one component, `?` = one character. Windows matches without case. A pattern starting with `/`
/// or a drive is anchored at the root as written; every other pattern is meant to start with `**/` to float.
pub fn glob_matches(flavor: Flavor, pattern: &str, path: &NormPath) -> bool {
    let pat = if flavor == Flavor::Windows { pattern.replace('\\', "/").to_lowercase() } else { pattern.to_string() };
    let pcomps: Vec<&str> = pat.split('/').filter(|c| !c.is_empty()).collect();
    let mut comps = path.keyed_parts();
    // an anchored Windows pattern may start with the drive: `c:/people/**`
    if flavor == Flavor::Windows {
        if let Some(first) = pcomps.first() {
            if first.len() == 2 && first.ends_with(':') {
                comps.insert(0, path.prefix().to_lowercase());
            }
        }
    }
    glob_components(&pcomps, &comps.iter().map(|s| s.as_str()).collect::<Vec<_>>())
}

fn glob_components(p: &[&str], c: &[&str]) -> bool {
    match p.split_first() {
        None => c.is_empty(),
        Some((&"**", rest)) => (0..=c.len()).any(|i| glob_components(rest, &c[i..])),
        Some((first, rest)) => match c.split_first() {
            Some((cf, crest)) => glob_one(first.as_bytes(), cf.as_bytes()) && glob_components(rest, crest),
            None => false,
        },
    }
}

fn glob_one(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| glob_one(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && glob_one(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && glob_one(rest, &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn np(f: Flavor, s: &str) -> NormPath {
        parse_abs(f, s).unwrap()
    }

    #[test]
    fn glob_table() {
        let l = Flavor::Posix;
        let w = Flavor::Windows;
        let rows: &[(Flavor, &str, &str, bool)] = &[
            (l, "**/.env", "/a/b/.env", true),
            (l, "**/.env", "/.env", true),
            (l, "**/.env", "/a/.envx", false),
            (l, "**/.env.*", "/a/.env.local", true),
            (l, "**/*.pem", "/x/y/cert.pem", true),
            (l, "**/*.pem", "/x/y/cert.pem.txt", false),
            (l, "**/id_*", "/home/u/.ssh/id_rsa", true),
            (l, "**/id_*", "/home/u/.ssh/id_ed25519.pub", true),
            (l, "**/id_*", "/home/u/valid_id", false),
            (l, "**/hub.json", "/p/hub.json", true),
            (l, "**/.ssh/**", "/home/u/.ssh/config", true),
            (l, "**/.ssh/**", "/home/u/.ssh", true),
            (l, "**/.ssh", "/home/u/.ssh", true),
            (l, "/home/u/*", "/home/u/a", true),
            (l, "/home/u/*", "/home/u/a/b", false),
            (l, "/home/u/**", "/home/u/a/b", true),
            (l, "**/a?c", "/x/abc", true),
            (l, "**/a?c", "/x/ac", false),
            (l, "**/.ENV", "/x/.env", false),
            (w, "**/.env", "C:\\Proj\\.ENV", true),
            (w, "**\\*.PEM", "d:\\k\\Cert.pem", true),
            (w, "C:\\People\\**", "c:/people/x/y", true),
            (w, "C:\\People\\**", "D:\\Users\\x", false),
        ];
        for (f, pat, path, want) in rows {
            assert_eq!(glob_matches(*f, pat, &np(*f, path)), *want, "{pat} vs {path}");
        }
    }

    #[test]
    fn floor_protects_files_dirs_and_rotations() {
        let l = Flavor::Posix;
        let f = Floor::with_flavor(l, &["/s/token", "/s/key/", "/s/scope.json", "/s/machine-audit.jsonl"]);
        for (p, want) in [
            ("/s/token", true),
            ("/s/key", true),
            ("/s/key/inner", true),
            ("/s/scope.json", true),
            ("/s/machine-audit.jsonl", true),
            ("/s/machine-audit.jsonl.1", true),
            ("/s/machine-audit.jsonlx", false),
            ("/s/tokenx", false),
            ("/s", false),
            ("/s/other", false),
            ("/t/token", false),
            ("/s/a/../token", true),
        ] {
            assert_eq!(f.protects(&np(l, p)), want, "{p}");
        }
        let w = Floor::with_flavor(Flavor::Windows, &["C:\\People\\Me\\AppData\\Local\\VibeRoller\\machine\\token"]);
        assert!(w.protects(&np(Flavor::Windows, "c:/people/me/appdata/local/viberoller/MACHINE/TOKEN")));
        assert!(w.protects(&np(Flavor::Windows, "\\\\?\\C:\\People\\Me\\AppData\\Local\\VibeRoller\\machine\\token")));
        assert!(!w.protects(&np(Flavor::Windows, "D:\\Users\\Me\\AppData\\Local\\VibeRoller\\machine\\token")));
    }

    #[test]
    fn payload_secrets() {
        let f = Floor::empty().with_secret(b"tok-123");
        assert!(f.payload_sendable(b"hello"));
        assert!(!f.payload_sendable(b"Authorization: Bearer tok-123"));
        assert!(Floor::empty().payload_sendable(b"anything"));
    }

    #[test]
    fn default_lists() {
        let g = default_deny_globs();
        for want in ["**/.env", "**/*.pem", "**/id_*", "**/hub.json", "**/.ssh/**"] {
            assert!(g.contains(&want), "{want}");
        }
        assert!(default_deny_paths(Flavor::Posix, "/home/u").contains(&"/home/u/.ssh".to_string()));
        assert!(default_deny_paths(Flavor::Windows, "C:\\People\\u").contains(&"C:\\People\\u\\.ssh".to_string()));
    }
}
