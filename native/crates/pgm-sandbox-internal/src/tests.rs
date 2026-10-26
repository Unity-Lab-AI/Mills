use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// A container engine that is a map: name -> (running, its exchange dirs). `exec` knows two commands:
/// `cp-in-out` copies /work/in to /work/out; anything else echoes its argument.
#[derive(Default)]
struct Fake { boxes: Mutex<BTreeMap<String, (bool, PathBuf, PathBuf)>>, fail_run: AtomicBool, runs: Mutex<Vec<RunSpec>> }
impl Runtime for Arc<Fake> {
    fn name(&self) -> &str { "fake" }
    fn available(&self) -> std::result::Result<(), String> { Ok(()) }
    fn image_present(&self, _: &str) -> bool { true }
    fn run(&self, s: &RunSpec) -> std::result::Result<(), String> {
        if self.fail_run.load(Ordering::SeqCst) { return Err("no such image".into()); }
        self.runs.lock().unwrap().push(s.clone());
        self.boxes.lock().unwrap().insert(s.name.clone(), (true, s.in_dir.clone(), s.out_dir.clone())); Ok(())
    }
    fn exec(&self, name: &str, argv: &[String], _t: Duration, sink: &mut dyn FnMut(Stream, &[u8])) -> std::result::Result<ExecEnd, String> {
        let b = self.boxes.lock().unwrap(); let Some((_, i, o)) = b.get(name) else { return Err("no such container".into()) };
        let last = argv.last().cloned().unwrap_or_default();
        if last == "cp-in-out" { for e in std::fs::read_dir(i).unwrap().flatten().filter(|e| e.path().is_file()) { std::fs::copy(e.path(), o.join(e.file_name())).unwrap(); } }
        else if last.starts_with("chmod") { return Ok(ExecEnd { code: Some(0), timed_out: false }); }
        else { sink(Stream::Stdout, last.as_bytes()); sink(Stream::Stderr, b"warn"); }
        Ok(ExecEnd { code: Some(0), timed_out: false })
    }
    fn remove(&self, name: &str) -> std::result::Result<(), String> { self.boxes.lock().unwrap().remove(name); Ok(()) }
    fn list(&self, prefix: &str) -> std::result::Result<Vec<Found>, String> { Ok(self.boxes.lock().unwrap().iter().filter(|(n, _)| n.starts_with(prefix)).map(|(n, b)| Found { name: n.clone(), running: b.0 }).collect()) }
}

