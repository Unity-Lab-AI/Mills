//! Triggers (T-186): what starts a workflow without anyone typing. A trigger is an object of its own that points at a
//! workflow (open question 19: the document does not change), stored next to the workflows of its scope
//! (`<root>/<scope>/triggers/<id>.json`), and its runs belong to the person who set it (the sender says `trigger`).
//! - **timer** (T-211): `every {n, unit: minutes | hours | days}`, or `recur` = `nightly` / `daily` / `weekly` /
//!   `biweekly` / `monthly` / `quarterly` / `half_year` / `yearly` (the board's own repeats, through the directory),
//!   `at` "HH:MM". The next time is saved before a run starts, so a restart never runs it twice; one hub at a time
//!   fires a store's timers (a lease in `scheduler.json`). A time missed while the hub was down follows `missed`:
//!   `once` (one run, then on schedule) or `skip`.
//! - **webhook** (T-212): `POST /hooks/hyperwork/<id>` from Forgejo / Gitea (`X-Forgejo-Signature`,
//!   `X-Gitea-Signature`) or GitHub (`X-Hub-Signature-256`), signed with the trigger's secret; `events` narrows which
//!   (`pull_request`, `issues`, `push` ...); the payload is the run's `inputs.event`.
//! - **event** (T-212): a name other processes on the server send (`Service::event`).
//!
//! Every trigger keeps its last runs and its next time, can be paused and resumed, and a run of it that fails is sent
//! to its owner as a chat alert (standing workflows, T-213).
use super::*;

const MINUTE: u64 = 60_000;
const RECUR: [&str; 8] = ["nightly", "daily", "weekly", "biweekly", "monthly", "quarterly", "half_year", "yearly"];
/// A timer later than this is a missed one (the hub was down), not a late tick.
const GRACE: u64 = 2 * MINUTE;
const HISTORY: usize = 30;
const LEASE: u64 = 30_000;

impl Service {
    fn trigger_dir(&self, scope: &str) -> PathBuf { self.root.join(scope).join("triggers") }

    /// Every trigger of every scope, each with its scope.
    fn triggers_all(&self) -> Vec<Value> {
        let mut out = vec![];
        for sc in std::fs::read_dir(&self.root).into_iter().flatten().flatten() {
            let scope = sc.file_name().to_string_lossy().to_string();
            for f in std::fs::read_dir(sc.path().join("triggers")).into_iter().flatten().flatten() {
                if f.file_name().to_string_lossy().ends_with(".json") { if let Some(mut t) = read(&f.path()) { t["scope"] = json!(scope); out.push(t); } }
            }
        }
        out.sort_by(|a, b| s(a, "id").cmp(s(b, "id")));
        out
    }

    fn trigger_by_id(&self, id: &str) -> Option<Value> { if safe(id) { self.triggers_all().into_iter().find(|t| s(t, "id") == id) } else { None } }

    fn save_trigger(&self, t: &Value) -> bool {
        let mut v = t.clone();
        let scope = s(t, "scope").to_string();
        if let Some(o) = v.as_object_mut() { o.remove("scope"); }
        write(&self.trigger_dir(&scope).join(format!("{}.json", s(t, "id"))), &v)
    }

    /// One period on from `ms`.
    fn advance(&self, timer: &Value, ms: u64) -> Option<u64> {
        if let Some(e) = timer.get("every").filter(|e| e.is_object()) {
            let n = e["n"].as_u64().unwrap_or(1).max(1);
            let unit = match e["unit"].as_str().unwrap_or("minutes") { "hours" => 60 * MINUTE, "days" => 1440 * MINUTE, _ => MINUTE };
            return Some(ms + n * unit);
        }
        self.dir.next_due(ms, timer["recur"].as_str().unwrap_or(""))
    }

