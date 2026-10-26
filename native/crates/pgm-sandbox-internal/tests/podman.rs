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

/// The REAL base image (2.25 GB), by hand only: `SANDBOX_REAL_IMAGE=1 SANDBOX_ENGINE_ARGS="--root R --runroot RR" cargo test -p
/// pgm-sandbox-internal --test podman real_rea -- --nocapture` (SANDBOX_REAL_TOOL picks the cheap tool to call; default list_adb_devices (a cheap REA tool; no
/// arguments). Starts a copy, lists REA's tools, reads the skill, calls one tool, stops.
#[test]
fn real_rea_end_to_end() {
    if std::env::var("SANDBOX_REAL_IMAGE").is_err() { eprintln!("skipped: set SANDBOX_REAL_IMAGE=1 to run against localhost/pgm-re-base"); return; }
    let args: Vec<String> = std::env::var("SANDBOX_ENGINE_ARGS").unwrap_or_default().split_whitespace().map(String::from).collect();
    let root = std::env::temp_dir().join(format!("pgm-sbx-rea-{}", std::process::id()));
    let mut cfg = Config::new(&root, "localhost/pgm-re-base:1.0.1"); cfg.name_prefix = "pgm-re-unit-".into();
    let m = Manager::new(Box::new(Cli::podman().with_env(env()).with_args(args.clone())), cfg);
    let a = Actor::new("alice", false);
    let s = m.start(&a, "real REA end to end", &StartOpts::default()).unwrap();
    assert_eq!(s.state, State::Running, "{:?}", s.error);
    let go = || -> Result<()> {
        let t0 = std::time::Instant::now();
        let list = m.mcp_tools(&a, &s.id, "", "")?;
        eprintln!("re_tools: {} of {} tools, server {}, first answer in {} ms", list["count"], list["of"], list["server"], t0.elapsed().as_millis());
        let names: Vec<String> = list["names"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
        eprintln!("tool names: {}", names.join(" "));
        eprintln!("size of the short list: {} bytes", list.to_string().len());
        eprintln!("jcode line: {:?}", m.mcp_exec_line(&a, &s.id)?);
        let sk = m.skill(&a, &s.id, "")?;
        eprintln!("re_skill: {} bytes of SKILL.md, files {}", sk["text"].as_str().unwrap().len(), sk["files"]);
        let tool = std::env::var("SANDBOX_REAL_TOOL").ok().unwrap_or_else(|| "list_adb_devices".to_string());
        eprintln!("calling {tool}: schema {}", m.mcp_tools(&a, &s.id, "", &tool)?["tool"]["inputSchema"]);
        let t1 = std::time::Instant::now();
        let r = m.mcp_call(&a, &s.id, &tool, &serde_json::json!({}), Some(120))?;
        eprintln!("re_call {tool} in {} ms: {}", t1.elapsed().as_millis(), r.to_string().chars().take(900).collect::<String>());
        Ok(())
    };
    let res = go();
    let dir = root.join(&s.id);
    m.stop(&a, &s.id).unwrap();
    assert!(!dir.exists() && !m.mcp_open(&s.id));
    assert!(Cli::podman().with_env(env()).with_args(args).list("pgm-re-unit-").unwrap().is_empty(), "no container left");
    let _ = std::fs::remove_dir_all(&root);
    res.unwrap();
}