fn root(n: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("pgm-sbx-{n}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); d }
fn mgr(n: &str) -> (Manager, Arc<Fake>, Arc<AtomicU64>) {
    let fake = Arc::new(Fake::default());
    let t = Arc::new(AtomicU64::new(1_000_000));
    let tc = t.clone();
    (Manager::with_clock(Box::new(fake.clone()), Config::new(root(n), "img:1"), move || tc.load(Ordering::SeqCst)), fake, t)
}
fn me(u: &str) -> Actor { Actor::new(u, false) }
fn states(m: &Manager) -> Vec<(String, State)> { m.list().into_iter().map(|s| (s.owner, s.state)).collect() }

#[test]
fn at_most_three_run_the_rest_wait_in_order_and_promote_when_a_slot_frees() {
    let (m, fake, _) = mgr("cap");
    let a = m.start(&me("a"), "one", &StartOpts::default()).unwrap();
    let b = m.start(&me("b"), "two", &StartOpts::default()).unwrap();
    let c = m.start(&me("c"), "three", &StartOpts::default()).unwrap();
    assert!([&a, &b, &c].iter().all(|s| s.state == State::Running));
    let d = m.start(&me("d"), "four", &StartOpts::default()).unwrap();
    let e = m.start(&me("e"), "five", &StartOpts::default()).unwrap();
    let f = m.start(&me("f"), "six", &StartOpts::default()).unwrap();
    assert_eq!((d.state, d.queue_position), (State::Queued, Some(1)));
    assert_eq!((e.queue_position, f.queue_position), (Some(2), Some(3)));
    assert_eq!(fake.boxes.lock().unwrap().len(), 3, "only three containers exist");
    assert_eq!(m.counts(), (3, 3, 3));
    // b stops: d (first in line) starts, e and f move up
    m.stop(&me("b"), &b.id).unwrap();
    assert_eq!(m.status(&d.id).unwrap().state, State::Running);
    assert_eq!(m.status(&e.id).unwrap().queue_position, Some(1));
    assert_eq!(m.status(&f.id).unwrap().queue_position, Some(2));
    assert_eq!(fake.boxes.lock().unwrap().len(), 3);
    // a queued copy can leave the queue without ever starting
    m.stop(&me("e"), &e.id).unwrap();
    assert_eq!(m.status(&f.id).unwrap().queue_position, Some(1));
    m.stop(&me("a"), &a.id).unwrap();
    assert_eq!(m.status(&f.id).unwrap().state, State::Running);
    assert_eq!(states(&m).iter().filter(|s| s.1 == State::Running).count(), 3);
    assert_eq!(fake.boxes.lock().unwrap().len(), 3);
}

#[test]
fn the_cap_is_configurable_and_a_failed_start_frees_its_slot_for_the_next() {
    let fake = Arc::new(Fake::default());
    let mut cfg = Config::new(root("cap1"), "img:1"); cfg.max_concurrent = 1;
    let m = Manager::new(Box::new(fake.clone()), cfg);
    let a = m.start(&me("a"), "", &StartOpts::default()).unwrap();
    let b = m.start(&me("b"), "", &StartOpts::default()).unwrap();
    assert_eq!(b.state, State::Queued);
    fake.fail_run.store(true, Ordering::SeqCst);
    m.stop(&me("a"), &a.id).unwrap(); // b is promoted, but the engine refuses it
    let b2 = m.status(&b.id).unwrap();
    assert_eq!(b2.state, State::Gone);
    assert!(b2.error.unwrap().contains("did not start"));
    assert!(m.list().is_empty());
    assert!(!root("cap1").join(&b.id).exists(), "a copy that never started leaves no folder");
}

#[test]
fn only_the_owner_stops_or_uses_a_copy_but_anyone_reads_and_an_admin_stops_any() {
    let (m, _, _) = mgr("owner");
    let s = m.start(&me("alice"), "re", &StartOpts::default()).unwrap();
    let bob = me("bob");
    assert_eq!(m.stop(&bob, &s.id).unwrap_err().status, 403);
    assert_eq!(m.exec(&bob, &s.id, &Cmd::Shell("id".into()), None).unwrap_err().status, 403);
    assert_eq!(m.put(&bob, &s.id, "x", b"1").unwrap_err().status, 403);
    assert_eq!(m.get(&bob, &s.id, "x").unwrap_err().status, 403);
    assert_eq!(m.list().len(), 1, "the list shows everyone's copies");
    assert_eq!(m.status(&s.id).unwrap().owner, "alice");
    assert_eq!(m.status(&s.id).unwrap().state, State::Running, "a refused stop changed nothing");
    assert_eq!(m.stop(&Actor::new("root", true), &s.id).unwrap().state, State::Gone);
    assert!(m.list().is_empty());
    assert_eq!(m.stop(&me("alice"), &s.id).unwrap().state, State::Gone, "stopping twice is fine");
    assert_eq!(m.stop(&me("alice"), "deadbeef").unwrap_err().status, 404);
    assert_eq!(m.stop(&me("alice"), "../../etc").unwrap_err().status, 400);
}

#[test]
fn data_goes_in_and_out_and_stop_deletes_everything() {
    let (m, fake, _) = mgr("data");
    let a = me("alice");
    let s = m.start(&a, "re", &StartOpts::default()).unwrap();
    let dir = m.config().root.join(&s.id);
    assert!(dir.join("in").is_dir() && dir.join("out").is_dir() && dir.join("meta.json").is_file());
    assert_eq!(m.put(&a, &s.id, "sample/app.bin", b"MZ\x90").unwrap(), ("/work/in/sample/app.bin".to_string(), 3));
    assert_eq!(std::fs::read(dir.join("in/sample/app.bin")).unwrap(), b"MZ\x90");
    // an exec streams: stdout and stderr are kept apart
    let r = m.exec(&a, &s.id, &Cmd::Shell("hello".into()), Some(5)).unwrap();
    assert_eq!((r.stdout.as_str(), r.stderr.as_str(), r.code), ("hello", "warn", Some(0)));
    // the copy writes a result; the person takes it out
    m.put(&a, &s.id, "note.txt", b"abc").unwrap();
    m.exec(&a, &s.id, &Cmd::Shell("cp-in-out".into()), None).unwrap();
    assert_eq!(m.get(&a, &s.id, "note.txt").unwrap(), b"abc");
    assert_eq!(m.get(&a, &s.id, "out/note.txt").unwrap(), b"abc");
    assert_eq!(m.get(&a, &s.id, "/work/out/note.txt").unwrap(), b"abc");
    assert_eq!(m.get(&a, &s.id, "missing").unwrap_err().status, 404);
    let files = m.files(&s.id);
    assert!(files.iter().any(|f| f.area == "in" && f.path == "sample/app.bin") && files.iter().any(|f| f.area == "out" && f.path == "note.txt"));
    assert_eq!(m.put(&a, &s.id, "big", &vec![0u8; (m.config().max_file_bytes + 1) as usize]).unwrap_err().status, 413);
    m.stop(&a, &s.id).unwrap();
    assert!(fake.boxes.lock().unwrap().is_empty(), "the container is gone");
    assert!(!dir.exists(), "its exchange folder is gone, nothing survives");
    assert_eq!(m.get(&a, &s.id, "note.txt").unwrap_err().status, 404);
}

#[test]
fn path_escapes_are_refused() {
    let (m, _, _) = mgr("esc");
    let a = me("alice");
    let s = m.start(&a, "re", &StartOpts::default()).unwrap();
    std::fs::write(m.config().root.join("outside.txt"), "host file").unwrap();
    for bad in ["../outside.txt", "in/../../outside.txt", "/etc/passwd", "a/../../outside.txt", "..\\outside.txt", "C:\\Windows\\x", "/work/../outside.txt", ""] {
        assert_eq!(m.put(&a, &s.id, bad, b"x").unwrap_err().status, 400, "put {bad:?}");
        assert_eq!(m.get(&a, &s.id, bad).unwrap_err().status, 400, "get {bad:?}");
    }
    assert_eq!(std::fs::read_to_string(m.config().root.join("outside.txt")).unwrap(), "host file");
    #[cfg(unix)]
    {
        // the copy plants a symlink to a host file in /work/out: reading it is refused
        std::os::unix::fs::symlink(m.config().root.join("outside.txt"), m.config().root.join(&s.id).join("out/link")).unwrap();
        assert_eq!(m.get(&a, &s.id, "link").unwrap_err().status, 400);
    }
}

#[test]
fn idle_is_flagged_never_acted_on_and_activity_resets_it() {
    let (m, _, t) = mgr("idle");
    let a = me("alice");
    let s = m.start(&a, "re", &StartOpts::default()).unwrap();
    assert!(!m.status(&s.id).unwrap().idle);
    t.fetch_add(5 * 3_600_000, Ordering::SeqCst);
    let st = m.status(&s.id).unwrap();
    assert!(st.idle && st.idle_hours >= 5.0 && st.state == State::Running);
    m.tick();
    assert_eq!(m.status(&s.id).unwrap().state, State::Running, "idle copies are never silently killed");
    m.exec(&a, &s.id, &Cmd::Shell("x".into()), None).unwrap();
    assert!(!m.status(&s.id).unwrap().idle, "using it resets the idle clock");
}

#[test]
fn a_restart_adopts_running_copies_and_lists_orphans_without_touching_them() {
    let (m, fake, t) = mgr("recon");
    let a = me("alice");
    let s = m.start(&a, "keep me", &StartOpts { network: true, image: None }).unwrap();
    t.fetch_add(7_200_000, Ordering::SeqCst);
    m.put(&a, &s.id, "keep.txt", b"data").unwrap(); // saves meta (last active)
    // foreign things: a container nobody here made, an exited copy, a folder without a container, another program's lookalike
    fake.boxes.lock().unwrap().insert("pgm-re-0badc0de".into(), (true, PathBuf::new(), PathBuf::new()));
    let cfg = m.config().clone();
    std::fs::create_dir_all(cfg.root.join("feedface/in")).unwrap();
    fake.boxes.lock().unwrap().insert("pgm-re-cafebabe".into(), (false, PathBuf::new(), PathBuf::new()));
    std::fs::create_dir_all(cfg.root.join("cafebabe")).unwrap();
    std::fs::write(cfg.root.join("cafebabe/meta.json"), json!({ "owner": "zed" }).to_string()).unwrap();
    fake.boxes.lock().unwrap().insert("pgm-re-something-else".into(), (true, PathBuf::new(), PathBuf::new()));
    // "restart": a new manager over the same engine and folders
    let m2 = Manager::with_clock(Box::new(fake.clone()), cfg, { let t = t.clone(); move || t.load(Ordering::SeqCst) });
    assert!(m2.list().is_empty());
    let (adopted, orphans) = m2.reconcile().unwrap();
    assert_eq!((adopted, orphans), (1, 3));
    let got = m2.status(&s.id).unwrap();
    assert!(got.adopted && got.state == State::Running && got.owner == "alice" && got.purpose == "keep me" && got.network);
    assert_eq!(m2.get(&a, &s.id, "in/keep.txt").unwrap(), b"data");
    assert_eq!(m2.exec(&me("bob"), &s.id, &Cmd::Shell("x".into()), None).unwrap_err().status, 403, "ownership survives the restart");
    let o = m2.orphans();
    assert!(o.iter().any(|x| x.id == "0badc0de" && x.kind == "container") && o.iter().any(|x| x.id == "cafebabe") && o.iter().any(|x| x.id == "feedface" && x.kind == "folder"));
    assert_eq!(fake.boxes.lock().unwrap().len(), 4 + 0, "reconcile killed nothing");
    assert!(cfg_root(&m2).join("feedface").exists());
    // only an admin clears an orphan
    assert_eq!(m2.stop(&me("alice"), "0badc0de").unwrap_err().status, 403);
    m2.stop(&Actor::new("root", true), "0badc0de").unwrap();
    assert!(!fake.boxes.lock().unwrap().contains_key("pgm-re-0badc0de"));
    // an adopted copy counts against the cap
    let (_, q, _) = m2.counts(); assert_eq!(q, 0);
}
fn cfg_root(m: &Manager) -> PathBuf { m.config().root.clone() }

#[test]
fn a_container_that_ends_by_itself_is_marked_gone_and_its_slot_is_reused() {
    let (m, fake, _) = mgr("died");
    let a = m.start(&me("a"), "", &StartOpts::default()).unwrap();
    for u in ["b", "c"] { m.start(&me(u), "", &StartOpts::default()).unwrap(); }
    let d = m.start(&me("d"), "", &StartOpts::default()).unwrap();
    assert_eq!(d.state, State::Queued);
    fake.boxes.lock().unwrap().remove(&a.name); // killed from outside (out of memory)
    m.tick();
    let gone = m.status(&a.id).unwrap();
    assert_eq!(gone.state, State::Gone);
    assert!(gone.error.unwrap().contains("ended by itself"));
    assert_eq!(m.status(&d.id).unwrap().state, State::Running);
    assert!(!m.config().root.join(&a.id).exists());
}

#[test]
fn network_is_opt_in_and_the_queue_is_bounded_per_owner() {
    let (m, fake, _) = mgr("net");
    m.start(&me("a"), "no net", &StartOpts::default()).unwrap();
    m.start(&me("a"), "net", &StartOpts { network: true, image: None }).unwrap();
    let runs = fake.runs.lock().unwrap().clone();
    assert!(!runs[0].network && runs[1].network);
    assert!(runs[0].args().join(" ").contains("--network none") && !runs[1].args().join(" ").contains("--network"));
    assert!(runs.iter().all(|r| r.name.starts_with("pgm-re-") && r.cap_add == vec!["SYS_PTRACE".to_string()]));
    m.start(&me("a"), "", &StartOpts::default()).unwrap();
    for _ in 0..10 { m.start(&me("a"), "", &StartOpts::default()).unwrap(); }
    assert_eq!(m.start(&me("a"), "", &StartOpts::default()).unwrap_err().status, 429);
    let mut c = Config::new(root("net2"), "i"); c.allow_network = false;
    let m2 = Manager::new(Box::new(fake.clone()), c);
    assert_eq!(m2.start(&me("a"), "", &StartOpts { network: true, image: None }).unwrap_err().status, 403);
}

#[test]
fn config_reads_the_hub_json() {
    let c = Config::from_json("/x", &json!({ "image": "i:2", "maxConcurrent": 5, "idleWarnHours": 1.5, "capAdd": [], "cpus": 4, "memory": "8g", "namePrefix": "pgm-re-t-", "maxFileMb": 99 }));
    assert_eq!((c.image.as_str(), c.max_concurrent, c.idle_warn_hours, c.cap_add.len(), c.cpus.as_str(), c.memory.as_str(), c.name_prefix.as_str(), c.max_file_bytes), ("i:2", 5, 1.5, 0, "4", "8g", "pgm-re-t-", 30 << 20));
    let d = Config::from_json("/x", &json!({}));
    assert_eq!((d.max_concurrent, d.image.as_str()), (3, "localhost/pgm-re-base:1.0.1"));
    assert!(matches!(Cmd::from_json(&json!("ls -l")), Some(Cmd::Shell(_))) && matches!(Cmd::from_json(&json!(["ls", "-l"])), Some(Cmd::Argv(_))) && Cmd::from_json(&json!("")).is_none() && Cmd::from_json(&json!([1])).is_none());
}

/// The MCP client against a fake server (tests/fake-mcp.sh run locally with `sh`, standing in for `exec -i`).
const FAKE_MCP: &str = include_str!("../tests/fake-mcp.sh");
static KILLS: AtomicU64 = AtomicU64::new(0);

struct Local(Arc<Fake>, PathBuf);
impl Runtime for Local {
    fn name(&self) -> &str { "local" }
    fn available(&self) -> std::result::Result<(), String> { Ok(()) }
    fn image_present(&self, i: &str) -> bool { self.0.image_present(i) }
    fn run(&self, s: &RunSpec) -> std::result::Result<(), String> { self.0.run(s) }
    fn exec(&self, n: &str, a: &[String], t: Duration, s: &mut dyn FnMut(Stream, &[u8])) -> std::result::Result<ExecEnd, String> { self.0.exec(n, a, t, s) }
    fn remove(&self, n: &str) -> std::result::Result<(), String> { self.0.remove(n) }
    fn list(&self, p: &str) -> std::result::Result<Vec<Found>, String> { self.0.list(p) }
    fn spawn(&self, _name: &str, _argv: &[String]) -> std::result::Result<Proc, String> {
        use std::process::{Command, Stdio};
        let mut c = Command::new("sh").arg(&self.1).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
        let (i, o, e) = (c.stdin.take().unwrap(), c.stdout.take().unwrap(), c.stderr.take().unwrap());
        Ok(Proc { stdin: Box::new(i), stdout: Box::new(o), stderr: Box::new(e), kill: Box::new(move || { KILLS.fetch_add(1, Ordering::SeqCst); let _ = c.kill(); let _ = c.wait(); }) })
    }
}

#[test]
fn mcp_tools_call_cap_and_close_on_stop() {
    let r = root("mcp"); std::fs::create_dir_all(&r).unwrap();
    let script = r.join("fake-mcp.sh"); std::fs::write(&script, FAKE_MCP).unwrap();
    let mut cfg = Config::new(r.join("x"), "img:1"); cfg.mcp_result_cap = 5000;
    let m = Manager::new(Box::new(Local(Arc::new(Fake::default()), script)), cfg);
    let (a, b) = (me("alice"), me("bob"));
    let s = m.start(&a, "mcp", &StartOpts::default()).unwrap();
    // nothing runs until the first use; then one session serves every call (the same pid)
    assert!(!m.mcp_open(&s.id));
    let t = m.mcp_tools(&a, &s.id, "", "").unwrap();
    assert_eq!((t["count"].clone(), t["of"].clone(), t["server"]["name"].clone(), t["names"].clone()), (json!(3), json!(3), json!("fake-rea"), json!(["echo", "big", "nope"])));
    assert_eq!(m.mcp_tools(&a, &s.id, "echo", "").unwrap()["tools"][0]["description"], json!("Echo the arguments back."), "first line only");
    assert_eq!(m.mcp_tools(&a, &s.id, "fail", "").unwrap()["tools"][0]["name"], json!("nope"));
    assert_eq!(m.mcp_tools(&a, &s.id, "", "echo").unwrap()["tool"]["inputSchema"]["type"], json!("object"));
    assert_eq!(m.mcp_tools(&a, &s.id, "", "zzz").unwrap_err().status, 404);
    let args = json!({ "path": "/work/in/ab.exe", "n": [1, 2], "deep": { "k": "v" } });
    let c1 = m.mcp_call(&a, &s.id, "echo", &args, Some(10)).unwrap();
    let c2 = m.mcp_call(&a, &s.id, "echo", &json!({}), Some(10)).unwrap();
    let (t1, t2) = (c1["content"][0]["text"].as_str().unwrap(), c2["content"][0]["text"].as_str().unwrap());
    assert!(t1.starts_with("[untrusted content] pid="), "markers survive: {t1}");
    let pid = |t: &str| t.split("pid=").nth(1).unwrap().split(' ').next().unwrap().to_string();
    assert_eq!(pid(t1), pid(t2), "one long-lived session");
    assert!(t1.contains(r#""n":[1,2]"#) && t1.contains(r#""k":"v""#), "{t1}");
    assert_eq!(m.mcp_call(&a, &s.id, "nope", &json!({}), None).unwrap()["isError"], json!(true));
    assert_eq!(m.mcp_call(&a, &s.id, "zzz", &json!({}), None).unwrap_err().status, 422);
    // too big: a head comes back, the whole is saved in /work/out/mcp-results/
    let big = m.mcp_call(&a, &s.id, "big", &json!({}), None).unwrap();
    assert_eq!(big["truncated"], json!(true));
    let head = big["content"][0]["text"].as_str().unwrap();
    assert!(head.len() <= 5000 && head.starts_with("[untrusted content]"));
    let saved = big["savedTo"].as_str().unwrap().to_string();
    assert!(saved.starts_with("/work/out/mcp-results/big-"));
    assert!(m.get(&a, &s.id, &saved["/work/out/".len()..]).unwrap().len() > 100_000);
    // owner only
    assert_eq!(m.mcp_tools(&b, &s.id, "", "").unwrap_err().status, 403);
    assert_eq!(m.mcp_call(&b, &s.id, "echo", &json!({}), None).unwrap_err().status, 403);
    assert_eq!(m.skill(&b, &s.id, "").unwrap_err().status, 403);
    assert_eq!(m.skill(&a, &s.id, "../etc/passwd").unwrap_err().status, 400);
    assert!(m.mcp_open(&s.id));
    let kills = KILLS.load(Ordering::SeqCst);
    m.stop(&a, &s.id).unwrap();
    assert!(!m.mcp_open(&s.id) && KILLS.load(Ordering::SeqCst) == kills + 1, "the session ends with the copy");
}