    /// The first time a new (or resumed) timer fires after `now`.
    fn first_fire(&self, timer: &Value, now: u64) -> Option<u64> {
        if let Some(st) = timer["start"].as_u64().filter(|st| *st > now) { return Some(st); }
        let at = timer["at"].as_str().or(if timer["recur"] == "nightly" { Some("02:00") } else { None });
        match at {
            Some(a) if timer.get("every").is_none() => self.dir.first_time(now, a),
            _ => self.advance(timer, now),
        }
    }

    /// One hub at a time fires a store's timers: this one, if it holds the lease or the holder let it lapse.
    fn take_lease(&self, now: u64) -> bool {
        let f = self.root.join("scheduler.json");
        let cur = read(&f).unwrap_or(json!({}));
        if s(&cur, "holder") != self.instance && cur["until"].as_u64().is_some_and(|u| u > now) { return false; }
        write(&f, &json!({ "holder": self.instance, "until": now + LEASE }))
    }

    fn remember(&self, t: &mut Value, entry: Value) {
        if !t["runs"].is_array() { t["runs"] = json!([]); }
        let runs = t["runs"].as_array_mut().unwrap();
        runs.insert(0, entry);
        runs.truncate(HISTORY);
    }

    /// Starts one run of a trigger, as its owner. The trigger is saved (with its history) whatever happens.
    fn fire(&self, t: &mut Value, prompt: String, extra: serde_json::Map<String, Value>) -> Result<String, String> {
        let c = Caller { user: s(t, "owner").to_string(), run_only: false };
        let mut inputs = t["inputs"].as_object().cloned().unwrap_or_default();
        inputs.extend(extra);
        let mut body = json!({ "id": t["workflow"], "prompt": prompt, "inputs": inputs, "sender": { "kind": "trigger", "id": t["id"], "owner": t["owner"] }, "trigger": t["id"] });
        if let Some(p) = t["project"].as_str() { body["project"] = json!(p); }
        let res = match self.start_run(&c, &body) {
            Answer::Json(202, v) => Ok(s(&v, "run").to_string()),
            Answer::Json(_, v) => Err(v["error"].as_str().unwrap_or("refused").to_string() + &v["problems"].as_array().map(|p| format!(": {}", p.iter().filter_map(|x| x["message"].as_str()).collect::<Vec<_>>().join("; "))).unwrap_or_default()),
            Answer::Lines(_) => Err("unexpected".into()),
        };
        match &res {
            Ok(run) => self.remember(t, json!({ "run": run, "at": now_ms(), "status": "running" })),
            Err(e) => {
                self.remember(t, json!({ "at": now_ms(), "status": "refused", "error": e }));
                if t["alert"] != json!(false) { self.dir.alert(s(t, "owner"), &format!("The trigger \"{}\" could not start its workflow: {e}", title_of(t))); }
            }
        }
        self.save_trigger(t);
        res
    }

    /// The timers that are due at `now` (the hub calls it through `tick`; tests with a clock of their own).
    pub fn tick_at(&self, now: u64) {
        // nothing is written (not even the lease) unless a timer is due
        let due: Vec<Value> = self.triggers_all().into_iter().filter(|t| s(t, "kind") == "timer" && t["paused"] != json!(true) && t["next"].as_u64().is_some_and(|n| n <= now)).collect();
        if due.is_empty() || !self.take_lease(now) { return; }
        for mut t in due {
            let Some(next) = t["next"].as_u64() else { continue };
            // the next time first, saved before the run: a restart in between never runs it twice
            let mut n = next;
            while n <= now { match self.advance(&t["timer"], n) { Some(x) if x > n => n = x, _ => { n = u64::MAX; break; } } }
            t["next"] = if n == u64::MAX { Value::Null } else { json!(n) };
            t["last"] = json!(now);
            let missed = now.saturating_sub(next) > GRACE;
            if missed && t["missed"] == "skip" {
                self.remember(&mut t, json!({ "at": now, "status": "skipped", "due": next, "reason": "missed while the hub was down" }));
                self.save_trigger(&t);
                continue;
            }
            self.save_trigger(&t);
            let prompt = t["prompt"].as_str().unwrap_or("").to_string();
            let mut extra = serde_json::Map::new();
            extra.insert("due".into(), json!(next));
            if missed { extra.insert("missed".into(), json!(true)); }
            let _ = self.fire(&mut t, prompt, extra);
        }
    }

