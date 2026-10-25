//! The real Podman, when it is here and has a small image (skips otherwise): start, put, exec, get, stop, nothing left.
use pgm_sandbox_internal::*;

/// SANDBOX_TEST_HOME: a HOME whose container storage works (the hub account's shared HOME has ACLs podman cannot use)
fn env() -> Vec<(String, String)> { std::env::var("SANDBOX_TEST_HOME").map(|h| vec![("HOME".to_string(), h)]).unwrap_or_default() }

#[test]
fn real_podman_round_trip() {
    let cli = Cli::podman().with_env(env());
    let image = "docker.io/library/debian:trixie-slim";
    if cli.available().is_err() || !cli.image_present(image) { eprintln!("skipped: no podman or no local {image}"); return; }
    let root = std::env::temp_dir().join(format!("pgm-sbx-real-{}", std::process::id()));
    let mut cfg = Config::new(&root, image); cfg.name_prefix = "pgm-re-unit-".into();
    let m = Manager::new(Box::new(Cli::podman().with_env(env())), cfg);
    let a = Actor::new("alice", false);
    let s = m.start(&a, "unit test", &StartOpts::default()).unwrap();
    if s.state == State::Gone && std::env::var("SANDBOX_TEST_HOME").is_err() { eprintln!("skipped: podman cannot start containers with this HOME ({:?}); set SANDBOX_TEST_HOME", s.error); return; }
    assert_eq!(s.state, State::Running, "{:?}", s.error);
    let go = || -> Result<()> {
        m.put(&a, &s.id, "hello.txt", b"hi there")?;
        let r = m.exec(&a, &s.id, &Cmd::Shell("cat /work/in/hello.txt > /work/out/copy.txt; echo done; id -u; ping -c1 -W1 127.0.0.1 >/dev/null 2>&1; ip link 2>/dev/null | grep -c '^[0-9]'; grep CapEff /proc/self/status".into()), Some(30))?;
        eprintln!("exec: {:?}", r);
        assert_eq!(r.code, Some(0));
        assert!(r.stdout.starts_with("done"));
        assert_eq!(m.get(&a, &s.id, "copy.txt")?, b"hi there");
        let t = m.exec(&a, &s.id, &Cmd::Shell("sleep 30".into()), Some(1))?;
        assert!(t.timed_out, "{t:?}");
        Ok(())
    };
    let res = go();
    let dir = root.join(&s.id);
    m.stop(&a, &s.id).unwrap();
    assert!(!dir.exists(), "the exchange folder is gone");
    assert!(Cli::podman().with_env(env()).list("pgm-re-unit-").unwrap().is_empty(), "no container left");
    let _ = std::fs::remove_dir_all(&root);
    res.unwrap();
}
