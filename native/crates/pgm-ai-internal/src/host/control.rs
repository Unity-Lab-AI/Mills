//! Controls from the hub (the cap `control`, 5.12.0): the person who owns this host pauses, resumes, restarts, updates
//! it or looks at its log from Settings → My AI hosts. Each action is exactly what the local control app (`vr-host`)
//! does: pause / resume = the off file, restart / update = the request files that wait until nothing is in flight. The
//! host also tells the hub how it is doing (`POST /node/status`, small, never a message): the hub only shows it.
use serde_json::{json, Map, Value};
use std::sync::atomic::Ordering;

use crate::host::folder::{log, Folder};
use crate::host::relay;
use crate::host::runner::{ct_json, H, TARGET, VERSION};
use pgm_core_internal::now_ms;
use pgm_net_internal::hostlink::Job;

/// What the hub may ask.
pub const ACTIONS: [&str; 5] = ["pause", "resume", "restart", "update", "logs"];
/// The first words of the off file when the hub paused the host. A host paused on its own machine stops polling (the
/// hub sees it offline and cannot reach it); one paused from the hub keeps listening so the hub can resume it.
pub const MARK: &str = "AI paused from the hub";
/// How many log lines "logs" returns.
pub const LOG_LINES: usize = 50;

/// The action in a control job's body (`{ "action": "pause" }`), or why it is refused.
pub fn parse(body: &[u8]) -> Result<String, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "A control job carries { action }.".to_string())?;
    let a = v["action"].as_str().unwrap_or("");
    if ACTIONS.contains(&a) { Ok(a.to_string()) } else { Err(format!("Unknown control {a:?} (this host knows {}).", ACTIONS.join(", "))) }
}

/// Is this host paused from the hub (listening for a resume) rather than on its own machine?
pub fn held(f: &Folder) -> bool { std::fs::read_to_string(f.off()).map(|t| t.starts_with(MARK)).unwrap_or(false) }

fn secret_like(w: &str) -> bool {
    w.len() >= 20 && (w.bytes().all(|b| b.is_ascii_hexdigit()) || ["sk-", "ghp_", "gho_", "xoxb-", "eyJ", "AIza"].iter().any(|p| w.starts_with(p)))
}
/// A log line without anything that opens a door: the host's token and the api keys of its providers wherever they
/// appear, whatever follows "Bearer", and words that look like keys (long hex, sk-…, JWTs).
pub fn redact(line: &str, secrets: &[String]) -> String {
    let mut s = line.to_string();
    for k in secrets { if k.len() >= 8 { s = s.replace(k.as_str(), "***"); } }
    let mut out = String::with_capacity(s.len());
    let (mut word, mut after_bearer) = (String::new(), false);
    let flush = |word: &mut String, out: &mut String, after_bearer: &mut bool| {
        if !word.is_empty() {
            if secret_like(word) || (*after_bearer && word.len() >= 8) { out.push_str("***"); } else { out.push_str(word); }
            *after_bearer = word.eq_ignore_ascii_case("bearer");
            word.clear();
        }
    };
    for c in s.chars() {
        if c.is_whitespace() || "\"',;=()[]{}<>".contains(c) { flush(&mut word, &mut out, &mut after_bearer); out.push(c); } else { word.push(c); }
    }
    flush(&mut word, &mut out, &mut after_bearer);
    out
}

/// The last `n` non-empty lines of a text.
pub fn tail_lines(text: &str, n: usize) -> Vec<String> {
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(n)..].iter().map(|l| l.chars().take(400).collect()).collect()
}

/// The end of the log file (the last 128 KB are read, never the whole file).
fn read_tail(f: &Folder) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(f.log()).ok()?;
    let len = file.metadata().ok()?.len();
    let from = len.saturating_sub(128 * 1024);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).to_string();
    // a cut at the start is not a whole line
    Some(if from > 0 { text.split_once('\n').map(|x| x.1.to_string()).unwrap_or_default() } else { text })
}

/// Everything in this host's config that must never reach the hub's page: its token and its providers' keys.
pub fn secrets_of(h: &crate::host::runner::Host) -> Vec<String> {
    let mut v = vec![h.cfg.token()];
    for p in h.providers() { if !p.api_key.is_empty() { v.push(p.api_key.clone()); } }
    v
}

