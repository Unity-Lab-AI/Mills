//! Work runs ON this machine (only with "allowWork": true): Claude Code runs here, in this machine's worktree of the
//! project (or an empty sandbox for API answers), its events go back sealed with the session key. The team tools (PC,
//! board, team) are MCP servers this same program serves (`vr-host mcp pc|board|team`), relayed to the hub with the
//! run's own token. Mirrors bridge/ai-node.js workJob.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::harness::{self, CommandSpec, Harness, RunContext};
use crate::host::folder::log;
use crate::host::runner::H;
use crate::host::settings::Provider;
use pgm_net_internal::hostlink::{EventsAnswer, Job};

/// The hub's three team tools as MCP servers this program serves (`vr-host mcp pc|board|team`), relayed to the hub with the
/// run's own token: `{ "mcpServers": { pc, board, team } }`, the config Claude Code is pointed at and Jcode PGM's mcp.json.
fn mcp_servers(payload: &Value) -> Value {
    let me = std::env::current_exe().unwrap_or_default().display().to_string();
    let env = json!({ "MILLS_BRIDGE": payload["mcpBase"].as_str().unwrap_or(""), "MILLS_INTERNAL": payload["token"].as_str().unwrap_or(""), "MILLS_USER": payload["display"].as_str().or(payload["account"].as_str()).unwrap_or(""), "MILLS_ACCOUNT": payload["account"].as_str().unwrap_or("") });
    let srv = |which: &str| json!({ "command": me, "args": ["mcp", which], "env": env });
    json!({ "mcpServers": { "pc": srv("pc"), "board": srv("board"), "team": srv("team") } })
}
/// The provider as the jcode harness takes it (jcode.rs `command`). No `home` key on purpose: the home is always the
/// person's own under this host's `jcode/` folder, never one a provider entry could share between people.
fn jcode_prov(p: &Provider) -> Value {
    json!({ "kind": crate::jcode::KIND, "label": p.label, "models": p.models, "command": p.jcode_command, "provider": p.jcode_provider, "args": p.jcode_args })
}
/// How a Jcode PGM Work turn starts on this host (WORKPLAN 9c): `jcodepgm run --ndjson --no-update [-p profile] [-m model]
/// [--resume id] <message>` in the person's worktree, with a private jcode home per person at
/// `<jcode_root>/<account>/jcode-home` (the same layout as the hub's jcode harness: the team MCP servers as mcp.json, the hub's
/// system prompt as the prompt overlay; no auto-update, no auto-poke, no telemetry). The model is the person's pick
/// (`stateModel`) when the provider lists it, else its first.
pub fn jcode_spec(p: &Provider, payload: &Value, dir: &Path, jcode_root: &Path, mcp: &Value) -> CommandSpec {
    let who: String = payload["account"].as_str().unwrap_or("").chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-').collect();
    let who = if who.is_empty() { "_".to_string() } else { who };
    let (prov, acct_dir) = (jcode_prov(p), jcode_root.join(who));
    let hx = harness::JcodeHarness::new(&prov, payload["stateModel"].as_str().unwrap_or(""));
    let resume = payload["resume"].as_str().filter(|s| !s.is_empty());
    let input = payload["message"].as_str().unwrap_or("");
    hx.command(&RunContext { prov: &prov, input, resume, model: "", state_model: payload["stateModel"].as_str().unwrap_or(""), acct_dir: &acct_dir, cwd: dir,
        system_prompt: payload["systemPrompt"].as_str().unwrap_or(""), mcp_json: mcp, mcp_config: Path::new(""), permission_mode: "", allowed_tools: &[], add_dirs: &[], claude_command: "", claude_args: &[] })
}

