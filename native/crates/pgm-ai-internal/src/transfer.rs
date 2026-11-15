//! /transfer in Work (T-375; Jcode PGM's /transfer, done the hub's way): a session's conversation is compacted into a
//! handoff and a fresh session starts from it, so the new AI picks up right where the old one left off.
//! - POST /api/work/sessions/transfer { session, focus?, keepWorktree?: true } (the person's own session, not while its
//!   run is working: 409) → { ok, from, to, title, handoff: { text, by: "model" | "plain", why?, model? }, worktree, run, started, board }
//! - the handoff: a throw-away one-shot model call (no tools, no MCP, no session kept, in an empty folder that goes, as the
//!   prompt enhancer does, crate::enhance) through the session's own AI: Claude Code (the hub's Claude, a key, a config
//!   folder, or an AI host's login relayed through /llm/<token>), or the session's OpenRouter provider (its key, budget and
//!   usage, through the gateway). It gets the session's conversation in short (enhance::digest), the worktree's state (git
//!   status --porcelain, branch, last commits), the files and paths the run touched (from its tool lines) and the person's
//!   focus. When there is no such call (another engine) or it fails, a plain structured handoff is built without a model,
//!   and the answer says so (by: "plain", why).
//! - the new session: the same project, provider, model, harness and mode, the old one's folder in the list, its title
//!   "<title> (continued)", links both ways (meta transferredTo / transferredFrom), and by default the old one's
//!   WORKTREE (meta tree; uncommitted work goes on there): the old one then gave it on (treeGivenTo), runs no more and
//!   deleting it leaves the worktree (crate::sessions). Messages still queued in the old one move over behind the handoff.
//! - the new session's first message is the handoff under a fixed first line; it runs at once (a normal Work run, POST
//!   /api/chat's own code) and the page switches to it. The handoff stays in the new session's conversation (never a file in
//!   the worktree: that would dirty it).
//! - board tasks the old session's runs left claimed get a handoff note naming the new session (T-266's format).
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::hub::{answer, AiHub, Resp};
use crate::sessions;
use crate::work::Work;
use pgm_core_internal::json::s;
use pgm_core_internal::now_ms;
use pgm_identity_internal::Account;
use pgm_net_internal::http::Req;

const TIMEOUT: Duration = Duration::from_secs(240);
/// How much of the conversation the handoff writer gets (characters; config transferContextChars).
pub const CONTEXT_CHARS: usize = 30_000;
/// The focus the person may add (characters).
pub const MAX_FOCUS: usize = 2_000;
/// The fixed first line of the new session's first message starts with this.
pub const FIRST_LINE: &str = "[transfer from session ";

/// What the handoff writer is told (its whole system prompt).
pub const SYSTEM: &str = r###"You are a handoff writer. You never do the work yourself and you never continue the conversation. You receive the record of a Work session between a person and an AI coding agent (Claude Code or Jcode PGM working in a software project), the exact state of its git worktree, and the files the agent touched. A FRESH agent session will continue the work from what you write, with nothing else of the old session: no history, no memory. Write the handoff that lets it pick up exactly where the old session left off.

RULES:
- Be faithful: only what the record shows. Never invent progress, files, decisions or results. Where the record is unclear, say so under "Open problems".
- Be specific: name files, functions, commands, branches, check numbers, error messages and versions exactly as they appear.
- Keep the person's wishes and constraints, especially the ones that still apply ("never …", "always …", "do not touch …").
- Mark clearly what is FINISHED (so it is not redone) and what is NOT.
- When the person gave a focus for the handoff, follow it: what it says to keep or leave out wins.

THE LAYOUT (Markdown; leave a section out only when there is nothing for it):

## Goal
What the person wants overall, in their terms.

## Done
What is finished and verified, one line each (with the commit, file or check that shows it).

## Current state
Where the work is right now: the branch, the uncommitted changes (from the git state given), what was being done at the end, what is half-done.

## Decisions
What was decided and why (designs chosen, things ruled out), including the person's own decisions.

## Open problems
Bugs, failures, unknowns and questions still open.

## Next steps
The next concrete steps, in order, for the new session.

## Important files and paths
The files and paths that matter for going on, with one line on why each matters.

FORMAT: only the handoff, ready to give to the new session. No preface, no closing remarks, no surrounding code fence. Write in the language of the conversation. As long as needed, no longer."###;

