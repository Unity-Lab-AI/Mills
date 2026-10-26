//! Tests of the machine's tools on a real temp tree (R328, R329 in unit form; the suite runs the same over the wire).
use super::*;
use crate::machine::sign::{Envelope, HubKey};

struct T { dir: PathBuf, home: PathBuf, m: Machine }
impl Drop for T { fn drop(&mut self) { self.m.stop_jobs(); let _ = std::fs::remove_dir_all(&self.dir); } }
fn actor() -> Actor { Actor { account: "sponge".into(), session: "s1".into(), run: "r1".into() } }
fn setup(tag: &str, scope: &str) -> T {
    let dir = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("vr-exec-{tag}-{}", pgm_core_internal::crypto::random_hex(4)));
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let state = dir.join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("scope.json"), scope.replace("HOME", home.to_str().unwrap())).unwrap();
    let m = Machine::open(&state, "m1", "test").unwrap();
    T { dir, home, m }
}
const FOLDER: &str = r#"{"preset":"folder","paths":[{"path":"HOME","access":"rw"}],"limits":{"concurrentJobs":2}}"#;
async fn call(t: &T, tool: &str, a: Value) -> Value { t.m.run_as(&actor(), tool, &a).await }
fn ok(v: &Value) -> bool { v["ok"] == json!(true) }

#[tokio::test]
async fn files_answer_like_the_host_and_stay_in_the_folder() {
    let t = setup("files", FOLDER);
    assert_eq!(call(&t, "list", json!({})).await["result"], json!("(empty folder)"));
    assert_eq!(call(&t, "write", json!({ "path": "a/b.txt", "content": "héllo" })).await["result"], json!("Wrote a/b.txt (5 chars)"));
    assert_eq!(call(&t, "write", json!({ "path": "", "content": "x" })).await["error"], json!("Missing file name."));
    assert_eq!(call(&t, "read", json!({ "path": "a/b.txt" })).await["result"], json!("héllo"));
    assert_eq!(call(&t, "list", json!({ "path": "a" })).await["result"], json!("b.txt"));
    assert_eq!(call(&t, "list", json!({})).await["result"], json!("a/"));
    assert_eq!(call(&t, "mkdir", json!({ "path": "x/y" })).await["result"], json!("Created folder x/y"));
    assert_eq!(call(&t, "read", json!({ "path": "a/b.txt", "offset": 0, "limit": 3 })).await["result"], json!("h\u{e9}"));
    assert_eq!(call(&t, "delete", json!({ "path": "a" })).await["result"], json!("Deleted a"));
    assert_eq!(call(&t, "delete", json!({ "path": "a" })).await["result"], json!("Deleted a"), "deleting what is not there is fine, as on the host");
    assert_eq!(call(&t, "delete", json!({ "path": "" })).await["error"], json!("Refusing to delete the whole folder."));
    // out of the folder: refused, nothing written
    for p in ["../outside.txt", "/etc/passwd", "a/../../x"] {
        let r = call(&t, "write", json!({ "path": p, "content": "no" })).await;
        assert!(!ok(&r), "{p}: {r}");
    }
    assert!(!t.dir.join("outside.txt").exists());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(t.dir.join("state"), t.home.join("ln")).unwrap();
        assert!(!ok(&call(&t, "read", json!({ "path": "ln/scope.json" })).await));
        assert!(!ok(&call(&t, "list", json!({ "path": "ln" })).await));
    }
    // no leftover temp file from the atomic write
    assert_eq!(std::fs::read_dir(t.home.join("x")).unwrap().count(), 1);
    assert_eq!(call(&t, "list", json!({ "path": "nonexistent" })).await["ok"], json!(false));
}

