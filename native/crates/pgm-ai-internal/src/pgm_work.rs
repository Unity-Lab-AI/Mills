//! Work on a PGM session (T-376 stage 3; team note plans/jcode-pgm-server-stage3.md F.3 and F.4): with a Jcode PGM Server next
//! to the hub (`pgmServer.url`), a Work session whose provider is the team's or the person's own OpenRouter is a PGM session.
//! The hub runs nothing for it: the server owns the session (one `ps-…` id across the website and jcode) and runs it, or jcode on
//! the person's computer does. The hub calls the server's `/__srv/*` (loopback, `x-pgm-secret`):
//! - the first message creates the PGM session (`POST /__srv/sessions`: user, title, project, branch, pair, the session's own
//!   worktree); every message is `POST /__srv/sessions/<id>/message` (worktree, project, branch);
//! - the server's events (`GET /__srv/sessions/<id>/events`, SSE; `GET /__srv/sessions/<id>?after=` to catch up, and as the
//!   fallback when the stream is not there) become the page's Work events: an assistant turn its text, a tool its tool_use and
//!   tool_result, a note or a turn typed in jcode one line; the end of the turn a result, so it looks like any Work run;
//! - `routed: "device"` says "Sent to jcode on <device>" and the run goes on showing jcode's turns; a handover, a continue from
//!   the last push and a 409 are plain lines; Stop is `POST /__srv/sessions/<id>/stop`;
//! - the session keeps its PGM id in its state (`pgm: {id, seq}`, seq = the last event shown);
//! - the list (GET /api/work/sessions) gets the person's PGM sessions from the server (`GET /__srv/sessions?user=`) next to its
//!   report; one opened from it gets a Work session made on the fly, and its transcript is followed live
//!   (`POST /api/work/sessions/pgm/open`).
//! Without a server nothing here runs: OpenRouter Work is Jcode PGM through the hub's own relay, as before.
use bytes::Bytes;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};

use crate::hub::{answer, AiHub, Resp};
use crate::sessions::{self, Sx};
use crate::work::Work;
use pgm_collab_internal::worktree::{Wt, HUB_PROJECT, NO_PROJECT};
use pgm_core_internal::json::s;
use pgm_core_internal::now_ms;
use pgm_identity_internal::Account;

pub const SECRET_HEADER: &str = "x-pgm-secret";
/// The source the server's sessions carry in the list (its report says the same).
pub const SOURCE: &str = "pgm-server";
pub const NO_SERVER: &str = "this hub has no Jcode PGM Server";
/// A device-run turn counts as over when jcode sent nothing for this long after its answer (config pgmServer.deviceQuietSec)
/// and the server said nothing about the session's run.
const DEVICE_QUIET_SEC: u64 = 20;