    /// A run of a trigger ended: its history says how; a failure goes to the owner as a chat alert (T-213).
    pub(super) fn trigger_finished(&self, run: &str, owner: &str, rep: &RunReport) {
        let Some(id) = self.run_meta(run).and_then(|m| m["trigger"].as_str().map(str::to_string)) else { return };
        let Some(mut t) = self.trigger_by_id(&id) else { return };
        if let Some(e) = t["runs"].as_array_mut().and_then(|a| a.iter_mut().find(|e| e["run"] == json!(run))) {
            e["status"] = json!(rep.status);
            e["outcome"] = json!(rep.outcome);
            e["ended"] = json!(now_ms());
        }
        self.save_trigger(&t);
        if matches!(rep.status.as_str(), "failed" | "gave-up" | "limit" | "invalid") && t["alert"] != json!(false) {
            let why = rep.error.as_ref().and_then(|e| e["message"].as_str()).unwrap_or("");
            self.dir.alert(owner, &format!("Hyper Workflows: the trigger \"{}\" ran its workflow and it ended {} ({run}){}", title_of(&t), rep.status, if why.is_empty() { String::new() } else { format!(": {why}") }));
        }
    }

    // ------------------------------------------------------------ routes

    fn trigger_visible(&self, c: &Caller, t: &Value) -> bool {
        s(t, "owner") == c.user || self.meta_by_id(s(t, "workflow")).is_some_and(|m| self.rights(c, &m).view)
    }
    fn trigger_mine(&self, c: &Caller, t: &Value) -> bool {
        s(t, "owner") == c.user || self.meta_by_id(s(t, "workflow")).is_some_and(|m| self.rights(c, &m).admin)
    }

    /// `GET triggers[?workflow=]`, `POST trigger`, `POST trigger/pause | resume | delete`.
    pub(super) fn triggers_route(&self, c: &Caller, method: &str, path: &str, query: &HashMap<String, String>, body: &Value) -> Answer {
        let b = |k: &str| body.get(k).and_then(Value::as_str).filter(|x| !x.is_empty());
        match (method, path) {
            ("GET", "triggers") => {
                let wf = query.get("workflow").map(String::as_str).filter(|x| !x.is_empty());
                let list: Vec<Value> = self.triggers_all().into_iter().filter(|t| wf.is_none_or(|w| s(t, "workflow") == w) && self.trigger_visible(c, t)).map(|mut t| {
                    if let Some(w) = t.get_mut("webhook") { let has = w["secret"].is_string(); if let Some(o) = w.as_object_mut() { o.remove("secret"); } w["hasSecret"] = json!(has); }
                    t
                }).collect();
                j(200, json!({ "triggers": list }))
            }
            ("POST", "trigger") => self.trigger_save(c, body),
            ("POST", "trigger/pause") | ("POST", "trigger/resume") | ("POST", "trigger/delete") => {
                let Some(mut t) = b("id").and_then(|id| self.trigger_by_id(id)) else { return bad(404, "No such trigger.") };
                if !self.trigger_visible(c, &t) { return bad(404, "No such trigger."); }
                if !self.trigger_mine(c, &t) { return bad(403, "Only its owner (or the workflow's admin) changes a trigger."); }
                match path {
                    "trigger/pause" => { t["paused"] = json!(true); }
                    "trigger/resume" => {
                        t["paused"] = json!(false);
                        if s(&t, "kind") == "timer" { t["next"] = json!(self.first_fire(&t["timer"], now_ms())); }
                    }
                    _ => {
                        let _ = std::fs::rename(self.trigger_dir(s(&t, "scope")).join(format!("{}.json", s(&t, "id"))), self.trigger_dir(s(&t, "scope")).join(format!("{}.deleted-{}", s(&t, "id"), stamp())));
                        self.audit(s(&t, "scope"), &c.user, "trigger-delete", json!({ "id": t["id"] }));
                        return j(200, json!({ "ok": true }));
                    }
                }
                self.save_trigger(&t);
                j(200, json!({ "ok": true, "paused": t["paused"], "next": t["next"] }))
            }
            _ => bad(404, "Unknown endpoint"),
        }
    }