/// What the hub gets about this host every ~15 s: counts, times, windows and login state; never a message or a path.
pub fn report(h: &crate::host::runner::Host) -> Value {
    let st = h.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let off = h.folder.off().exists();
    let mut logins = Map::new();
    for p in h.providers() { if p.is_relay() { logins.insert(p.id.clone(), relay::login_problem(&p).map(|m| json!(m.chars().take(200).collect::<String>())).unwrap_or(Value::Null)); } }
    let usage: Map<String, Value> = h.usage.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(k, v)| (k.clone(), json!({ "five_hour": v["five_hour"], "seven_day": v["seven_day"] }))).collect();
    json!({ "version": VERSION, "target": TARGET, "native": true, "startedAt": st.get("startedAt").cloned().unwrap_or(json!(0)),
        "paused": off, "pausedBy": if !off { "" } else if held(&h.folder) { "hub" } else { "machine" },
        "active": h.active.load(Ordering::SeqCst), "runs": h.runs.lock().unwrap_or_else(|e| e.into_inner()).len(), "lastJobAt": h.last_job_at.load(Ordering::SeqCst),
        "lastUpdate": st.get("lastUpdate").cloned().unwrap_or(json!("")), "lastUpdateAt": st.get("lastUpdateAt").cloned().unwrap_or(json!(0)), "pending": st.get("pending").cloned().unwrap_or(json!("")),
        "usage": usage, "logins": logins, "at": now_ms() })
}

/// Applies one control and answers `{ ok, result, lines? }`.
pub fn apply(h: &crate::host::runner::Host, action: &str) -> Value {
    let f = &h.folder;
    match action {
        "pause" => {
            let _ = std::fs::write(f.off(), format!("{MARK} at {}\n", now_ms()));
            log("paused from the hub (its owner asked in Settings → My AI hosts)");
            h.set_status(json!({ "paused": true, "pausedBy": "hub" }));
            json!({ "ok": true, "result": "Paused: this host lends nothing new until it is resumed (jobs already running finish)." })
        }
        "resume" => {
            let _ = std::fs::remove_file(f.off());
            log("resumed from the hub");
            h.set_status(json!({ "paused": false, "pausedBy": "" }));
            json!({ "ok": true, "result": "Resumed: this host lends its AI again." })
        }
        "restart" => {
            let _ = std::fs::write(f.restart_now(), "quiet");
            log("restart asked from the hub: it happens once nothing is in flight");
            json!({ "ok": true, "result": "Restart asked: it happens once nothing is in flight." })
        }
        "update" => {
            let _ = std::fs::write(f.update_now(), now_ms().to_string());
            log("update asked from the hub: it looks for one once nothing is in flight");
            json!({ "ok": true, "result": "Update asked: the host looks for one now (once nothing is in flight) and restarts on it." })
        }
        _ => match read_tail(f) {
            Some(t) => { let secrets = secrets_of(h); let lines: Vec<String> = tail_lines(&t, LOG_LINES).iter().map(|l| redact(l, &secrets)).collect(); json!({ "ok": true, "result": format!("The last {} log lines.", lines.len()), "lines": lines }) }
            None => json!({ "ok": true, "result": "This host writes no log file here (it was started in a terminal, or as a service that logs elsewhere).", "lines": [] }),
        },
    }
}

/// A control job from the hub, answered with one short line.
pub async fn control_job(h: H, job: Job) {
    let out = match job.body_bytes().map_err(|e| e.to_string()).and_then(|b| parse(&b)) {
        Ok(a) => apply(&h, &a),
        Err(e) => json!({ "ok": false, "result": e }),
    };
    let status = if out["ok"] == json!(true) { 200 } else { 400 };
    h.link.read().await.reply(&job.id, status, &ct_json(), out.to_string().as_bytes()).await;
    h.nudge.notify_one(); // the hub hears the new state now, not in 15 s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_known_actions_are_taken() {
        for a in ACTIONS { assert_eq!(parse(json!({ "action": a }).to_string().as_bytes()).unwrap(), a); }
        assert!(parse(br#"{"action":"rm -rf"}"#).is_err() && parse(b"{}").is_err() && parse(b"not json").is_err());
    }
    #[test]
    fn the_log_loses_every_key() {
        let tok = "0123456789abcdef".repeat(3); let tok = tok.as_str(); // built, so the tracked-file scan (R31) sees no hash
        let l = redact(&format!("12:00:00 [ai-host] token={tok} Authorization: Bearer abcdefgh1234 key sk-ant-api03-abcdefghijklmnop and {} ok", "a".repeat(40)), &[tok.to_string(), "my-api-key-1".into()]);
        assert!(!l.contains(tok) && !l.contains("abcdefgh1234") && !l.contains("sk-ant") && !l.contains(&"a".repeat(40)), "{l}");
        assert!(l.contains("12:00:00 [ai-host]") && l.ends_with(" ok"));
        assert_eq!(redact("provider key my-api-key-1 here", &["my-api-key-1".into()]), "provider key *** here");
        assert_eq!(redact("connected to https://hub as \"silly-panda\"", &[]), "connected to https://hub as \"silly-panda\"");
    }
    #[test]
    fn the_tail_is_the_last_lines_without_blanks() {
        let t: String = (1..=80).map(|i| format!("line {i}\n\n")).collect();
        let v = tail_lines(&t, 50);
        assert_eq!(v.len(), 50); assert_eq!(v[0], "line 31"); assert_eq!(v[49], "line 80");
        assert_eq!(tail_lines("a\nb", 5), ["a", "b"]);
    }
}