#[tokio::test]
async fn a_read_only_scope_refuses_writes_and_hides_the_write_tools() {
    let t = setup("ro", r#"{"preset":"folders","paths":[{"path":"HOME","access":"ro"}]}"#);
    std::fs::write(t.home.join("f.txt"), "data").unwrap();
    assert_eq!(call(&t, "read", json!({ "path": "f.txt" })).await["result"], json!("data"));
    for (tool, a) in [("write", json!({ "path": "n.txt", "content": "x" })), ("delete", json!({ "path": "f.txt" })), ("mkdir", json!({ "path": "d" }))] {
        let r = call(&t, tool, a).await;
        assert!(!ok(&r), "{tool}");
    }
    assert!(t.home.join("f.txt").exists() && !t.home.join("n.txt").exists() && !t.home.join("d").exists());
    let tools = t.m.tools();
    assert!(tools.contains(&"read") && !tools.contains(&"write") && !tools.contains(&"transfer_put"));
    let log = std::fs::read_to_string(t.dir.join("state/machine-audit.jsonl")).unwrap();
    assert!(log.lines().filter(|l| l.contains("\"deny\"")).count() >= 3, "every deny is a line");
    assert!(!log.contains("\"data\""), "contents are never logged");
}

#[tokio::test]
async fn machine_info_tells_the_scope_and_the_tools() {
    let t = setup("info", FOLDER);
    let r = call(&t, "machine_info", json!({})).await;
    assert!(ok(&r));
    let i = &r["result"];
    assert_eq!(i["os"], json!(std::env::consts::OS));
    assert_eq!(i["arch"], json!(std::env::consts::ARCH));
    assert!(i["scope"].as_str().unwrap().starts_with("one folder: "), "{i}");
    assert_eq!(i["access"], json!("read-write"));
    let names: Vec<&str> = i["tools"].as_array().unwrap().iter().filter_map(|x| x.as_str()).collect();
    for n in ["machine_info", "list", "read", "write", "delete", "mkdir", "shell_run", "job_start", "job_read", "job_stop", "proc_list", "proc_kill", "transfer_get", "transfer_put"] { assert!(names.contains(&n), "{n} in {names:?}"); }
    assert!(i["shell"]["confinement"].as_str().unwrap().starts_with("advisory"));
    assert_eq!(i["containers"]["available"], json!(false));
    // RE is off in a folder scope until the owner turns it on, and without an engine it says so
    assert!(!names.contains(&"re_run"));
    let t2 = setup("info2", r#"{"preset":"folder","paths":[{"path":"HOME","access":"rw"}],"tools":{"containers":{"re":true}}}"#);
    let r = call(&t2, "re_run", json!({ "cmd": "id" })).await;
    assert!(!ok(&r) && r["error"].as_str().unwrap().contains("no container engine"), "{r}");
}

#[cfg(unix)]
#[tokio::test]
async fn shell_is_confined_times_out_and_is_capped() {
    let t = setup("shell", r#"{"preset":"folder","paths":[{"path":"HOME","access":"rw"}],"tools":{"shell":{"enabled":true,"timeoutSec":3,"maxOutputKB":1}}}"#);
    std::fs::create_dir_all(t.home.join("sub")).unwrap();
    let r = call(&t, "shell_run", json!({ "cmd": "pwd; echo err >&2; exit 3" })).await;
    assert!(ok(&r), "{r}");
    assert_eq!(r["result"]["stdout"].as_str().unwrap().trim(), t.home.to_str().unwrap());
    assert_eq!(r["result"]["stderr"].as_str().unwrap().trim(), "err");
    assert_eq!(r["result"]["exit"], json!(3));
    let r = call(&t, "shell_run", json!({ "command": "pwd", "cwd": "sub" })).await;
    assert!(r["result"]["stdout"].as_str().unwrap().trim().ends_with("/sub"));
    for cwd in ["..", "/", "/etc", "sub/../.."] { let r = call(&t, "shell_run", json!({ "cmd": "pwd", "cwd": cwd })).await; assert!(!ok(&r), "{cwd}: {r}"); }
    std::os::unix::fs::symlink("/", t.home.join("rootlink")).unwrap();
    assert!(!ok(&call(&t, "shell_run", json!({ "cmd": "pwd", "cwd": "rootlink" })).await));
    let t0 = std::time::Instant::now();
    let r = call(&t, "shell_run", json!({ "cmd": "sleep 30", "timeoutSec": 1 })).await;
    assert!(ok(&r) && r["result"]["timedOut"] == json!(true) && t0.elapsed().as_secs() < 8, "{r}");
    let r = call(&t, "shell_run", json!({ "cmd": "sleep 30" })).await; // the scope's own cap (3 s) applies when none is asked
    assert!(r["result"]["timedOut"] == json!(true));
    let r = call(&t, "shell_run", json!({ "cmd": "head -c 100000 /dev/zero | tr '\\0' x", "timeoutSec": 99 })).await;
    assert!(r["result"]["truncated"] == json!(true) && r["result"]["stdout"].as_str().unwrap().len() <= 1024, "{r}");
    assert!(!ok(&call(&t, "shell_run", json!({ "cmd": "  " })).await));
}

#[tokio::test]
async fn no_shell_removes_the_shell_tools() {
    let t = setup("noshell", r#"{"preset":"no-shell","paths":[{"path":"HOME","access":"rw"}],"tools":{"shell":{"enabled":false}}}"#);
    let tools = t.m.tools();
    for n in ["shell_run", "job_start", "job_read", "job_stop"] { assert!(!tools.contains(&n), "{n}"); }
    let r = call(&t, "shell_run", json!({ "cmd": "echo hi" })).await;
    assert!(!ok(&r) && r["error"].as_str().unwrap().contains("does not offer shell_run"), "{r}");
    assert!(!ok(&call(&t, "job_start", json!({ "cmd": "echo hi" })).await));
    assert!(ok(&call(&t, "list", json!({})).await));
}

#[cfg(unix)]
#[tokio::test]
async fn jobs_run_on_the_machine_and_survive_a_reader() {
    let t = setup("jobs", FOLDER);
    let r = call(&t, "job_start", json!({ "cmd": "echo one; sleep 1; echo two; sleep 30", "timeoutSec": 60 })).await;
    assert!(ok(&r), "{r}");
    let id = r["result"]["jobId"].as_str().unwrap().to_string();
    // "a page reload": a different caller reads by job id later
    tokio::time::sleep(Duration::from_millis(400)).await;
    let a = call(&t, "job_read", json!({ "jobId": id })).await;
    assert_eq!(a["result"]["running"], json!(true));
    assert_eq!(a["result"]["output"], json!("one\n"));
    let next = a["result"]["next"].as_u64().unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let b = call(&t, "job_read", json!({ "jobId": id, "since": next })).await;
    assert_eq!(b["result"]["output"], json!("two\n"));
    let list = call(&t, "job_read", json!({})).await;
    assert_eq!(list["result"]["jobs"].as_array().unwrap().len(), 1);
    // the limit
    assert!(ok(&call(&t, "job_start", json!({ "cmd": "sleep 30" })).await));
    let r3 = call(&t, "job_start", json!({ "cmd": "sleep 30" })).await;
    assert!(!ok(&r3) && r3["error"].as_str().unwrap().contains("running already"), "{r3}");
    let s = call(&t, "job_stop", json!({ "jobId": id })).await;
    assert_eq!(s["result"]["state"], json!("stopped"));
    assert!(t.m.stop_jobs() >= 1, "the revoke / kill switch ends what still runs");
    assert!(!ok(&call(&t, "job_read", json!({ "jobId": id })).await), "the table is empty after a revoke");
    // an exit code
    let r = call(&t, "job_start", json!({ "cmd": "exit 7" })).await;
    let id = r["result"]["jobId"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let a = call(&t, "job_read", json!({ "jobId": id })).await;
    assert_eq!((a["result"]["state"].clone(), a["result"]["exit"].clone()), (json!("exited"), json!(7)));
}

#[cfg(unix)]
#[tokio::test]
async fn own_children_can_be_killed_and_unrelated_processes_cannot() {
    let t = setup("kill", FOLDER);
    let mut other = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let r = call(&t, "proc_kill", json!({ "pid": other.id() })).await;
    assert!(!ok(&r) && r["error"].as_str().unwrap().contains("not started by a tool"), "{r}");
    assert!(other.try_wait().unwrap().is_none(), "the unrelated process still runs");
    let l = call(&t, "proc_list", json!({})).await;
    assert!(l["result"]["processes"].as_array().unwrap().iter().any(|p| p["pid"] == json!(other.id()) && p["ownChild"] == json!(false)));
    let r = call(&t, "job_start", json!({ "cmd": "sleep 30" })).await;
    let id = r["result"]["jobId"].as_str().unwrap().to_string();
    let pid = call(&t, "job_read", json!({ "jobId": id })).await["result"]["pid"].as_u64().unwrap();
    let k = call(&t, "proc_kill", json!({ "pid": pid })).await;
    assert!(ok(&k) && k["result"]["ownChild"] == json!(true), "{k}");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(call(&t, "job_read", json!({ "jobId": id })).await["result"]["running"], json!(false));
    assert!(!ok(&call(&t, "proc_kill", json!({ "pid": 1 })).await));
    let _ = other.kill();
    let _ = other.wait();
}

#[tokio::test]
async fn a_request_goes_through_the_signature_the_kill_switch_and_the_audit_log() {
    let t = setup("sig", FOLDER);
    let hub = HubKey::generate();
    let body = br#"{"path":""}"#;
    let header = |tool: &str, owner: &str, who: &str| hub.sign(&Envelope::new(owner, Actor { account: who.into(), session: "s1".into(), run: "r9".into() }, tool, body, "m1", unix_now())).to_header();
    // no hub key pinned: everything is refused
    assert!(!ok(&t.m.handle("list", body, &header("list", "sponge", "sponge")).await));
    t.m.set_verifier(Some(Arc::new(Verifier::new(&hub.public_hex(), "sponge", "m1").unwrap())));
    assert!(ok(&t.m.handle("list", body, &header("list", "sponge", "sponge")).await));
    for (h, why) in [(header("list", "gee", "gee"), "owner"), (header("list", "sponge", "gee"), "actor"), (header("read", "sponge", "sponge"), "tool"), (String::new(), "unsigned")] {
        let r = t.m.handle("list", body, &h).await;
        assert!(!ok(&r) && r["error"].as_str().unwrap().contains("refused"), "{why}: {r}");
    }
    let liar = HubKey::generate().sign(&Envelope::new("sponge", Actor { account: "sponge".into(), ..Default::default() }, "list", body, "m1", unix_now())).to_header();
    assert!(!ok(&t.m.handle("list", body, &liar).await), "a hub stub that lies");
    // a replayed request
    let h = header("list", "sponge", "sponge");
    assert!(ok(&t.m.handle("list", body, &h).await));
    assert!(!ok(&t.m.handle("list", body, &h).await));
    // the kill switch: nothing runs, even with a good signature
    t.m.kill().stop("test", "unit test", unix_now()).unwrap();
    assert!(!ok(&t.m.handle("list", body, &header("list", "sponge", "sponge")).await));
    std::fs::remove_file(t.dir.join("state/KILL")).unwrap();
    assert!(ok(&t.m.handle("list", body, &header("list", "sponge", "sponge")).await));
    let v = t.m.audit().verify();
    assert!(v.ok && v.lines >= 8, "{v:?}");
}

#[tokio::test]
async fn the_state_folder_is_never_reachable_even_unrestricted() {
    let t = setup("floor", r#"{"preset":"unrestricted"}"#);
    let state = t.dir.join("state");
    for (tool, a) in [("read", json!({ "path": state.join("scope.json").to_str().unwrap() })), ("write", json!({ "path": state.join("scope.json").to_str().unwrap(), "content": "{}" })), ("read", json!({ "path": state.join("machine-audit.jsonl").to_str().unwrap() })), ("delete", json!({ "path": state.join("KILL").to_str().unwrap() }))] {
        let r = call(&t, tool, a).await;
        assert!(!ok(&r), "{tool}: {r}");
    }
}
