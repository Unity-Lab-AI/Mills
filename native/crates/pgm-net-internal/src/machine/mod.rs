//! The linked-machines library core (team note plans/linked-machines.md): the scope model and its evaluator, the
//! one path canonicaliser, and the safety floor. Pure logic: no hub, no network. Linux and Windows only.
//! - [`scope`]: the scope document, presets, the allow / deny evaluator, time-limited grants, the tool set
//! - [`paths`]: path normalising (Linux and Windows rules, testable anywhere), real-path resolution, checked open
//! - [`device`]: the per-device ed25519 key of a linked machine (sign and verify replies, owner-only key and token files)
//! - [`floor`]: the paths no tool may ever touch, even under `unrestricted`; the default deny list; globs
//! - [`sign`]: the hub signing key, the signed request envelope, the machine-side verifier (T-308)

pub mod audit;
pub mod changes;
pub mod device;
pub mod floor;
pub mod elevate;
pub mod exec;
pub mod jobs;
pub mod kill;
pub mod limits;
pub mod paths;
pub mod runner;
pub mod scope;
pub mod sign;
pub mod transfer;

pub use floor::Floor;
pub use paths::{Flavor, NormPath, PathError};
pub use scope::{Access, Decision, Op, Preset, Scope, ScopeError};
pub use sign::{Actor, Envelope, HubKey, SignedRequest, Verifier, VerifyError};

#[cfg(test)]
mod tests {
    //! Scope + canonicaliser + floor together, on a real temp tree (R340, and the evaluator on real paths).
    use super::paths::Flavor;
    #[cfg(unix)]
    use super::paths::OpenMode;
    use super::scope::{Access, Op, Preset, Scope};
    use super::Floor;
    use std::path::PathBuf;