#[derive(Clone)]
pub struct Srv { url: String, secret: String }
fn srv_of(hub: &dyn AiHub) -> Option<Srv> { hub.pgm_server().map(|(url, secret)| Srv { url, secret }) }
fn http() -> &'static reqwest::Client {
    static C: LazyLock<reqwest::Client> = LazyLock::new(|| reqwest::Client::builder().connect_timeout(Duration::from_secs(3)).no_proxy().redirect(reqwest::redirect::Policy::none()).build().expect("http"));
    &C
}
/// A PGM session id as the server makes them ("ps-" + a short id).
fn valid_ps(id: &str) -> bool { id.len() <= 64 && id.strip_prefix("ps-").is_some_and(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')) }
fn q(x: &str) -> String { x.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect() }
/// One call to the server: (status, JSON body); Err = it could not be reached, in plain words.
async fn call(sv: &Srv, method: reqwest::Method, path: &str, body: Option<&Value>, wait: Duration) -> Result<(u16, Value), String> {
    let mut rq = http().request(method, format!("{}{path}", sv.url)).header(SECRET_HEADER, &sv.secret).timeout(wait);
    if let Some(b) = body { rq = rq.json(b); }
    match rq.send().await {
        Ok(r) => { let st = r.status().as_u16(); let t = r.text().await.unwrap_or_default(); Ok((st, serde_json::from_str(&t).unwrap_or_else(|_| json!({ "error": t.trim().chars().take(300).collect::<String>() })))) }
        Err(e) => Err(if e.is_timeout() { "it did not answer in time".into() } else if e.is_connect() { "it is not running (nothing answers at its address)".into() } else { "the connection broke off".into() }),
    }
}
/// The plain reason in an error answer (`{error}` or OpenRouter's `{error: {message}}`).
fn err_text(v: &Value, st: u16) -> String {
    v["error"].as_str().map(String::from).or_else(|| v["error"]["message"].as_str().map(String::from)).filter(|t| !t.trim().is_empty()).unwrap_or_else(|| format!("the Jcode PGM Server answered {st}"))
}
fn one_line(v: Value) -> Resp { hyper::Response::builder().status(200).header("content-type", "application/x-ndjson").body(pgm_net_internal::http::full(v.to_string() + "\n")).expect("response") }
fn info(t: impl Into<String>) -> Value { json!({ "type": "bridge_info", "text": t.into() }) }
fn error(t: impl Into<String>) -> Value { json!({ "type": "bridge_error", "text": t.into() }) }

// ---------- the session's PGM id ----------
/// The PGM session a Work session is ("" = none yet) and the last of its events shown.
fn pgm_of(w: &Work, a: &Account, sx: &Sx) -> (String, u64) { let st = sessions::sstate(w, a, sx); (s(&st["pgm"], "id"), st["pgm"]["seq"].as_u64().unwrap_or(0)) }
fn set_pgm(w: &Work, a: &Account, sx: &Sx, id: &str, seq: u64) { let (id, seq) = (id.to_string(), seq); sessions::supdate(w, a, sx, |m| { m.insert("pgm".into(), json!({ "id": id, "seq": seq })); }); }
/// The person's Work session that is this PGM session.
fn session_with(w: &Work, a: &Account, pid: &str) -> Option<Sx> {
    sessions::ids(a).into_iter().filter_map(|sid| sessions::resolve(a, &sid)).find(|sx| pgm_of(w, a, sx).0 == pid)
}
/// The Forgejo repo ("owner/name") a worktree pushes to, "" when it is not on the team's Forgejo.
fn repo_of(w: &Work, wd: &Wt) -> String {
    if wd.project == NO_PROJECT { return String::new(); }
    let url = pgm_collab_internal::worktree::git(&wd.dir, &["remote", "get-url", "origin"]).unwrap_or_default();
    let host = w.forgejo_url(); let host = host.split("://").nth(1).unwrap_or(&host).trim_end_matches('/');
    let Some(i) = url.find(host).filter(|_| !host.is_empty()) else { return String::new() };
    let path = url[i + host.len()..].trim_start_matches([':', '/']).trim().trim_end_matches('/').trim_end_matches(".git");
    let segs: Vec<&str> = path.split('/').filter(|x| !x.is_empty()).collect();
    if segs.len() >= 2 { format!("{}/{}", segs[segs.len() - 2], segs[segs.len() - 1]) } else { String::new() }
}
/// Where a PGM session runs now, in words: "running in jcode on <device>" / "running on the server".
pub fn where_text(state: &Value) -> String {
    let r = &state["runner"];
    let dev = { let d = s(r, "device"); if d.is_empty() { s(state, "device") } else { d } };
    if s(r, "kind") == "device" || (r.is_null() && !dev.is_empty()) { format!("running in jcode on {}", if dev.is_empty() { "your computer".to_string() } else { dev }) } else { "running on the server".into() }
}
/// A session's state from an answer that has it as `session` or at its top level.
fn state_in(v: &Value) -> Value { if v["session"].is_object() { v["session"].clone() } else { v.clone() } }
fn running(state: &Value) -> Option<bool> { state["running"].as_bool().or_else(|| state["busy"].as_bool()) }

// ---------- the server's events onto the page's ----------
/// What one event of the server (F.1: `{seq, at, from, kind, text, tool?}`) shows on the page: Work events of the usual shape.
/// `skip` = the text of the website's own message (the page shows it already): its first echo is left out.
pub fn map_event(ev: &Value, skip: &mut Option<String>) -> Vec<Value> {
    let text = s(ev, "text"); let from = s(ev, "from"); let seq = ev["seq"].as_u64().unwrap_or(0);
    let tag = json!({ "seq": seq, "from": from });
    let device = from.strip_prefix("device:").map(String::from);
    match s(ev, "kind").as_str() {
        "assistant" if !text.trim().is_empty() => vec![json!({ "type": "assistant", "harness": "jcode", "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] }, "pgm": tag })],
        "tool" => {
            let t = &ev["tool"]; let id = format!("pgm-{seq}");
            let name = t["name"].as_str().or(t.as_str()).filter(|n| !n.is_empty()).unwrap_or("tool").to_string();
            let input = if t["input"].is_object() { t["input"].clone() } else { json!({ "summary": text }) };
            let out = t["output"].as_str().map(String::from).unwrap_or_else(|| text.clone());
            vec![json!({ "type": "assistant", "harness": "jcode", "message": { "role": "assistant", "content": [{ "type": "tool_use", "id": id, "name": name, "input": input }] }, "pgm": tag }),
                 json!({ "type": "user", "harness": "jcode", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": id, "content": out, "is_error": t["error"] == json!(true) }] }, "pgm": tag })]
        }
        "user" => {
            if from == "website" && skip.as_deref() == Some(text.trim()) { *skip = None; return vec![]; }
            match device { Some(d) => vec![info(format!("You in jcode on {d}: {text}"))], None if from == "website" => vec![info(format!("You (website): {text}"))], None => vec![info(text)] }
        }
        _ if !text.trim().is_empty() => vec![info(match device { Some(d) => format!("jcode on {d}: {text}"), None => text })],
        _ => vec![],
    }
}
/// The line for the server's answer to a message (None = nothing to say; Err = the plain refusal).
pub fn routed_line(st: u16, v: &Value) -> Result<Option<String>, String> {
    if !(200..300).contains(&st) { return Err(err_text(v, st)); }
    let dev = s(v, "device");
    Ok(match s(v, "routed").as_str() {
        "device" => Some(format!("Sent to jcode on {}: it works on it there, and its turns appear here.", if dev.is_empty() { "your computer".into() } else { dev })),
        _ => {
            let h = &v["handover"];
            let mut l: Vec<String> = vec![];
            if h.is_object() {
                let d = { let x = s(h, "device"); if x.is_empty() { dev.clone() } else { x } };
                l.push(format!("jcode on {} handed its unfinished work over: committed and pushed {}{}; the server continues from it.", if d.is_empty() { "your computer".into() } else { d }, s(h, "branch"), { let c = s(h, "commit"); if c.is_empty() { String::new() } else { format!(" at {}", c.chars().take(12).collect::<String>()) } }));
            }
            if s(v, "from") == "last-push" { l.push({ let n = s(v, "note"); if n.is_empty() { "The newest work is on your computer, which is offline; the server continued from the last push.".into() } else { cap_first(&n) } }); }
            else if !s(v, "note").is_empty() { l.push(cap_first(&s(v, "note"))); }
            if l.is_empty() { None } else { Some(l.join(" ")) }
        }
    })
}
fn cap_first(t: &str) -> String { let mut c = t.chars(); match c.next() { Some(f) => f.to_uppercase().collect::<String>() + c.as_str(), None => String::new() } }

// ---------- the runs that follow a PGM session ----------
/// A run's controls: messages for it to send, and Stop.
struct Ctl { inbox: mpsc::UnboundedSender<String>, stop: Arc<Notify> }
static CTL: LazyLock<Mutex<HashMap<String, Ctl>>> = LazyLock::new(Mutex::default);

/// Stop (the page's Stop, the liveness check): the server is told, and the run that follows the session ends.
pub fn stop(hub: &dyn AiHub, user: &str, pid: &str, run: &str) {
    if let Some(c) = CTL.lock().unwrap_or_else(|e| e.into_inner()).get(run) { c.stop.notify_one(); }
    let Some(sv) = srv_of(hub) else { return };
    let (user, pid) = (user.to_string(), pid.to_string());
    tokio::spawn(async move { if let Err(e) = call(&sv, reqwest::Method::POST, &format!("/__srv/sessions/{}/stop", q(&pid)), Some(&json!({ "user": user })), Duration::from_secs(10)).await { eprintln!("[work] pgm: stop of {pid} did not reach the Jcode PGM Server: {e}"); } });
}

/// The server's frames for one session: its events and state changes, from the SSE stream (and from polling while that is not
/// there). Ends when the receiver is dropped.
fn frames(sv: Srv, user: String, pid: String, after: u64) -> mpsc::UnboundedReceiver<Value> {
    let (tx, rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut last = after;
        let mut sse_ok = true;
        loop {
            if tx.is_closed() { return; }
            if sse_ok {
                let rq = http().get(format!("{}/__srv/sessions/{}/events?user={}&after={last}", sv.url, q(&pid), q(&user))).header(SECRET_HEADER, &sv.secret).header("accept", "text/event-stream");
                match rq.send().await {
                    Ok(r) if r.status().is_success() && r.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|c| c.contains("event-stream")) => {
                        if tx.send(json!({ "type": "_connected" })).is_err() { return; }
                        let mut body = r.bytes_stream(); let mut buf: Vec<u8> = vec![]; let mut data = String::new();
                        loop {
                            let chunk: Option<Result<Bytes, reqwest::Error>> = tokio::select! { _ = tx.closed() => return, c = body.next() => c };
                            let Some(Ok(c)) = chunk else { break };
                            buf.extend_from_slice(&c);
                            while let Some(i) = buf.iter().position(|b| *b == b'\n') {
                                let line: Vec<u8> = buf.drain(..=i).collect();
                                let line = String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string();
                                if line.is_empty() {
                                    if !data.is_empty() { if let Ok(v) = serde_json::from_str::<Value>(&data) { if let Some(n) = v["seq"].as_u64() { last = last.max(n); } if tx.send(v).is_err() { return; } } data.clear(); }
                                } else if let Some(d) = line.strip_prefix("data:") { if !data.is_empty() { data.push('\n'); } data.push_str(d.strip_prefix(' ').unwrap_or(d)); }
                            }
                        }
                    }
                    Ok(r) if r.status().as_u16() == 404 || r.status().as_u16() == 405 || r.status().is_success() => { sse_ok = false; } // no stream here: poll
                    _ => {}
                }
            }
            // catch up (after a stream that ended, and every second while there is none)
            if let Ok((200, v)) = call(&sv, reqwest::Method::GET, &format!("/__srv/sessions/{}?user={}&after={last}", q(&pid), q(&user)), None, Duration::from_secs(10)).await {
                for e in v["events"].as_array().cloned().unwrap_or_default() { if let Some(n) = e["seq"].as_u64() { if n <= last { continue; } last = n; } if tx.send(e).is_err() { return; } }
                let mut st = state_in(&v); if let Some(o) = st.as_object_mut() { o.remove("events"); o.insert("type".into(), json!("session")); }
                if tx.send(st).is_err() { return; }
            }
            tokio::select! { _ = tx.closed() => return, _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
        }
    });
    rx
}

/// One run following PGM session `pid` for Work session `sx`, until its turn is over (or Stop): the history since `after`, then
/// each message from the inbox sent, and the server's frames shown as they come.
#[allow(clippy::too_many_arguments)]
async fn follow(hub: Arc<dyn AiHub>, a: Account, sx: Sx, run: String, sv: Srv, pid: String, mut after: u64, mut inbox: mpsc::UnboundedReceiver<String>, stopper: Arc<Notify>, extra: (String, String, String)) {
    let w = hub.work().clone();
    let (dir, project, branch) = extra;
    let emit = |ev: Value| w.on_event(&*hub, &a, &sx, ev, "openrouter");
    let line = |ev: Value| w.emit_key(&sx.key, ev);
    let user = a.username.clone();
    let mut fr = frames(sv.clone(), user.clone(), pid.clone(), after);
    let quiet = pgm_core_internal::config::Config::load(hub.root()).get("pgmServer").and_then(|c| c["deviceQuietSec"].as_u64()).filter(|n| *n >= 1).unwrap_or(DEVICE_QUIET_SEC) * 1000;
    let started = now_ms();
    // what this run waits for: a message sent and its turn not over yet; the turn's last answer (the result's text)
    let (mut waiting, mut active, mut device, mut last_text, mut last_at, mut skip, mut shown) = (false, false, false, String::new(), 0u64, None::<String>, 0u32);
    // the history since the last event shown, and where it runs
    let mut is_running;
    match call(&sv, reqwest::Method::GET, &format!("/__srv/sessions/{}?user={}&after={after}", q(&pid), q(&user)), None, Duration::from_secs(15)).await {
        Ok((200, v)) => {
            for e in v["events"].as_array().cloned().unwrap_or_default() {
                let n = e["seq"].as_u64().unwrap_or(0); if n != 0 && n <= after { continue; } after = after.max(n);
                for x in map_event(&e, &mut skip) { emit(x); }
            }
            let st = state_in(&v); is_running = running(&st).unwrap_or(false);
            line(info(format!("PGM session {pid}: {}.", where_text(&st))));
        }
        Ok((404, _)) => { line(error(format!("The Jcode PGM Server does not know this session ({pid}) any more. \"New chat\" starts a new one."))); return end(&hub, &a, &sx, &run, &pid, after, json!(1)); }
        Ok((st, v)) => { line(error(format!("The Jcode PGM Server refused: {}", err_text(&v, st)))); return end(&hub, &a, &sx, &run, &pid, after, json!(1)); }
        Err(e) => { line(error(format!("The Jcode PGM Server could not be reached: {e}."))); return end(&hub, &a, &sx, &run, &pid, after, json!(1)); }
    }
    set_pgm(&w, &a, &sx, &pid, after);
    if is_running { active = true; }
    let mut sent_any = false;
    // a message goes once the stream is there (its state changes are not replayed), or after 2 s without one
    let mut ready = false; let mut last_poll = now_ms();
    let mut batch: Vec<Value> = vec![]; let mut polled: Option<bool> = None;
    loop {
        // nothing to wait for: a run that only follows the session ends when it is not running (and no message is on its way)
        if !waiting && !is_running && !sent_any_pending(&inbox) { break; }
        if !ready && now_ms().saturating_sub(started) > 2000 { ready = true; }
        let tick = tokio::time::sleep(Duration::from_millis(500));
        tokio::select! {
            _ = stopper.notified() => { set_pgm(&w, &a, &sx, &pid, after); return end(&hub, &a, &sx, &run, &pid, after, Value::Null); }
            m = inbox.recv(), if ready => {
                let Some(text) = m else { continue };
                let body = json!({ "user": user, "text": text, "worktree": dir, "project": project, "branch": branch });
                match call(&sv, reqwest::Method::POST, &format!("/__srv/sessions/{}/message", q(&pid)), Some(&body), Duration::from_secs(600)).await {
                    Err(e) => { line(error(format!("The Jcode PGM Server could not be reached: {e}."))); if !is_running { return end(&hub, &a, &sx, &run, &pid, after, json!(1)); } }
                    Ok((st, v)) => match routed_line(st, &v) {
                        Err(why) => { line(error(cap_first(&why))); if !is_running && !waiting { return end(&hub, &a, &sx, &run, &pid, after, json!(1)); } }
                        Ok(l) => {
                            if let Some(l) = l { line(info(l)); }
                            device = s(&v, "routed") == "device";
                            waiting = true; active = false; skip = Some(text.trim().to_string()); last_at = now_ms(); sent_any = true; last_text.clear();
                        }
                    },
                }
            }
            f = fr.recv() => {
                let Some(f) = f else { break };
                let ty = s(&f, "type");
                if ty == "_connected" { ready = true; continue; }
                if ty == "ping" { continue; }
                if ty == "session" || ty == "state" {
                    if let Some(r) = running(&f) {
                        if r { active = true; is_running = true; }
                        else { is_running = false; if waiting && active { waiting = false; } }
                    }
                    continue;
                }
                if ty == "handover" { line(info(format!("The server asked jcode{} to hand its unfinished work over (stage, commit, push).", { let d = s(&f, "device"); if d.is_empty() { String::new() } else { format!(" on {d}") } }))); continue; }
                batch.push(f);
            }
            _ = tick => {
                // a turn in jcode: over once jcode has answered and gone quiet (the server may never say)
                if waiting && device && !last_text.is_empty() && now_ms().saturating_sub(last_at) > quiet { waiting = false; is_running = false; }
                // the session's state now and then (a change the stream did not carry: it came before the stream was there)
                if (waiting || is_running) && now_ms().saturating_sub(last_poll) > 1500 {
                    last_poll = now_ms();
                    if let Ok((200, v)) = call(&sv, reqwest::Method::GET, &format!("/__srv/sessions/{}?user={}&after={after}", q(&pid), q(&user)), None, Duration::from_secs(5)).await {
                        // events the stream has not brought (yet) are shown now (the stream's copies are skipped by their seq), then whether it runs
                        batch.extend(v["events"].as_array().cloned().unwrap_or_default());
                        polled = running(&state_in(&v));
                    }
                }
            }
        }
        for f in batch.drain(..) {
            let n = f["seq"].as_u64().unwrap_or(0);
            if n == 0 || n <= after { continue; }
            after = n; last_at = now_ms();
            if s(&f, "kind") == "assistant" { last_text = s(&f, "text"); active = true; }
            if matches!(s(&f, "kind").as_str(), "tool" | "note") { active = true; }
            for x in map_event(&f, &mut skip) { emit(x); }
            shown += 1; if shown % 20 == 0 { set_pgm(&w, &a, &sx, &pid, after); }
        }
        match polled.take() { Some(false) => { is_running = false; if waiting && active { waiting = false; } } Some(true) => { is_running = true; if waiting { active = true; } } None => {} }
    }
    set_pgm(&w, &a, &sx, &pid, after);
    if sent_any || !last_text.is_empty() {
        emit(json!({ "type": "result", "subtype": "success", "is_error": false, "result": last_text, "duration_ms": now_ms().saturating_sub(started), "harness": "jcode", "uuid": format!("{run}-{after}") }));
    }
    end(&hub, &a, &sx, &run, &pid, after, json!(0))
}
fn sent_any_pending(inbox: &mpsc::UnboundedReceiver<String>) -> bool { !inbox.is_empty() }
/// The run ends: its controls go first (a message that comes now starts a new run), then the run itself.
fn end(hub: &Arc<dyn AiHub>, a: &Account, sx: &Sx, run: &str, _pid: &str, _after: u64, code: Value) {
    CTL.lock().unwrap_or_else(|e| e.into_inner()).remove(run);
    hub.work().finish(&**hub, a, sx, run, code);
}

/// Starts a run following the session's PGM session (or joins the one going) and hands it `message` (None = only follow).
/// Some((run, its stream)) ; None = the session runs something else.
#[allow(clippy::too_many_arguments)]
fn begin(hub: &Arc<dyn AiHub>, a: &Account, sx: &Sx, sv: &Srv, prov: &str, pid: &str, after: u64, wd: (String, String, String), message: Option<String>, started_by: &str, banner: Vec<String>) -> Option<(String, mpsc::UnboundedReceiver<Result<Bytes, String>>)> {
    let w = hub.work();
    for _ in 0..20 {
        let (run, rx, new) = w.pgm_begin(a, sx, prov, &wd.1, &wd.2, &wd.0, pid, started_by)?;
        w.emit_key(&sx.key, json!({ "type": "bridge_run", "id": run, "session": sx.sid }));
        if new {
            let (itx, irx) = mpsc::unbounded_channel(); let stopper = Arc::new(Notify::new());
            if let Some(m) = message.clone() { let _ = itx.send(m); }
            CTL.lock().unwrap_or_else(|e| e.into_inner()).insert(run.clone(), Ctl { inbox: itx, stop: stopper.clone() });
            for b in &banner { w.emit_key(&sx.key, info(b.clone())); }
            Work::watch_liveness(hub, a, sx, &run);
            tokio::spawn(follow(hub.clone(), a.clone(), sx.clone(), run.clone(), sv.clone(), pid.to_string(), after, irx, stopper, wd.clone()));
            return Some((run, rx));
        }
        // the run going takes the message (it may be just ending: then a new one starts)
        let ok = match &message { None => true, Some(m) => CTL.lock().unwrap_or_else(|e| e.into_inner()).get(&run).map(|c| c.inbox.send(m.clone()).is_ok()).unwrap_or(false) };
        if ok { return Some((run, rx)); }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// POST /api/chat on a PGM session (called by work::chat once the session, its worktree and the provider are settled).
#[allow(clippy::too_many_arguments)]
pub async fn chat(hub: &Arc<dyn AiHub>, a: &Account, sx: &Sx, prov: &Value, state_model: &str, wd: &Wt, text: &str, fb_line: Option<&str>, started_by: &str, srv: (String, String)) -> Resp {
    let w = hub.work(); let sv = Srv { url: srv.0, secret: srv.1 };
    let (pair, label) = match crate::openrouter::pgm_work_pair(&**hub, &a.username, prov, state_model) { Ok(x) => x, Err(e) => return one_line(error(e)) };
    let (mut pid, after) = pgm_of(w, a, sx);
    let mut banner = vec![format!("Using the Jcode PGM Server ({label}): the session lives on the server, so jcode on your computer can continue it too.")];
    if let Some(l) = fb_line { banner.insert(0, l.to_string()); }
    let dir = wd.dir.display().to_string();
    let project = repo_of(w, wd);
    // the first message makes the PGM session (runner: the server), with the session's own worktree
    if pid.is_empty() {
        let t = s(&sessions::index(a)["sessions"][&sx.sid], "title");
        let title = if t.is_empty() { text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect() } else { t };
        let body = json!({ "user": a.username, "title": title, "project": project, "branch": wd.branch, "pair": pair, "worktree": dir });
        match call(&sv, reqwest::Method::POST, "/__srv/sessions", Some(&body), Duration::from_secs(30)).await {
            Ok((st, v)) if (200..300).contains(&st) => {
                let id = v["id"].as_str().or(v["session"]["id"].as_str()).unwrap_or("").to_string();
                if !valid_ps(&id) { return one_line(error("The Jcode PGM Server made the session but gave no id it could be found by again.")); }
                pid = id; set_pgm(w, a, sx, &pid, 0);
            }
            Ok((st, v)) => return one_line(error(format!("The Jcode PGM Server could not start the session: {}", err_text(&v, st)))),
            Err(e) => return one_line(error(format!("The Jcode PGM Server could not be reached: {e}. Pick a Claude in the dropdown meanwhile, or ask an admin to start it."))),
        }
    }
    match begin(hub, a, sx, &sv, &s(prov, "id"), &pid, after, (dir, project, wd.branch.clone()), Some(text.to_string()), started_by, banner) {
        Some((_, rx)) => Work::stream_of(rx),
        None => answer(409, json!({ "error": "Claude is already working. Stop it first." })),
    }
}

/// A PGM session opened (the list, a reload): what happened on the server or in jcode since is shown, and followed live while it
/// runs. Nothing for a session that is not a PGM session, or that runs now.
pub async fn watch(hub: &Arc<dyn AiHub>, a: &Account, sid: &str) {
    let Some(sv) = srv_of(&**hub) else { return };
    let Some(sx) = sessions::resolve(a, sid) else { return };
    let w = hub.work();
    if w.busy_key(&sx.key) { return; }
    let (pid, after) = pgm_of(w, a, &sx); if pid.is_empty() { return; }
    let st = sessions::sstate(w, a, &sx);
    let wt = &sessions::index(a)["sessions"][&sx.sid]["worktree"];
    let _ = begin(hub, a, &sx, &sv, &s(&st, "provider"), &pid, after, (s(wt, "path"), s(&st, "project"), s(wt, "branch")), None, &a.username, vec![]);
}

/// The person's PGM sessions as the server lists them (None = it did not answer).
async fn server_list(sv: &Srv, user: &str) -> Option<Vec<Value>> {
    match call(sv, reqwest::Method::GET, &format!("/__srv/sessions?user={}", q(user)), None, Duration::from_millis(1500)).await {
        Ok((200, v)) => Some(v["sessions"].as_array().or(v.as_array()).cloned().unwrap_or_default()),
        _ => None,
    }
}
/// GET /api/work/sessions with a server: each Work session that is a PGM session says where it runs (`pgm: {id, where,
/// runner}`); the server's sessions that have no Work session yet are in `external` (source pgm-server, `where`), next to and
/// merged with its report; one that has a Work session is listed as that session only.
pub async fn merge_list(hub: &Arc<dyn AiHub>, a: &Account, v: &mut Value) {
    let Some(sv) = srv_of(&**hub) else { return };
    let listed = server_list(&sv, &a.username).await;
    let by_id: HashMap<String, Value> = listed.clone().unwrap_or_default().into_iter().filter_map(|x| { let id = s(&x, "id"); valid_ps(&id).then_some((id, x)) }).collect();
    let mut attached: Vec<String> = vec![];
    for x in v["sessions"].as_array_mut().into_iter().flatten() {
        let id = s(&x["pgm"], "id"); if id.is_empty() { continue; }
        if let Some(st) = by_id.get(&id) { x["pgm"]["where"] = json!(where_text(st)); x["pgm"]["runner"] = st["runner"].clone(); }
        attached.push(id);
    }
    let mut ext: Vec<Value> = v["external"].as_array().cloned().unwrap_or_default().into_iter().filter(|x| !(s(x, "source") == SOURCE && attached.contains(&s(x, "id")))).collect();
    for x in ext.iter_mut().filter(|x| s(x, "source") == SOURCE) { if let Some(st) = by_id.get(&s(x, "id")) { x["where"] = json!(where_text(st)); x["pgm"] = json!(true); } }
    for (id, st) in &by_id {
        if attached.contains(id) || ext.iter().any(|x| s(x, "source") == SOURCE && s(x, "id") == *id) { continue; }
        let state = { let t = s(st, "state"); if ["waiting", "working", "done-unseen", "draft", "idle", "ready"].contains(&t.as_str()) { t } else if running(st) == Some(true) { "working".into() } else { "ready".into() } };
        ext.push(json!({ "source": SOURCE, "id": id, "title": s(st, "title"), "state": state, "project": s(st, "project"), "branch": s(st, "branch"), "host": "", "device": s(&st["runner"], "device"), "updatedAt": st["updatedAt"], "where": where_text(st), "pgm": true }));
    }
    for x in ext.iter_mut().filter(|x| s(x, "source") == SOURCE) { x["pgm"] = json!(true); }
    v["external"] = json!(ext);
}

/// POST /api/work/sessions/pgm/open {id: "ps-…"}: the Work session of that PGM session (made now, with the same title, project
/// and branch, when there is none: one started in jcode), open on screen and followed live. {id: <its session id>, pgm, created}.
pub async fn open(hub: &Arc<dyn AiHub>, a: &Account, b: &Value) -> Resp {
    let Some(sv) = srv_of(&**hub) else { return answer(404, json!({ "error": NO_SERVER })) };
    let pid = s(b, "id");
    if !valid_ps(&pid) { return answer(400, json!({ "error": "A PGM session id looks like ps-…" })); }
    let w = hub.work();
    let (sid, created) = match session_with(w, a, &pid) {
        Some(sx) => (sx.sid, false),
        None => {
            let v = match call(&sv, reqwest::Method::GET, &format!("/__srv/sessions/{}?user={}&after={}", q(&pid), q(&a.username), u32::MAX), None, Duration::from_secs(10)).await {
                Ok((200, v)) => v,
                Ok((404, _)) | Ok((403, _)) => return answer(404, json!({ "error": "The Jcode PGM Server has no such session of yours." })),
                Ok((st, v)) => return answer(502, json!({ "error": format!("The Jcode PGM Server refused: {}", err_text(&v, st)) })),
                Err(e) => return answer(502, json!({ "error": format!("The Jcode PGM Server could not be reached: {e}.") })),
            };
            let st = state_in(&v);
            let mut m = Map::new();
            // the provider and model of its pair: their own key's (my/<id>) or the team's
            let pair = s(&st, "pair");
            match crate::openrouter::pgm_resolve(&**hub, &a.username, &pair) {
                Some((p, pr)) => { m.insert("provider".into(), p["id"].clone()); m.insert("model".into(), pr["id"].clone()); }
                None => { if let Some(p) = w.team_provider() { m.insert("provider".into(), p["id"].clone()); } m.insert("model".into(), json!(pair.strip_prefix("my/").unwrap_or(&pair))); }
            }
            // the project: the hub's project of that Forgejo repo (owner/name), else none
            let repo = s(&st, "project"); let name = repo.rsplit('/').next().unwrap_or("").to_string();
            match (!name.is_empty()).then(|| w.wt.project(&name).ok()).flatten() { Some(p) if p.hub => {} Some(p) => { m.insert("project".into(), json!(p.name)); } None => { m.insert("noProject".into(), json!(true)); } }
            m.insert("pgm".into(), json!({ "id": pid, "seq": 0 }));
            let title = { let t = s(&st, "title"); if t.is_empty() { pid.clone() } else { t } };
            match sessions::create(a, &title, Value::Object(m), json!({ "branch": s(&st, "branch"), "project": if name.is_empty() { json!(HUB_PROJECT) } else { json!(name) }, "pgm": true }), true) { Ok(sid) => (sid, true), Err(e) => return answer(400, json!({ "error": e })) }
        }
    };
    let _ = sessions::touch(a, &sid, |m| { m.insert("seenAt".into(), json!(now_ms())); m.insert("hidden".into(), json!(false)); m.insert("archived".into(), json!(false)); });
    sessions::set_current(a, &sid);
    watch(hub, a, &sid).await;
    answer(200, json!({ "id": sid, "pgm": pid, "created": created }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_server_s_events_become_the_page_s() {
        let mut skip = Some("hi there".to_string());
        assert!(map_event(&json!({ "seq": 1, "from": "website", "kind": "user", "text": "hi there" }), &mut skip).is_empty(), "the website's own message is not shown twice");
        assert_eq!(map_event(&json!({ "seq": 2, "from": "website", "kind": "user", "text": "hi there" }), &mut skip)[0]["text"], "You (website): hi there");
        let a = map_event(&json!({ "seq": 3, "from": "server", "kind": "assistant", "text": "Done it." }), &mut None);
        assert_eq!(a[0]["type"], "assistant"); assert_eq!(a[0]["message"]["content"][0]["text"], "Done it.");
        let t = map_event(&json!({ "seq": 4, "from": "device:laptop", "kind": "tool", "text": "ran ls", "tool": { "name": "bash", "input": { "command": "ls" }, "output": "a b" } }), &mut None);
        assert_eq!(t[0]["message"]["content"][0]["name"], "bash"); assert_eq!(t[1]["message"]["content"][0]["tool_use_id"], "pgm-4"); assert_eq!(t[1]["message"]["content"][0]["content"], "a b");
        assert_eq!(map_event(&json!({ "seq": 5, "from": "device:laptop", "kind": "user", "text": "go on" }), &mut None)[0]["text"], "You in jcode on laptop: go on");
        assert_eq!(map_event(&json!({ "seq": 6, "from": "server", "kind": "note", "text": "stopped" }), &mut None)[0]["type"], "bridge_info");
        assert!(map_event(&json!({ "seq": 7, "kind": "assistant", "text": "  " }), &mut None).is_empty());
    }
    #[test]
    fn the_answers_to_a_message_are_plain_lines() {
        assert!(routed_line(200, &json!({ "routed": "device", "device": "laptop" })).unwrap().unwrap().starts_with("Sent to jcode on laptop"));
        assert_eq!(routed_line(200, &json!({ "routed": "server", "run": "r1" })).unwrap(), None);
        assert!(routed_line(200, &json!({ "routed": "server", "from": "last-push", "note": "the newest work is on laptop; continued from abc" })).unwrap().unwrap().starts_with("The newest work is on laptop"));
        assert!(routed_line(200, &json!({ "routed": "server", "handover": { "device": "laptop", "branch": "work/x", "commit": "0123456789abcdef" } })).unwrap().unwrap().contains("work/x at 0123456789ab"));
        assert_eq!(routed_line(409, &json!({ "error": "the work is on laptop, which is offline, and was never pushed" })), Err("the work is on laptop, which is offline, and was never pushed".to_string()));
        assert_eq!(where_text(&json!({ "runner": { "kind": "device", "device": "laptop" } })), "running in jcode on laptop");
        assert_eq!(where_text(&json!({ "runner": { "kind": "server" } })), "running on the server");
        assert!(valid_ps("ps-0123456789ab") && !valid_ps("s-0123") && !valid_ps("ps-") && !valid_ps("ps-a/b"));
    }
}