fn running() -> &'static Mutex<HashSet<String>> { static R: OnceLock<Mutex<HashSet<String>>> = OnceLock::new(); R.get_or_init(Mutex::default) }
/// Is this session (its run key) being transferred right now (its runs wait)?
pub fn busy(key: &str) -> bool { running().lock().unwrap_or_else(|e| e.into_inner()).contains(key) }
/// The title of the session `sid` was transferred to ("" when none).
pub fn to_title(a: &Account, sid: &str) -> String {
    let v = sessions::index(a); let to = s(&v["sessions"][sid]["transferredTo"], "id");
    let t = s(&v["sessions"][&to], "title"); if t.is_empty() { s(&v["sessions"][sid]["transferredTo"], "title") } else { t }
}

// ---------- what the handoff is made of ----------
/// The files and paths a session's runs touched, from its tool lines (the page's "Name  target"): edited ones (Edit,
/// Write, MultiEdit, NotebookEdit, a PC write / delete / mkdir, jcode's edit / write / patch) and read ones (Read, a PC
/// read, jcode's read / view), each once, the most recent last, at most 40 each. Commands and searches are left out.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Touched { pub edited: Vec<String>, pub read: Vec<String> }
pub fn touched(transcript: &[Value]) -> Touched {
    let mut t = Touched::default();
    for line in transcript.iter().filter(|x| x["cls"] == json!("tool")).filter_map(|x| x["text"].as_str()) {
        let Some((name, target)) = line.split_once("  ") else { continue };
        let target = target.trim(); let name = name.trim().to_lowercase();
        if target.is_empty() || target.contains('\n') || target.chars().count() > 300 { continue; }
        let tool = name.rsplit(['›', ' ', '_', '.']).next().unwrap_or(&name).trim().to_string();
        let bucket = if name.contains("bash") || name.contains("shell") || name.contains("grep") || name.contains("glob") || name.contains("search") || name.contains("fetch") || name.contains("job") { None }
            else if ["edit", "write", "multiedit", "notebookedit", "patch", "delete", "mkdir", "create"].iter().any(|k| tool == *k || tool.ends_with(k)) { Some(&mut t.edited) }
            else if ["read", "view", "open", "cat", "list"].iter().any(|k| tool == *k) { Some(&mut t.read) }
            else { None };
        if let Some(l) = bucket { l.retain(|x| x != target); l.push(target.to_string()); }
    }
    // a file edited and read is listed as edited
    let ed = t.edited.clone(); t.read.retain(|x| !ed.contains(x));
    for l in [&mut t.edited, &mut t.read] { let n = l.len(); if n > 40 { l.drain(0..n - 40); } }
    t
}
impl Touched {
    pub fn text(&self) -> String {
        if self.edited.is_empty() && self.read.is_empty() { return "None seen in the session's tool lines.".into(); }
        let mut out = String::new();
        if !self.edited.is_empty() { out.push_str("Edited / written:\n"); for p in &self.edited { out.push_str(&format!("- {p}\n")); } }
        if !self.read.is_empty() { out.push_str("Read:\n"); for p in &self.read { out.push_str(&format!("- {p}\n")); } }
        out.trim_end().to_string()
    }
}
/// A worktree's state as the handoff names it.
#[derive(Default, Clone, Debug)]
pub struct GitState { pub project: String, pub dir: String, pub branch: String, pub git: bool, pub status: Vec<String>, pub commits: Vec<String>, pub note: String }
impl GitState {
    pub fn text(&self) -> String {
        let mut out = vec![];
        if self.project == pgm_collab_internal::worktree::NO_PROJECT { out.push(format!("No project: the session works in the person's scratch folder {} (no git).", self.dir)); }
        else {
            out.push(format!("Project: {}", self.project));
            if !self.dir.is_empty() { out.push(format!("Worktree: {}", self.dir)); }
            if !self.branch.is_empty() { out.push(format!("Branch: {}", self.branch)); }
        }
        if !self.note.is_empty() { out.push(self.note.clone()); }
        if self.git {
            if self.status.is_empty() { out.push("Uncommitted changes: none (git status --porcelain is empty).".into()); }
            else { out.push(format!("Uncommitted changes (git status --porcelain, {}):", self.status.len())); for l in self.status.iter().take(100) { out.push(format!("    {l}")); } if self.status.len() > 100 { out.push(format!("    … and {} more", self.status.len() - 100)); } }
            if !self.commits.is_empty() { out.push("Last commits:".into()); for c in &self.commits { out.push(format!("    {c}")); } }
        }
        out.join("\n")
    }
    pub fn changes(&self) -> Option<u64> { if self.git { Some(self.status.len() as u64) } else { None } }
}
fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new("git");
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
    let o = c.arg("-C").arg(dir).args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    if o.status.success() { Some(String::from_utf8_lossy(&o.stdout).to_string()) } else { None }
}
/// The state of the worktree `dir` (git; nothing when it is no checkout).
pub fn git_state(project: &str, dir: &Path, branch: &str) -> GitState {
    let mut g = GitState { project: project.into(), dir: dir.display().to_string(), branch: branch.into(), ..Default::default() };
    if project == pgm_collab_internal::worktree::NO_PROJECT { return g; }
    let Some(st) = git_out(dir, &["status", "--porcelain"]) else { g.note = "(the worktree is no git checkout yet)".into(); return g };
    g.git = true;
    g.status = st.lines().map(|l| l.trim_end().to_string()).filter(|l| !l.trim().is_empty()).collect();
    if let Some(b) = git_out(dir, &["rev-parse", "--abbrev-ref", "HEAD"]) { let b = b.trim(); if !b.is_empty() && b != "HEAD" { g.branch = b.to_string(); } }
    g.commits = git_out(dir, &["log", "-5", "--format=%h %s"]).unwrap_or_default().lines().map(String::from).filter(|l| !l.trim().is_empty()).collect();
    g
}
/// The attachments the conversation still refers to (the page's `<!--vr-files [...] -->` blocks), as one block for the new
/// session's first message (the hub resolves it into paths the run may read; ones that are gone are left out there). None = none.
pub fn attachments(transcript: &[Value]) -> Option<String> {
    let mut seen: Vec<Value> = vec![];
    for t in transcript.iter().filter(|x| x["cls"] == json!("me")).filter_map(|x| x["text"].as_str()) {
        let tr = t.trim_end();
        let Some(i) = tr.rfind("<!--vr-files ") else { continue };
        let inner = tr[i + "<!--vr-files ".len()..].trim_end_matches("-->").trim();
        for f in serde_json::from_str::<Value>(inner).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default() {
            if f["id"].is_string() && !seen.iter().any(|x| x["id"] == f["id"]) { seen.push(f); }
        }
    }
    if seen.is_empty() { return None; }
    let n = seen.len(); if n > 20 { seen.drain(0..n - 20); }
    Some(format!("<!--vr-files {} -->", Value::Array(seen)))
}
/// What the handoff writer gets: the session, the person's focus, the conversation in short, the worktree, the files.
pub fn writer_input(title: &str, person: &str, digest: &str, git: &GitState, t: &Touched, focus: &str) -> String {
    format!("The Work session \"{title}\" of {person}, to be continued in a fresh session.\n\n{}THE CONVERSATION IN SHORT (its first message, then the newest turns; tool lines left out):\n==========\n{}\n==========\n\nTHE WORKTREE NOW (exact, from git):\n{}\n\nFILES AND PATHS THE SESSION'S RUNS TOUCHED (from its tool lines):\n{}\n\nWrite the handoff.",
        if focus.is_empty() { String::new() } else { format!("THE PERSON'S FOCUS FOR THIS HANDOFF (follow it):\n{focus}\n\n") },
        if digest.is_empty() { "(nothing said yet)" } else { digest }, git.text(), t.text())
}
/// The handoff without a model (the writer is not available or failed): the facts the hub has, in the same layout.
pub fn plain_handoff(why: &str, first: &str, digest: &str, git: &GitState, t: &Touched, focus: &str) -> String {
    let mut out = format!("## Handoff (made without a model: {why})\n\n## Goal\nThe session's first message, as the person wrote it:\n\n{}\n", quote(first));
    if !focus.is_empty() { out.push_str(&format!("\n## Focus for this handoff\n{focus}\n")); }
    out.push_str(&format!("\n## The conversation in short\n{}\n", if digest.is_empty() { "(nothing said yet)".to_string() } else { digest.to_string() }));
    out.push_str(&format!("\n## Current state\n{}\n", git.text()));
    out.push_str(&format!("\n## Important files and paths\n{}\n", t.text()));
    out.push_str("\n## Next steps\nNot written (no model made this handoff): read the conversation above and the worktree's state, say what you take the next step to be, and ask the person when it is unclear.");
    out
}
fn quote(t: &str) -> String { let t: String = t.chars().take(3000).collect(); t.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n") }
/// The exact facts appended to a handoff the model wrote (the model may shorten them; these never change).
pub fn facts(git: &GitState, t: &Touched) -> String { format!("## From the hub (exact, at the transfer)\n{}\n\nFiles and paths touched:\n{}", git.text(), t.text()) }
/// The new session's first message: the fixed first line, what to do with it, the handoff, the attachments still referred to.
pub fn first_message(old_title: &str, at_ms: u64, handoff: &str, files: Option<&str>) -> String {
    let when = chrono::DateTime::from_timestamp_millis(at_ms as i64).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default();
    let mut m = format!("{FIRST_LINE}{old_title} · {when}]\nThis session continues the Work session \"{old_title}\", which was handed over to you with the handoff below (written from its conversation and its worktree at the transfer; you have none of its history). Read it, then confirm in a few lines where things stand and what you will do next, and go on from there. Do not redo work it says is finished; check the worktree's state it names before you change anything.\n\n----- HANDOFF -----\n{handoff}\n----- END OF HANDOFF -----");
    if let Some(f) = files { m.push_str("\n\n"); m.push_str(f); }
    m
}

// ---------- the one-shot call ----------
/// The handoff from the session's own AI: (text, model) or why there is none.
async fn write(hub: &Arc<dyn AiHub>, a: &Account, st: &Value, input: &str) -> Result<(String, String), String> {
    let w = hub.work(); let ai = hub.ai();
    let Some(prov) = w.effective(ai, &a.username, &s(st, "provider")) else { return Err("no AI is available for this session".into()) };
    let kind = s(&prov, "kind");
    let model = hub.cfg().get("transferModel").and_then(Value::as_str).map(String::from).unwrap_or_else(|| { let m = s(st, "model"); if m.is_empty() { w.default_model_of(a) } else { m } });
    if kind == crate::openrouter::KIND { return openrouter(hub, a, &prov, &s(st, "model"), input).await; }
    let claude = matches!(kind.as_str(), "host" | "anthropic" | "claude-cli") || (kind == "host-claude" && s(&prov, "remoteKind") != "claude-cli-work");
    if !claude { return Err(format!("the session's AI ({}) offers no one-shot call to write it", if s(&prov, "label").is_empty() { kind.clone() } else { s(&prov, "label") })); }
    if (kind == "host" || kind == "claude-cli") && hub.root().join("ai-host.off").exists() { return Err("the host's AI is switched off right now".into()); }
    ai.scope_check(&a.username, &s(&prov, "id"), "work", &model).map_err(|m| m.trim_end_matches('.').to_string())?;
    let mut env: Vec<(String, String)> = Work::claude_env(&prov).unwrap_or_default();
    if kind == "host-claude" {
        let host_id = s(&prov, "hostId"); let prov_id = s(&prov, "id").split_once('/').map(|x| x.1.to_string()).unwrap_or_default();
        if !ai.online(&host_id) { return Err(format!("{} is offline right now", s(&prov, "nickname"))); }
        let tok = w.llm_token(&a.username, &host_id, &prov_id);
        env.extend([("ANTHROPIC_BASE_URL".into(), format!("http://127.0.0.1:{}/llm/{tok}", hub.cfg().port)), ("ANTHROPIC_AUTH_TOKEN".into(), "viberoller-relay".into()), ("ANTHROPIC_API_KEY".into(), String::new()), ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into())]);
    }
    let dir = std::env::temp_dir().join(format!("vr-transfer-{}", pgm_core_internal::crypto::random_hex(8)));
    std::fs::create_dir_all(&dir).map_err(|_| "no folder to write it in".to_string())?;
    struct Gone(std::path::PathBuf);
    impl Drop for Gone { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _gone = Gone(dir.clone());
    let pre: Vec<String> = hub.cfg().get("claudeArgs").and_then(Value::as_array).map(|x| x.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut cmd = tokio::process::Command::new(hub.cfg().get("claudeCommand").and_then(Value::as_str).unwrap_or("claude"));
    cmd.args(&pre).args(args(&model)).current_dir(&dir).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    for (k, v) in &env { cmd.env(k, v); }
    #[cfg(windows)]
    { cmd.creation_flags(0x0800_0000); }
    let mut child = cmd.spawn().map_err(|e| format!("Claude could not be started ({e})"))?;
    if let Some(mut i) = child.stdin.take() { let p = input.to_string(); tokio::spawn(async move { let _ = i.write_all(p.as_bytes()).await; let _ = i.shutdown().await; }); }
    let out = match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await { Ok(Ok(o)) => o, Ok(Err(e)) => return Err(format!("Claude stopped ({e})")), Err(_) => return Err("writing it took too long".into()) };
    match crate::enhance::result_of(&String::from_utf8_lossy(&out.stdout)) {
        Some((t, false)) if !t.trim().is_empty() => Ok((t.trim().to_string(), model)),
        Some((t, _)) => Err(format!("Claude could not write it{}", if t.trim().is_empty() { String::new() } else { format!(": {}", t.trim().chars().take(200).collect::<String>()) })),
        None => Err("Claude gave no answer".into()),
    }
}
/// The `claude` arguments of the throw-away handoff writer (the record goes on stdin): no tools, no MCP, no session kept.
pub fn args(model: &str) -> Vec<String> {
    let mut v: Vec<String> = ["-p", "--output-format", "json", "--no-session-persistence", "--strict-mcp-config", "--disable-slash-commands", "--tools", "", "--permission-mode", "default", "--system-prompt", SYSTEM].iter().map(|x| x.to_string()).collect();
    if !model.is_empty() { v.push("--model".into()); v.push(model.into()); }
    v
}
/// The handoff through the session's OpenRouter provider (its key, routing, budget and usage: the gateway's chat call).
async fn openrouter(hub: &Arc<dyn AiHub>, a: &Account, prov: &Value, state_model: &str, input: &str) -> Result<(String, String), String> {
    use http_body_util::BodyExt;
    if let Some(m) = crate::openrouter::guard(&**hub, &a.username, prov) { return Err(m.trim_end_matches('.').to_string()); }
    let ps = crate::openrouter::pairs_of(prov);
    let Some(pr) = ps.iter().find(|x| s(x, "id") == state_model).or(ps.first()).cloned() else { return Err("the OpenRouter provider offers no models".into()) };
    let routed = s(&pr, "id");
    let key = crate::openai::KeyCtx { id: "work-transfer".into(), label: "Work transfer".into(), named: true, ..Default::default() };
    let body = json!({ "model": routed, "stream": false, "max_tokens": 6000, "messages": [{ "role": "system", "content": SYSTEM }, { "role": "user", "content": input }] });
    let resp = tokio::time::timeout(TIMEOUT, crate::gateway::chat_raw(hub, &a.username, &key, prov, &routed, body)).await.map_err(|_| "writing it took too long".to_string())?;
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.map(|c| c.to_bytes()).map_err(|_| "the answer broke off".to_string())?;
    let j: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status != 200 { return Err(format!("OpenRouter answered {status}{}", j["error"]["message"].as_str().map(|m| format!(": {}", m.chars().take(200).collect::<String>())).unwrap_or_default())); }
    let text = j["choices"][0]["message"]["content"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() { return Err("the model gave an empty answer".into()); }
    Ok((text, s(&pr, "label")))
}

// ---------- the route ----------
/// POST /api/work/sessions/transfer.
pub async fn route(hub: &Arc<dyn AiHub>, a: &Account, r: &Req) -> Resp {
    let w = hub.work();
    let b = r.json();
    let Some(sx) = sessions::resolve(a, &sessions::wanted(r, &b)) else { return answer(404, json!({ "error": "No such session." })) };
    if w.busy_key(&sx.key) { return answer(409, json!({ "error": "That session is still working: wait for its reply (or stop it), then transfer it.", "working": true })); }
    if !running().lock().unwrap_or_else(|e| e.into_inner()).insert(sx.key.clone()) { return answer(409, json!({ "error": "That session is being transferred already: wait for it." })); }
    struct Done(String);
    impl Drop for Done { fn drop(&mut self) { running().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0); } }
    let _done = Done(sx.key.clone());
    if w.busy_key(&sx.key) { return answer(409, json!({ "error": "That session is still working: wait for its reply (or stop it), then transfer it.", "working": true })); }
    let ix = sessions::index(a); let meta = ix["sessions"][&sx.sid].clone();
    let to_now = s(&meta["transferredTo"], "id");
    if !to_now.is_empty() && ix["sessions"][&to_now].is_object() { return answer(409, json!({ "error": format!("This session was transferred already, to \"{}\": transfer that one instead.", s(&ix["sessions"][&to_now], "title")) })); }
    let cur = sessions::scurrent(a, &sx); let st = sessions::sstate(w, a, &sx);
    let tr: Vec<Value> = cur["transcript"].as_array().cloned().unwrap_or_default();
    if !tr.iter().any(|t| t["cls"] == json!("me") || t["cls"] == json!("claude")) { return answer(400, json!({ "error": "There is nothing to transfer yet: this session has no conversation. Just keep working in it." })); }
    let focus: String = s(&b, "focus").trim().chars().take(MAX_FOCUS).collect();
    let keep = b["keepWorktree"] != json!(false);
    let none = w.is_none(a, &sx);
    let project = w.project_in(a, &sx);
    let old_title = { let t = s(&meta, "title"); if t.is_empty() { let t2 = s(&cur, "archiveTitle"); if t2.is_empty() { "Session".to_string() } else { t2 } } else { t } };

    // 1. the state of its worktree (git) and the paths its runs touched
    let wd = { let (w2, a2, sx2) = (w.clone(), a.clone(), sx.clone()); tokio::task::spawn_blocking(move || w2.session_wt(&a2, &sx2)).await.unwrap_or_else(|e| Err(e.to_string())) };
    let git = match &wd {
        Ok(wd) => { let (pj, dir, br) = (wd.project.clone(), wd.dir.clone(), wd.branch.clone()); tokio::task::spawn_blocking(move || git_state(&pj, &dir, &br)).await.unwrap_or_default() }
        Err(e) => GitState { project: project.clone(), note: format!("(the worktree could not be read: {e})"), ..Default::default() },
    };
    let t = touched(&tr);
    let budget = hub.cfg().get("transferContextChars").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(CONTEXT_CHARS);
    let digest = crate::enhance::digest(&tr, budget);
    let first = tr.iter().find(|x| x["cls"] == json!("me")).and_then(|x| x["text"].as_str()).map(|x| x.trim().to_string()).unwrap_or_default();

    // 2. the handoff: the session's own AI, else a plain one
    let input = writer_input(&old_title, &a.display, &digest, &git, &t, &focus);
    let (text, by, why, model) = match write(hub, a, &st, &input).await {
        Ok((text, model)) => (format!("{text}\n\n{}", facts(&git, &t)), "model", String::new(), model),
        Err(why) => (plain_handoff(&why, &first, &crate::enhance::digest(&tr, budget.min(12_000)), &git, &t, &focus), "plain", why, String::new()),
    };

    // 3. the new session: the same settings, its worktree, the links both ways
    if w.busy_key(&sx.key) { return answer(409, json!({ "error": "That session started working meanwhile: transfer it once it is done." })); }
    let mut ns = Map::new();
    for k in ["provider", "model", "project", "noProject", "sessionHarness"] { if !st[k].is_null() { ns.insert(k.into(), st[k].clone()); } }
    let tree: Option<String> = if keep && !none { Some(if sx.own { sx.tree().to_string() } else { sessions::HOME_TREE.to_string() }) } else { None };
    let wmeta = match (&tree, &wd) { (Some(_), Ok(wd)) => json!({ "path": wd.dir.display().to_string(), "branch": wd.branch, "project": wd.project }), _ => Value::Null };
    let (to, title) = match sessions::make_transfer(a, &sx, &Value::Object(ns), tree.clone(), &wmeta) { Ok(x) => x, Err(e) => return answer(if e.starts_with("No such") { 404 } else { 409 }, json!({ "error": e })) };
    let Some(nx) = sessions::resolve(a, &to) else { return answer(500, json!({ "error": "The new session went missing." })) };

    // 4. board tasks the old session's runs left claimed: a handoff note naming the new session
    let tasks: Vec<Value> = meta["heldTasks"].as_array().cloned().unwrap_or_default();
    let board = if tasks.is_empty() { vec![] } else { hub.session_handoff(json!({ "account": a.username, "from": sx.sid, "fromTitle": old_title, "to": to, "toTitle": title, "project": if none { String::new() } else { project.clone() }, "branch": git.branch, "changes": git.changes(), "tasks": tasks, "sameTree": tree.is_some() })) };

    // 5. its first message (the handoff), and what was still queued in the old one behind it
    let message = first_message(&old_title, now_ms(), &text, attachments(&tr).as_deref());
    let mut queue: Vec<Value> = vec![];
    if let Some(sn) = cur["sendNow"].as_str() { queue.push(json!(sn)); }
    queue.extend(cur["queue"].as_array().cloned().unwrap_or_default());
    if !queue.is_empty() { let mut oc = cur.clone(); oc["queue"] = json!([]); oc["sendNow"] = Value::Null; sessions::save_scurrent(a, &sx, &mut oc); }
    let mut nc = crate::work::current_of(&json!({}));
    nc["transcript"] = json!([{ "cls": "me", "text": message }]); nc["queue"] = json!(queue);
    sessions::save_scurrent(a, &nx, &mut nc);

    // 6. it runs at once: POST /api/chat's own code, as the person
    let req = Req { method: hyper::Method::POST, path: "/api/chat".into(), query: String::new(), headers: hyper::HeaderMap::new(), body: bytes::Bytes::from(json!({ "message": message, "session": to }).to_string()), peer: std::net::SocketAddr::from(([127, 0, 0, 1], 0)) };
    let resp = crate::work::chat(hub, a, &req).await;
    let (mut run, mut error, status) = (String::new(), String::new(), resp.status().as_u16());
    if status == 200 {
        use http_body_util::BodyExt;
        let mut body = resp.into_body(); let mut buf = String::new();
        'read: while let Some(fr) = body.frame().await {
            let Ok(fr) = fr else { break };
            let Ok(data) = fr.into_data() else { continue };
            buf.push_str(&String::from_utf8_lossy(&data));
            while let Some(i) = buf.find('\n') {
                let line: String = buf.drain(..=i).collect();
                let Ok(ev) = serde_json::from_str::<Value>(line.trim()) else { continue };
                match ev["type"].as_str().unwrap_or("") { "bridge_run" => { run = s(&ev, "id"); break 'read; } "bridge_error" => { error = s(&ev, "text"); } "bridge_done" => break 'read, _ => {} }
            }
        }
        // (the rest of the run goes on without this answer: the page joins it through /api/work/attach)
    } else {
        use http_body_util::BodyExt;
        let j: Value = resp.into_body().collect().await.ok().and_then(|c| serde_json::from_slice(&c.to_bytes()).ok()).unwrap_or(Value::Null);
        error = s(&j, "error");
        if status == 429 {
            // over the session limit: the handoff waits in the new session's queue and goes when a slot frees (as a 429 always does)
            let mut c = sessions::scurrent(a, &nx); let mut q = vec![json!(message)]; q.extend(c["queue"].as_array().cloned().unwrap_or_default());
            c["transcript"] = json!([]); c["queue"] = json!(q); sessions::save_scurrent(a, &nx, &mut c);
        }
    }
    if !run.is_empty() { let mut c = sessions::scurrent(a, &nx); c["run"] = json!({ "id": run, "n": 0 }); sessions::save_scurrent(a, &nx, &mut c); }
    let nm = sessions::index(a)["sessions"][&to].clone();
    let worktree = if nm["worktree"].is_object() { nm["worktree"].clone() } else { wmeta.clone() };
    pgm_core_internal::obs::info("work", "session transferred", json!({ "user": a.username, "from": sx.sid, "to": to, "by": by, "keptTree": tree.is_some(), "run": run, "board": board }));
    let mut h = json!({ "text": text, "by": by });
    if !why.is_empty() { h["why"] = json!(why); }
    if !model.is_empty() { h["model"] = json!(model); }
    answer(200, json!({ "ok": true, "from": sx.sid, "to": to, "title": title, "handoff": h, "worktree": { "path": worktree["path"], "branch": worktree["branch"], "project": if none { pgm_collab_internal::worktree::NO_PROJECT.to_string() } else { project }, "kept": tree.is_some() },
        "run": if run.is_empty() { Value::Null } else { json!(run) }, "started": !run.is_empty(), "queued": status == 429, "error": if error.is_empty() { Value::Null } else { json!(error) }, "board": board }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tool(x: &str) -> Value { json!({ "cls": "tool", "text": x }) }
    #[test]
    fn the_paths_come_from_the_tool_lines_edited_and_read_once_each() {
        let tr = vec![json!({ "cls": "me", "text": "Read  not/a/tool.rs" }), tool("Read  src/a.rs"), tool("Edit  src/b.rs"), tool("Bash  cargo build --release"), tool("Grep  fn main"),
            tool("Read  src/b.rs"), tool("your PC › write  notes/plan.md"), tool("Write  /tmp/x.txt"), tool("Read  src/a.rs"), tool("your PC › read  todo.txt"), tool("mcp__board__task_get"), tool("file_edit  crates/c.rs"), tool("Glob  **/*.rs")];
        let t = touched(&tr);
        assert_eq!(t.edited, vec!["src/b.rs", "notes/plan.md", "/tmp/x.txt", "crates/c.rs"]);
        assert_eq!(t.read, vec!["src/a.rs", "todo.txt"], "a file also edited is listed once, as edited; the newest touch last");
        assert!(t.text().contains("Edited / written:\n- src/b.rs") && t.text().contains("Read:\n- src/a.rs"));
        assert_eq!(touched(&[tool("Bash  ls")]).text(), "None seen in the session's tool lines.");
        let many: Vec<Value> = (0..60).map(|i| tool(&format!("Edit  f{i}.rs"))).collect();
        let m = touched(&many); assert_eq!(m.edited.len(), 40); assert_eq!(m.edited.last().map(String::as_str), Some("f59.rs"));
    }
    #[test]
    fn the_git_state_names_branch_changes_and_commits() {
        let g = GitState { project: "VibeRoller".into(), dir: "/w/sponge.abc".into(), branch: "work/sponge.abc".into(), git: true, status: vec![" M web/index.html".into(), "?? new.txt".into()], commits: vec!["1a2b3c4 the last".into()], note: String::new() };
        let t = g.text();
        assert!(t.contains("Project: VibeRoller") && t.contains("Worktree: /w/sponge.abc") && t.contains("Branch: work/sponge.abc"));
        assert!(t.contains("Uncommitted changes (git status --porcelain, 2):\n     M web/index.html\n    ?? new.txt"));
        assert!(t.contains("Last commits:\n    1a2b3c4 the last"));
        assert_eq!(g.changes(), Some(2));
        let clean = GitState { git: true, ..g.clone() }; let clean = GitState { status: vec![], ..clean };
        assert!(clean.text().contains("Uncommitted changes: none"));
        let none = GitState { project: pgm_collab_internal::worktree::NO_PROJECT.into(), dir: "/scratch/sponge".into(), ..Default::default() };
        assert!(none.text().starts_with("No project: the session works in the person's scratch folder /scratch/sponge") && none.changes().is_none());
        // a real checkout: an uncommitted file is seen
        let dir = std::env::temp_dir().join(format!("vr-transfer-git-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let ok = std::process::Command::new("git").arg("-C").arg(&dir).args(["init", "-q"]).status().map(|s| s.success()).unwrap_or(false);
        if ok { std::fs::write(dir.join("left.txt"), "x").unwrap(); let g = git_state("P", &dir, "work/u"); assert!(g.git && g.status.iter().any(|l| l.ends_with("left.txt"))); }
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn the_handoff_parts_and_the_first_message() {
        let g = GitState { project: "P".into(), git: true, status: vec!["?? a".into()], ..Default::default() };
        let t = Touched { edited: vec!["x.rs".into()], read: vec![] };
        let i = writer_input("My work", "Sponge", "Person: build it", &g, &t, "keep the API part");
        assert!(i.contains("THE PERSON'S FOCUS FOR THIS HANDOFF (follow it):\nkeep the API part") && i.contains("Person: build it") && i.contains("?? a") && i.contains("- x.rs"));
        assert!(!writer_input("t", "S", "", &g, &t, "").contains("FOCUS"));
        let p = plain_handoff("the summariser failed", "build the thing", "Person: build the thing", &g, &t, "only X");
        assert!(p.starts_with("## Handoff (made without a model: the summariser failed)") && p.contains("> build the thing") && p.contains("## Focus for this handoff\nonly X") && p.contains("## Current state\nProject: P") && p.contains("## Next steps"));
        let m = first_message("My work", 1_794_000_000_000, "## Goal\nx", Some("<!--vr-files [{\"id\":\"f1\"}] -->"));
        assert!(m.starts_with("[transfer from session My work · 2026-") && m.lines().next().unwrap().ends_with(" UTC]"));
        assert!(m.contains("confirm in a few lines where things stand") && m.contains("Do not redo work") && m.contains("----- HANDOFF -----\n## Goal\nx\n----- END OF HANDOFF -----"));
        assert!(m.ends_with("<!--vr-files [{\"id\":\"f1\"}] -->"));
        let a = args("m1"); assert!(a.windows(2).any(|w| w[0] == "--tools" && w[1].is_empty()) && a.contains(&"--no-session-persistence".to_string()) && !a.iter().any(|x| x == "--resume" || x == "--mcp-config"));
    }
    #[test]
    fn the_attachments_still_referred_to_come_along_once() {
        let tr = vec![json!({ "cls": "me", "text": "look\n\n<!--vr-files [{\"id\":\"aa\",\"name\":\"a.png\"}] -->" }), json!({ "cls": "claude", "text": "<!--vr-files [{\"id\":\"zz\"}] -->" }),
            json!({ "cls": "me", "text": "again <!--vr-files [{\"id\":\"aa\"},{\"id\":\"bb\"}] -->" }), json!({ "cls": "me", "text": "plain" })];
        let m = attachments(&tr).unwrap();
        let inner: Value = serde_json::from_str(m.trim_start_matches("<!--vr-files ").trim_end_matches(" -->")).unwrap();
        assert_eq!(inner.as_array().unwrap().iter().map(|x| s(x, "id")).collect::<Vec<_>>(), vec!["aa", "bb"]);
        assert!(attachments(&[json!({ "cls": "me", "text": "none" })]).is_none());
    }
}