    const NOW: i64 = 1_790_000_000;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let d = std::env::temp_dir().join(format!("pgm-machine-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Tmp(std::fs::canonicalize(&d).unwrap())
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct World {
        _t: Tmp,
        root: PathBuf,
        state: PathBuf,
        floor: Floor,
    }

    fn world(tag: &str) -> World {
        let t = Tmp::new(tag);
        let root = t.0.join("home");
        let state = t.0.join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(state.join("keys")).unwrap();
        for f in ["token", "scope.json", "machine-audit.jsonl", "machine-audit.jsonl.1", "keys/device.key"] {
            std::fs::write(state.join(f), "secret").unwrap();
        }
        let floor = Floor::new(state.join("token"), state.join("keys"), state.join("scope.json"), state.join("machine-audit.jsonl")).with_secret(b"tok-SECRET-1");
        World { root, state, floor, _t: t }
    }

    #[test]
    fn floor_holds_under_unrestricted_for_every_op() {
        let w = world("floor");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(w.state.join("token"), w.root.join("innocent-link")).unwrap();
            std::os::unix::fs::symlink(&w.state, w.root.join("state-link")).unwrap();
        }
        let s = Scope::preset(Preset::Unrestricted, &[], &[]).with_floor(w.floor.clone());
        let protected = ["token", "scope.json", "machine-audit.jsonl", "machine-audit.jsonl.1", "keys", "keys/device.key", "keys/newkey"];
        for rel in protected {
            let abs = w.state.join(rel);
            for op in [Op::Read, Op::Write, Op::Delete, Op::List, Op::Exec, Op::Shell] {
                let d = s.allow(op, abs.to_str().unwrap(), NOW);
                assert!(!d.allow, "{rel} {}: {}", op.name(), d.reason);
                assert!(d.reason.contains("safety floor"));
                // and through the canonicaliser, typed with a .. in it
                let rel_typed = format!("{}/../state/{rel}", w.root.display());
                assert!(s.authorize(op, &w.root, &rel_typed, NOW).is_err(), "{rel_typed} {}", op.name());
            }
        }
        // the neighbours are reachable under unrestricted
        for op in [Op::Read, Op::Write, Op::List] {
            assert!(s.authorize(op, &w.root, "fine.txt", NOW).is_ok());
        }
        assert!(s.authorize(Op::List, &w.root, w.state.to_str().unwrap(), NOW).is_ok(), "the state folder itself is not a floor entry, only what is in it");
        #[cfg(unix)]
        {
            // through links: the floor sees the real path
            for op in [Op::Read, Op::Write, Op::Delete] {
                assert!(s.authorize(op, &w.root, "innocent-link", NOW).is_err(), "{}", op.name());
                assert!(s.authorize(op, &w.root, "state-link/token", NOW).is_err());
                assert!(s.authorize(op, &w.root, "state-link/keys/device.key", NOW).is_err());
            }
            assert!(s.authorize(Op::List, &w.root, "state-link/keys", NOW).is_err());
            assert!(s.open(Op::Read, &w.root, "innocent-link", OpenMode::Read, NOW).is_err());
        }
        // every other preset also keeps the floor
        for p in [Preset::Full, Preset::FullRo, Preset::DenyList, Preset::NoShell, Preset::NoNetwork, Preset::Custom] {
            let s = Scope::preset(p, &[("*", Access::Rw)], &[]).with_floor(w.floor.clone());
            assert!(!s.allow(Op::Read, w.state.join("token").to_str().unwrap(), NOW).allow, "{p:?}");
            assert!(!s.allow(Op::Read, w.state.join("keys/device.key").to_str().unwrap(), NOW).allow, "{p:?}");
        }
    }

    #[test]
    fn fetch_never_sends_the_floor() {
        let w = world("net");
        let tok = w.state.join("token");
        let key = w.state.join("keys/device.key");
        for preset in [Preset::Unrestricted, Preset::Full] {
            let mut s = Scope::preset(preset, &[], &[]).with_floor(w.floor.clone());
            // fetch on (full has it off by default)
            if preset == Preset::Full {
                s = Scope::try_parse(r#"{"preset":"full","tools":{"network":{"fetch":true}}}"#).unwrap().with_floor(w.floor.clone());
            }
            assert!(s.check_net(&[], b"GET /x", NOW).allow, "{preset:?}");
            assert!(!s.check_net(&[tok.to_str().unwrap()], b"", NOW).allow);
            assert!(!s.check_net(&[key.to_str().unwrap()], b"", NOW).allow);
            assert!(!s.check_net(&[], b"Authorization: tok-SECRET-1", NOW).allow);
            assert!(s.check_net(&[w.root.join("ok.txt").to_str().unwrap()], b"", NOW).allow);
        }
        // fetch off: refused whatever it sends
        let s = Scope::preset(Preset::Folders, &[(w.root.to_str().unwrap(), Access::Rw)], &[]).with_floor(w.floor.clone());
        assert!(!s.check_net(&[], b"", NOW).allow);
    }

    #[test]
    fn default_deny_list_on_real_files() {
        let w = world("deny");
        for f in [".env", "a.pem", "id_rsa", "hub.json", "plain.txt"] {
            std::fs::write(w.root.join(f), "x").unwrap();
        }
        std::fs::create_dir_all(w.root.join(".ssh")).unwrap();
        std::fs::write(w.root.join(".ssh/config"), "x").unwrap();
        let s = Scope::preset(Preset::Full, &[], &[]).with_home(w.root.to_str().unwrap());
        for (f, want) in [(".env", false), ("a.pem", false), ("id_rsa", false), ("hub.json", false), (".ssh/config", false), ("plain.txt", true)] {
            assert_eq!(s.authorize(Op::Read, &w.root, f, NOW).is_ok(), want, "{f}");
        }
        // a link with an innocent name to a denied file is denied by its real path
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(w.root.join(".env"), w.root.join("notes.txt")).unwrap();
            assert!(s.authorize(Op::Read, &w.root, "notes.txt", NOW).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn scope_checks_real_paths_and_open_is_link_safe() {
        use std::io::Write;
        use std::os::unix::fs::symlink;
        let w = world("real");
        let out = w.root.parent().unwrap().join("outside");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("victim"), "keep").unwrap();
        std::fs::create_dir_all(w.root.join("p/sub")).unwrap();
        std::fs::create_dir_all(w.root.join("q")).unwrap();
        symlink(&out, w.root.join("p/escape")).unwrap();
        symlink(w.root.join("q"), w.root.join("p/to-q")).unwrap();
        let p = w.root.join("p");
        let s = Scope::preset(Preset::Folder, &[(p.to_str().unwrap(), Access::Rw)], &[]);
        assert!(s.authorize(Op::Write, &p, "sub/new.txt", NOW).is_ok());
        assert!(s.authorize(Op::Read, &p, "escape/victim", NOW).is_err(), "link out of the folder");
        assert!(s.authorize(Op::Read, &p, "to-q/x", NOW).is_err(), "link to a sibling outside the folder");
        assert!(s.authorize(Op::Read, &p, "../q/x", NOW).is_err());
        assert!(s.authorize(Op::Read, &p, "/etc/passwd", NOW).is_err());
        assert!(s.authorize(Op::Write, &p, "", NOW).is_err());
        // followLinks: true on the entry lets the typed path through; deny rules still look at the real path
        let j = format!(r#"{{"paths":[{{"path":{:?},"access":"rw","followLinks":true}}],"deny":[{{"path":{:?}}}]}}"#, p.to_str().unwrap(), out.join("blocked").to_str().unwrap());
        let f = Scope::try_parse(&j).unwrap();
        assert!(f.authorize(Op::Read, &p, "escape/victim", NOW).is_ok());
        std::fs::create_dir_all(out.join("blocked")).unwrap();
        assert!(f.authorize(Op::Read, &p, "escape/blocked/x", NOW).is_err());
        // a ro folder refuses the write before any open
        let ro = Scope::preset(Preset::Folders, &[(p.to_str().unwrap(), Access::Ro)], &[]);
        assert!(ro.open(Op::Write, &p, "sub/a", OpenMode::WriteTruncate, NOW).is_err());
        assert!(!p.join("sub/a").exists());
        // the swapped-link TOCTOU: approved first, a link swapped in afterwards
        let r = s.authorize(Op::Write, &p, "sub/swap.txt", NOW).unwrap();
        symlink(out.join("victim"), p.join("sub/swap.txt")).unwrap();
        let v = |_: &std::path::Path| true;
        assert!(super::paths::open_verified(&r, &v, OpenMode::WriteTruncate).is_err());
        assert!(s.open(Op::Write, &p, "sub/swap.txt", OpenMode::WriteTruncate, NOW).is_err(), "a link there now: authorize already refuses");
        assert_eq!(std::fs::read_to_string(out.join("victim")).unwrap(), "keep");
        let mut f = s.open(Op::Write, &p, "sub/ok.txt", OpenMode::WriteTruncate, NOW).unwrap();
        f.write_all(b"ok").unwrap();
        assert_eq!(std::fs::read_to_string(p.join("sub/ok.txt")).unwrap(), "ok");
    }

    #[test]
    fn windows_floor_with_unrestricted_on_any_host() {
        let floor = Floor::with_flavor(Flavor::Windows, &["C:\\People\\Me\\AppData\\Local\\VibeRoller\\machine\\token", "C:\\People\\Me\\AppData\\Local\\VibeRoller\\machine\\keys"]);
        let s = Scope::preset_with(Flavor::Windows, Preset::Unrestricted, &[], &[]).with_floor(floor);
        for p in [
            "C:\\People\\Me\\AppData\\Local\\VibeRoller\\machine\\token",
            "c:/people/me/appdata/local/viberoller/MACHINE/Token",
            "\\\\?\\C:\\People\\Me\\AppData\\Local\\VibeRoller\\machine\\keys\\device.key",
            "C:\\People\\Me\\AppData\\Local\\VibeRoller\\machine\\..\\machine\\token",
        ] {
            for op in [Op::Read, Op::Write, Op::Delete, Op::List] {
                assert!(!s.allow(op, p, NOW).allow, "{p} {}", op.name());
            }
        }
        assert!(s.allow(Op::Write, "C:\\People\\Me\\Documents\\a.txt", NOW).allow);
        assert!(s.allow(Op::Read, "C:\\People\\Me\\.ssh\\id_rsa", NOW).allow, "unrestricted lifts the default deny list");
    }
}