fn valid_user(u: &str) -> bool { !u.is_empty() && u.len() <= 32 && u.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') }
fn valid_project(n: &str) -> bool { !n.is_empty() && n.len() <= 64 && n.chars().next().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false) && n.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && !n.contains("..") && !n.eq_ignore_ascii_case("_repo") }
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = pgm_platform_internal::proc::std_command("git").args(args).current_dir(dir).env("GIT_TERMINAL_PROMPT", "0").output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().lines().last().unwrap_or("git failed").to_string()) }
}
/// The folder a run happens in: the person's worktree of the project on work/<person> (made when missing, the
/// project cloned from the hub's address when this machine has none), like bridge/worktree.js ensure.
fn work_dir(h: &H, p: &Value) -> Result<PathBuf, String> {
    if p["sandbox"].as_bool() == Some(true) { let d = h.folder.sandbox(); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?; return Ok(d); }
    let account = p["account"].as_str().unwrap_or("");
    if !valid_user(account) { return Err("bad account name".into()); }
    let w = h.cfg.work();
    let hub_repo = w.get("hubRepo").and_then(Value::as_str).map(PathBuf::from).or_else(|| std::env::var_os("VIBEROLLER_HUB_REPO").map(PathBuf::from))
        .unwrap_or_else(|| h.folder.dir.join("..").join("VibeRoller"));
    let projects_root = w.get("projectsRoot").and_then(Value::as_str).map(PathBuf::from).unwrap_or_else(|| hub_repo.join("..").join("VibeRoller-projects"));
    let is_hub = p["hub"].as_bool() == Some(true) || p["project"].as_str().map(|n| n.eq_ignore_ascii_case("VibeRoller")).unwrap_or(true);
    let (repo, dir) = if is_hub {
        let root = w.get("worktreeRoot").and_then(Value::as_str).map(PathBuf::from).unwrap_or_else(|| hub_repo.join("..").join("Mills-worktrees"));
        (hub_repo.clone(), root.join(account))
    } else {
        let name = p["project"].as_str().unwrap_or("");
        if !valid_project(name) { return Err("bad project name".into()); }
        (projects_root.join(name).join("_repo"), projects_root.join(name).join(account))
    };
    if !repo.join(".git").exists() {
        let url = p["repoUrl"].as_str().unwrap_or("");
        if url.is_empty() { return Err(format!("This host has no copy of the project and the hub gave no address to clone it from ({}).", repo.display())); }
        if !(url.starts_with("https://") || url.starts_with("ssh://") || (url.contains('@') && url.contains(':'))) { return Err("the hub gave a repo address this host does not clone".into()); }
        log(format!("cloning {url} into {}", repo.display()));
        std::fs::create_dir_all(repo.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
        git(repo.parent().unwrap_or(Path::new(".")), &["clone", "-q", "-c", "protocol.ext.allow=never", "-c", "protocol.file.allow=never", "--", url, &repo.display().to_string()])?;
        if git(&repo, &["rev-parse", "--verify", "--quiet", "refs/remotes/origin/develop"]).is_ok() { let _ = git(&repo, &["checkout", "-q", "develop"]); }
    }
    if dir.join(".git").exists() {
        if git(&dir, &["rev-parse", "--git-dir"]).is_err() { let _ = git(&repo, &["worktree", "repair", &dir.display().to_string()]); }
        return Ok(dir);
    }
    if dir.exists() { return Err(format!("{} exists but is not a git worktree. Move it away first.", dir.display())); }
    let _ = git(&repo, &["worktree", "prune"]);
    let branch = format!("work/{account}");
    let main = if git(&repo, &["rev-parse", "--verify", "--quiet", "refs/heads/develop"]).is_ok() { "develop".to_string() } else { git(&repo, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_else(|_| "main".into()) };
    let d = dir.display().to_string();
    if git(&repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok() { git(&repo, &["worktree", "add", &d, &branch])?; } else { git(&repo, &["worktree", "add", "-b", &branch, &d, &main])?; }
    Ok(dir)
}

pub async fn work_job(h: H, job: Job) {
    let payload = { let link = h.link.read().await; match (&link.key, &job.enc) { (Some(k), Some(e)) => k.open(e).ok(), _ => None } };
    let Some(payload) = payload else { log("a Work job arrived in an envelope this key cannot open; saying hello again"); h.link.write().await.key = None; return; };
    let job_id = job.id.clone();
    let prov = h.providers().into_iter().find(|x| Some(x.id.as_str()) == payload["providerId"].as_str());
    let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let flush_lock = Arc::new(tokio::sync::Mutex::new(()));
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flush = { let (h, events, lock, id, prov) = (h.clone(), events.clone(), flush_lock.clone(), job_id.clone(), prov.clone()); move |fin: Option<Value>| {
        let (h, events, lock, id, prov) = (h.clone(), events.clone(), lock.clone(), id.clone(), prov.clone());
        async move {
            let _g = lock.lock().await;
            let batch: Vec<Value> = std::mem::take(&mut *events.lock().unwrap_or_else(|e| e.into_inner()));
            if batch.is_empty() && fin.is_none() { return EventsAnswer::Ok; }
            let usage = prov.as_ref().and_then(|p| h.usage.lock().unwrap_or_else(|e| e.into_inner()).get(&p.id).cloned()).unwrap_or(Value::Null);
            let mut b = json!({ "events": batch, "usage": usage });
            if let (Some(o), Some(Value::Object(f))) = (b.as_object_mut(), fin) { o.extend(f); }
            h.link.read().await.events(&id, &b).await
        }
    } };
    macro_rules! fail { ($text:expr) => {{ let t: String = $text; events.lock().unwrap_or_else(|e| e.into_inner()).push(json!({ "type": "bridge_error", "text": t })); flush(Some(json!({ "done": true, "code": 1 }))).await; return; }}; }
    let nick = h.nickname.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(p) = prov.clone().filter(|p| p.kind != "openai") else { fail!("This host no longer offers that provider for Work.".into()); };
    let cap = h.over_cap(&p); if !cap.is_empty() { fail!(format!("{nick}: {cap}. Pick another AI or wait for the window to reset.")); }
    let dir = match work_dir(&h, &payload) { Ok(d) => d, Err(e) => fail!(format!("{nick} could not prepare the folder: {e}")) };
    let sandbox = payload["sandbox"].as_bool() == Some(true);
    let mcp_file = h.folder.f(&format!(".mcp-host-{}.json", job_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect::<String>()));
    // Jcode PGM instead of Claude Code (opt-in, a provider of kind "jcode"): its command, home and events are in jcode_spec
    let jspec: Option<CommandSpec> = if p.is_jcode() {
        if sandbox { fail!(format!("{nick}: a Jcode PGM provider runs Work only, not API answers.")); }
        Some(jcode_spec(&p, &payload, &dir, &h.folder.f("jcode"), &mcp_servers(&payload)))
    } else { None };
    let mut args: Vec<String> = vec![];
    if jspec.is_none() {
        args = vec!["-p".into(), "--output-format".into(), "stream-json".into(), "--verbose".into(), "--permission-mode".into(), if sandbox { "default".into() } else { payload["permissionMode"].as_str().unwrap_or("acceptEdits").to_string() }];
        if sandbox { args.push("--strict-mcp-config".into()); } else {
            let _ = std::fs::write(&mcp_file, mcp_servers(&payload).to_string());
            pgm_core_internal::fs::private(&mcp_file);
            args.extend(["--mcp-config".into(), mcp_file.display().to_string(), "--strict-mcp-config".into()]);
            let tools: Vec<String> = payload["allowedTools"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default();
            if !tools.is_empty() { args.extend(["--allowedTools".into(), tools.join(",")]); }
        }
        for (flag, key) in [("--append-system-prompt", "systemPrompt"), ("--model", "model"), ("--resume", "resume")] { if let Some(v) = payload[key].as_str().filter(|s| !s.is_empty()) { args.extend([flag.to_string(), v.to_string()]); } }
    }
    let w = h.cfg.work();
    let command = match &jspec { Some(sp) => sp.program.clone(), None => w.get("claudeCommand").and_then(Value::as_str).unwrap_or("claude").to_string() };
    let pre: Vec<String> = if jspec.is_some() { vec![] } else { w.get("claudeArgs").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
    let mut c = pgm_platform_internal::proc::command(&command);
    match &jspec {
        Some(sp) => { c.args(&sp.args).current_dir(&sp.cwd); for (k, v) in &sp.env { c.env(k, v); } }
        None => { c.args(&pre).args(&args).current_dir(&dir); }
    }
    c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    if p.kind == "claude-cli" && !p.config_dir.is_empty() { c.env("CLAUDE_CONFIG_DIR", &p.config_dir); }
    if p.kind == "anthropic" && !p.api_key.is_empty() { c.env("ANTHROPIC_API_KEY", &p.api_key); }
    if let Some(env) = payload["env"].as_object() { for (k, v) in env { if let Some(v) = v.as_str() { c.env(k, v); } } }
    let pe = |k: &str| payload["env"][k].as_str().unwrap_or("").to_string();
    let who: String = payload["account"].as_str().unwrap_or("").chars().filter(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || *ch == '-').collect();
    if !who.is_empty() { for (k, v) in crate::fj::env(&h.folder.f(".fj").join(&who), &pe("FORGEJO_URL"), &pe("FORGEJO_TOKEN"), &pe("FORGEJO_LOGIN")) { c.env(k, v); } } // fj signed in as the person, from the token the hub sent
    log(format!("run {job_id} for {}: {} ({}{})", payload["account"].as_str().unwrap_or(""), if sandbox { "API answer".to_string() } else { format!("{} in {}", payload["project"].as_str().unwrap_or("VibeRoller"), dir.display()) }, p.label, payload["model"].as_str().map(|m| format!(", {m}")).unwrap_or_default()));
    let mut child = match c.spawn() { Ok(ch) => ch, Err(e) => { let _ = std::fs::remove_file(&mcp_file); fail!(format!("{nick} could not start {}: {e}", if jspec.is_some() { "jcodepgm" } else { "claude" })); } };
    h.runs.lock().unwrap_or_else(|e| e.into_inner()).insert(job_id.clone(), stop_tx);
    // Claude Code reads the message on stdin; Jcode PGM gets it as its last argument (nothing on stdin)
    if let Some(mut si) = child.stdin.take() { let m = if jspec.is_some() { String::new() } else { payload["message"].as_str().unwrap_or("").to_string() }; tokio::spawn(async move { let _ = si.write_all(m.as_bytes()).await; }); }
    let (out, mut err) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let ev2 = events.clone(); let pid = p.id.clone(); let h2 = h.clone();
    let mut jh = jspec.as_ref().map(|_| harness::JcodeHarness::new(&jcode_prov(&p), payload["stateModel"].as_str().unwrap_or("")));
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(out).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim().to_string(); if line.is_empty() { continue; }
            if let Some(jh) = jh.as_mut() { // jcode's events, mapped onto the hub's (the alias; the hub adds `hub`)
                for e in harness::Harness::map_line(jh, &line) { ev2.lock().unwrap_or_else(|e| e.into_inner()).push(e.into_value()); }
                continue;
            }
            match serde_json::from_str::<Value>(&line) { Ok(ev) => { note_usage(&h2, &pid, &ev); ev2.lock().unwrap_or_else(|e| e.into_inner()).push(ev); } Err(_) => ev2.lock().unwrap_or_else(|e| e.into_inner()).push(json!({ "type": "raw", "text": line })) }
        }
    });
    let ev3 = events.clone();
    let errs = tokio::spawn(async move { let mut buf = [0u8; 4096]; while let Ok(n) = err.read(&mut buf).await { if n == 0 { break; } ev3.lock().unwrap_or_else(|e| e.into_inner()).push(json!({ "type": "stderr", "text": String::from_utf8_lossy(&buf[..n]) })); } });
    // events go out every 150 ms (or at 40); a cancel / gone answer stops the run
    let status = loop {
        tokio::select! {
            s = child.wait() => break s,
            _ = &mut stop_rx => { stopped.store(true, std::sync::atomic::Ordering::SeqCst); let _ = child.start_kill(); }
            _ = tokio::time::sleep(Duration::from_millis(150)) => {
                match flush(None).await { EventsAnswer::Ok => {}, a => { if !stopped.swap(true, std::sync::atomic::Ordering::SeqCst) { log(format!("stopping run {job_id} ({})", if a == EventsAnswer::Gone { "the hub gave it up" } else { "stopped by its person" })); let _ = child.start_kill(); } } }
            }
        }
    };
    let _ = reader.await; let _ = errs.await;
    h.runs.lock().unwrap_or_else(|e| e.into_inner()).remove(&job_id);
    let _ = std::fs::remove_file(&mcp_file);
    let code = if stopped.load(std::sync::atomic::Ordering::SeqCst) { 130 } else { status.ok().and_then(|s| s.code()).unwrap_or(1) };
    flush(Some(json!({ "done": true, "code": code }))).await;
    log(format!("run {job_id} ended (code {code})"));
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::settings::HostSettings;

    fn provider(v: Value) -> Provider { let mut s = HostSettings::new("x.json"); s.raw.insert("providers".into(), json!([v])); s.all_providers().remove(0) }

    #[test]
    fn a_jcode_turn_is_jcodepgm_run_in_the_persons_own_home() {
        let p = provider(json!({ "id": "jcode", "kind": "jcode", "label": "Gee's jcode", "models": ["m1", "m2"], "jcodeCommand": "/opt/jcodepgm", "jcodeProvider": "viberoller", "jcodeArgs": ["--socket", "/tmp/j.sock"] }));
        let root = std::env::temp_dir().join(format!("pgm-hostjcode-{}", pgm_core_internal::crypto::random_hex(4)));
        let mcp = json!({ "mcpServers": { "pc": { "command": "vr-host" } } });
        let payload = json!({ "account": "sponge", "message": "do it", "systemPrompt": "LAWS", "stateModel": "m2", "model": "claude-opus-5-5", "resume": "sess-1" });
        let sp = jcode_spec(&p, &payload, Path::new("/wt/sponge"), &root, &mcp);
        assert_eq!(sp.program, "/opt/jcodepgm");
        assert_eq!(sp.args, ["--socket", "/tmp/j.sock", "run", "--ndjson", "--no-update", "-p", "viberoller", "-m", "m2", "--resume", "sess-1", "do it"]);
        assert_eq!(sp.stdin, ""); assert_eq!(sp.cwd, PathBuf::from("/wt/sponge"));
        let home = root.join("sponge").join("jcode-home");
        let get = |k: &str| sp.env.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).unwrap_or_default();
        assert_eq!(get("JCODE_HOME"), home.display().to_string()); assert_eq!(get("JCODE_NO_AUTO_UPDATE"), "1"); assert_eq!(get("JCODE_RUN_AUTO_POKE"), "0"); assert_eq!(get("JCODE_NO_TELEMETRY"), "1");
        assert_eq!(std::fs::read_to_string(home.join("prompt-overlay.md")).unwrap(), "LAWS");
        assert!(std::fs::read_to_string(home.join("mcp.json")).unwrap().contains("vr-host"));
        // another person gets another home; no resume, a model the provider does not list falls back to its first, default binary
        let q = provider(json!({ "id": "j2", "harness": "jcode", "models": ["only"] }));
        assert!(q.is_jcode());
        let sp2 = jcode_spec(&q, &json!({ "account": "gee", "message": "hi", "stateModel": "claude-opus-5-5" }), Path::new("/wt/gee"), &root, &mcp);
        assert_eq!(sp2.program, "jcodepgm"); assert_eq!(sp2.args, ["run", "--ndjson", "--no-update", "-m", "only", "hi"]);
        assert_ne!(sp2.env.iter().find(|(k, _)| k == "JCODE_HOME").unwrap().1, get("JCODE_HOME"));
        assert!(root.join("gee").join("jcode-home").join("mcp.json").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn only_a_host_that_allows_work_with_a_jcode_provider_announces_it() {
        let p = provider(json!({ "id": "claude", "kind": "claude-cli" }));
        assert!(!p.is_jcode());
        assert_eq!(pgm_net_internal::hostlink::caps_for(false), pgm_net_internal::hostlink::CAPS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    }
}

/// The usage windows Claude Code reports in its rate-limit events.
fn note_usage(h: &H, pid: &str, ev: &Value) {
    if !ev["type"].as_str().unwrap_or("").contains("rate_limit") { return; }
    let info = if ev.get("rate_limit_info").is_some() { &ev["rate_limit_info"] } else if ev.get("rateLimitInfo").is_some() { &ev["rateLimitInfo"] } else { ev };
    let kind = info["rateLimitType"].as_str().or(info["rate_limit_type"].as_str()).or(info["type"].as_str()).unwrap_or("").to_ascii_lowercase();
    let w = if ["seven", "week", "7d"].iter().any(|k| kind.contains(k)) { "seven_day" } else if ["five", "5h", "session"].iter().any(|k| kind.contains(k)) { "five_hour" } else { return };
    let raw = info.get("utilization").or(info.get("usage")).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
    let mut all = h.usage.lock().unwrap_or_else(|e| e.into_inner());
    let u = all.entry(pid.to_string()).or_insert_with(|| json!({}));
    let prev = u[w].clone();
    let util = raw.map(|n| (if n > 1.0 { n / 100.0 } else { n }).clamp(0.0, 1.0)).or(prev["utilization"].as_f64());
    let resets = info.get("resetsAt").or(info.get("resets_at")).and_then(|v| v.as_f64().map(|n| if n < 1e12 { n * 1000.0 } else { n })).or(prev["resetsAt"].as_f64());
    u[w] = json!({ "status": info["status"].as_str().or(prev["status"].as_str()).unwrap_or(""), "utilization": util, "resetsAt": resets });
}