    fn trigger_save(&self, c: &Caller, body: &Value) -> Answer {
        let existing = body["id"].as_str().filter(|x| !x.is_empty()).and_then(|id| self.trigger_by_id(id));
        if body["id"].as_str().is_some_and(|x| !x.is_empty()) && existing.is_none() { return bad(404, "No such trigger."); }
        if let Some(e) = &existing { if !self.trigger_mine(c, e) { return bad(403, "Only its owner (or the workflow's admin) changes a trigger."); } }
        let wf_id = body["workflow"].as_str().or(existing.as_ref().and_then(|e| e["workflow"].as_str())).unwrap_or("").to_string();
        let Some(m) = self.meta_by_id(&wf_id) else { return bad(404, "No such workflow.") };
        let r = self.rights(c, &m);
        if !(r.view || r.run) { return bad(404, "No such workflow."); }
        if !r.run { return bad(403, "You may not run this workflow, so you may not trigger it."); }
        let kind = body["kind"].as_str().or(existing.as_ref().and_then(|e| e["kind"].as_str())).unwrap_or("").to_string();
        let mut t = existing.clone().unwrap_or_else(|| json!({ "id": format!("tr-{}", pgm_core_internal::crypto::random_hex(6)), "owner": c.user, "scope": m["scope"], "createdAt": now_ms(), "runs": [] }));
        t["workflow"] = json!(wf_id);
        t["kind"] = json!(kind);
        for k in ["title", "prompt", "inputs", "project", "alert", "missed"] { if body.get(k).is_some() { t[k] = body[k].clone(); } }
        if !t["prompt"].is_string() { t["prompt"] = json!(""); }
        if !t["missed"].is_string() { t["missed"] = json!("once"); }
        if !matches!(s(&t, "missed"), "once" | "skip") { return bad(400, "`missed` is once or skip."); }
        let mut secret = None;
        match kind.as_str() {
            "timer" => {
                let timer = if body["timer"].is_object() { body["timer"].clone() } else { t["timer"].clone() };
                let every_ok = timer.get("every").is_some_and(|e| e["n"].as_u64().is_some_and(|n| n >= 1) && matches!(e["unit"].as_str(), Some("minutes" | "hours" | "days")));
                let recur_ok = timer["recur"].as_str().is_some_and(|x| RECUR.contains(&x));
                if every_ok == recur_ok { return bad(400, format!("A timer has `every: {{n, unit: minutes | hours | days}}` or `recur: {}` (one of the two).", RECUR.join(" | "))); }
                if let Some(a) = timer["at"].as_str() { if self.dir.first_time(0, a).is_none() { return bad(400, "`at` is a time of day, HH:MM."); } }
                t["timer"] = timer.clone();
                if body["timer"].is_object() || existing.is_none() { t["next"] = json!(self.first_fire(&timer, now_ms())); }
            }
            "webhook" => {
                let mut w = if body["webhook"].is_object() { body["webhook"].clone() } else { t["webhook"].clone() };
                if !w.is_object() { w = json!({}); }
                if !w["secret"].is_string() { let sec = pgm_core_internal::crypto::random_hex(24); w["secret"] = json!(sec); secret = Some(sec); }
                if !w["events"].is_array() { w["events"] = json!([]); }
                t["webhook"] = w;
            }
            "event" => {
                let e = if body["event"].is_object() { body["event"].clone() } else { t["event"].clone() };
                if !e["name"].as_str().is_some_and(|n| !n.is_empty() && n.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_.".contains(ch))) { return bad(400, "An event trigger needs `event: {name}` (letters, digits, - _ .)."); }
                t["event"] = e;
            }
            _ => return bad(400, "`kind` is timer, webhook or event."),
        }
        if !self.save_trigger(&t) { return bad(500, "Could not save the trigger."); }
        self.audit(s(&t, "scope"), &c.user, "trigger-save", json!({ "id": t["id"], "kind": kind, "workflow": wf_id }));
        let mut out = json!({ "ok": true, "id": t["id"], "next": t["next"] });
        if let Some(sec) = secret { out["secret"] = json!(sec); out["path"] = json!(format!("/hooks/hyperwork/{}", s(&t, "id"))); }
        j(200, out)
    }

    /// `POST /hooks/hyperwork/<id>` (no session: the signature is the key). `header` reads a request header (any
    /// case). Answers 202 with the run, or why nothing ran.
    pub fn webhook(&self, id: &str, header: &dyn Fn(&str) -> Option<String>, body: &[u8]) -> (u16, Value) {
        let Some(mut t) = self.trigger_by_id(id).filter(|t| s(t, "kind") == "webhook") else { return (404, json!({ "error": "No such hook." })) };
        let secret = t["webhook"]["secret"].as_str().unwrap_or("").to_string();
        let sig = header("x-hub-signature-256").map(|x| x.trim_start_matches("sha256=").to_string()).or_else(|| header("x-forgejo-signature")).or_else(|| header("x-gitea-signature"));
        let Some(sig) = sig else { return (401, json!({ "error": "The request is not signed." })) };
        if secret.is_empty() || !pgm_core_internal::crypto::hmac_ok(secret.as_bytes(), body, sig.trim()) { return (401, json!({ "error": "The signature does not match." })); }
        let event = header("x-github-event").or_else(|| header("x-forgejo-event")).or_else(|| header("x-gitea-event")).unwrap_or_else(|| "unknown".into());
        let wanted: Vec<String> = t["webhook"]["events"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        if !wanted.is_empty() && !wanted.contains(&event) { return (202, json!({ "ignored": event })); }
        if t["paused"] == json!(true) { return (202, json!({ "paused": true })); }
        let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let prompt = render_with(s(&t, "prompt"), &json!({ "event": payload, "type": event }));
        let mut extra = serde_json::Map::new();
        extra.insert("event".into(), payload);
        extra.insert("eventType".into(), json!(event));
        match self.fire(&mut t, prompt, extra) { Ok(run) => (202, json!({ "run": run })), Err(e) => (422, json!({ "error": e })) }
    }

    /// A named event from another process on this server (T-212's internal sockets): every unpaused `event` trigger of
    /// that name starts its workflow with the payload as `inputs.event`. The runs it started.
    pub fn event(&self, name: &str, payload: &Value) -> Vec<String> {
        let mut out = vec![];
        for mut t in self.triggers_all() {
            if s(&t, "kind") != "event" || t["event"]["name"] != json!(name) || t["paused"] == json!(true) { continue; }
            let prompt = render_with(s(&t, "prompt"), &json!({ "event": payload, "type": name }));
            let mut extra = serde_json::Map::new();
            extra.insert("event".into(), payload.clone());
            extra.insert("eventType".into(), json!(name));
            if let Ok(run) = self.fire(&mut t, prompt, extra) { out.push(run); }
        }
        out
    }
}

fn title_of(t: &Value) -> String { t["title"].as_str().filter(|x| !x.is_empty()).map(str::to_string).unwrap_or_else(|| s(t, "id").to_string()) }

/// A trigger's prompt with `{{event.pull_request.title}}`-style holes from its event; a hole with nothing stays empty.
fn render_with(template: &str, root: &Value) -> String {
    if !template.contains("{{") { return template.to_string(); }
    let look = |p: &[String]| -> Option<Value> { Some(crate::host::walk(root, p).unwrap_or(Value::Null)) };
    pgm_workflow_internal::expr::render(template, &look).unwrap_or_else(|_| template.to_string())
}
