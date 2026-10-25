//! The workflow store and the run API (T-204; spec 7, 9, 15): what a hub (or the gateway) serves under
//! `/api/hyperwork/`. No web server here: [`Service::handle`] takes a method, a path, the query, the body and who asks,
//! and gives a JSON answer or a stream of NDJSON lines; the program around it maps that onto its own HTTP and says,
//! through [`Directory`], who belongs where and how a run of a person reaches AI, tools and people.
//!
//! The store is files (atomic writes, private):
//! - `<root>/<scope>/workflows/<id>/meta.json` (name, owner, perms, versions, the published pointer, tags),
//!   `v<N>.json` (each published version, never rewritten), `draft-<user>.json` (one draft per editor);
//! - `<root>/runs/<run>/run.json` (owner, workflow, version, status), `log.ndjson` (the run log, append-only, secrets
//!   scrubbed), `snapshot.json`; `<root>/<scope>/audit.ndjson` (who published, deleted, changed rights);
//! - scopes: `me-<user>` (personal, private), `team-<id>`, `grp-<id>`, `company`.
//!
//! A file that is not valid JSON is moved aside as `*.bad-<time>` and the store goes on (FR2).
use crate::cancel::Cancel;
use crate::engine::{Engine, RunInput, RunOptions, RunReport};
use crate::host::{Ask, BoxFut, HttpAnswer, HttpRequest, ModelAnswer, ModelRequest, NodeError, ToolCtx, WorkflowHost};
use pgm_workflow_internal::{diff, doc, validate, Document, KindManifest, KindRegistry, Options};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tokio::sync::{broadcast, mpsc};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }
fn now_ms() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) }
fn stamp() -> String {
    let s = now_ms() / 1000;
    let (d, t) = (s / 86_400, s % 86_400);
    // civil date from days since 1970 (Howard Hinnant's algorithm)
    let z = d as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (day, month) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{year:04}{month:02}{day:02}-{:02}{:02}{:02}", t / 3600, t / 60 % 60, t % 60)
}

/// What the service asks the hub (or the gateway) it lives in.
pub trait Directory: Send + Sync {
    fn exists(&self, user: &str) -> bool;
    fn is_admin(&self, user: &str) -> bool;
    /// The scopes a person is in, nearest first: `me-<user>`, their groups, their teams, `company`.
    fn scopes_of(&self, user: &str) -> Vec<String>;
    /// The host a run of this person uses (AI, tools, HTTP, agents, people). The service puts its own parts around it
    /// (the log, snapshots, the store's workflows, questions).
    fn host_for(&self, user: &str) -> Arc<dyn WorkflowHost>;
    /// Something about a person's run changed (a page shows it in their sessions list): `{type, run, ...}`.
    fn changed(&self, _user: &str, _event: &Value) {}
    /// Values that must never be written into a log (keys, tokens), besides the secrets a run asked its host for.
    fn secret_values(&self) -> Vec<String> { vec![] }
    /// The tools a run may use when its scope's policy names none (spec 9.3). None by default.
    fn default_tools(&self) -> Vec<String> { vec![] }
    /// A timer's next time after a fire at `ms` (T-211): `daily`, `nightly`, `weekly`, `biweekly`, `monthly`,
    /// `quarterly`, `half_year`, `yearly`. The hub answers with the board's own repeats (month ends, local time); the
    /// default knows the fixed ones.
    fn next_due(&self, ms: u64, recur: &str) -> Option<u64> {
        const DAY: u64 = 86_400_000;
        Some(ms + match recur { "daily" | "nightly" => DAY, "weekly" => 7 * DAY, "biweekly" => 14 * DAY, _ => return None })
    }
    /// The first time of day `at` ("HH:MM") after `ms`: the hub's in its local time, the default in UTC.
    fn first_time(&self, ms: u64, at: &str) -> Option<u64> {
        let (h, m) = at.split_once(':')?;
        let (h, m): (u64, u64) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
        if h > 23 || m > 59 { return None; }
        let day = ms - ms % 86_400_000;
        let t = day + (h * 60 + m) * 60_000;
        Some(if t > ms { t } else { t + 86_400_000 })
    }
    /// Tells a person something that went wrong while they were not looking (a triggered run failed, T-213): the hub
    /// sends it as a private chat message.
    fn alert(&self, _user: &str, _text: &str) {}
}

/// Who asks: a person, through a full client (the page, the apps) or a run-only one (jcode: list, view, run only).
#[derive(Debug, Clone)]
pub struct Caller {
    pub user: String,
    pub run_only: bool,
}

/// What a route answers.
pub enum Answer {
    Json(u16, Value),
    /// NDJSON lines (each ends with `\n`), until the sender is dropped.
    Lines(mpsc::Receiver<String>),
}

fn j(status: u16, v: Value) -> Answer { Answer::Json(status, v) }
fn bad(status: u16, msg: impl Into<String>) -> Answer { j(status, json!({ "error": msg.into() })) }

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rights {
    pub run: bool,
    pub view: bool,
    pub edit: bool,
    pub publish: bool,
    pub admin: bool,
}

impl Rights {
    fn json(&self) -> Value { json!({ "run": self.run, "view": self.view, "edit": self.edit, "publish": self.publish, "admin": self.admin }) }
}

struct Live {
    cancel: Cancel,
    tx: broadcast::Sender<Value>,
    /// values to scrub from this run's log (the secrets its host gave out)
    secrets: Arc<Mutex<Vec<String>>>,
}

pub struct Service {
    root: PathBuf,
    engine: Engine,
    dir: Arc<dyn Directory>,
    runs: Mutex<HashMap<String, Arc<Live>>>,
    rev: AtomicU64,
    me: Weak<Service>,
    /// writes of one workflow's files never interleave
    write: Mutex<()>,
    /// this service's own id, for the scheduler's lease (one hub fires the timers of a store)
    instance: String,
}

mod triggers;

fn read(p: &Path) -> Option<Value> {
    let t = std::fs::read_to_string(p).ok()?;
    match serde_json::from_str(&t) {
        Ok(v) => Some(v),
        Err(_) => { let _ = std::fs::rename(p, p.with_file_name(format!("{}.bad-{}", p.file_name()?.to_string_lossy(), stamp()))); None }
    }
}

fn write(p: &Path, v: &Value) -> bool {
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let tmp = p.with_extension(format!("tmp-{}", std::process::id()));
    let ok = std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).is_ok() && std::fs::rename(&tmp, p).is_ok();
    #[cfg(unix)]
    if ok { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)); }
    ok
}

fn append_line(p: &Path, line: &str) {
    use std::io::Write;
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) { let _ = writeln!(f, "{line}"); }
}

fn read_lines(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p).map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
}

fn semver(v: &str) -> (u32, u32, u32) { pgm_workflow_internal::kinds::parse_semver(v).unwrap_or((0, 0, 0)) }
fn safe(id: &str) -> bool { !id.is_empty() && id.len() <= 80 && id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) && !id.starts_with('.') }
fn s<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(Value::as_str).unwrap_or("") }

/// `prefix` first, then a word start, then anywhere (spec 10.2); None: no match.
fn rank(name: &str, q: &str) -> Option<u8> {
    if q.is_empty() { return Some(0); }
    let (n, q) = (name.to_lowercase(), q.to_lowercase());
    if n.starts_with(&q) { return Some(0); }
    if n.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&q)) { return Some(1); }
    if n.contains(&q) { Some(2) } else { None }
}

impl Service {
    pub fn new(root: PathBuf, engine: Engine, dir: Arc<dyn Directory>) -> Arc<Service> {
        let rev = read(&root.join("state.json")).and_then(|v| v["rev"].as_u64()).unwrap_or(0);
        Arc::new_cyclic(|me| Service { root, engine, dir, runs: Mutex::new(HashMap::new()), rev: AtomicU64::new(rev), me: me.clone(), write: Mutex::new(()), instance: pgm_core_internal::crypto::random_hex(8) })
    }

    pub fn engine(&self) -> &Engine { &self.engine }

    fn bump(&self) -> u64 {
        let r = self.rev.fetch_add(1, Ordering::SeqCst) + 1;
        write(&self.root.join("state.json"), &json!({ "rev": r }));
        r
    }

    fn wf_dir(&self, scope: &str, id: &str) -> PathBuf { self.root.join(scope).join("workflows").join(id) }
    fn run_dir(&self, run: &str) -> PathBuf { self.root.join("runs").join(run) }

    /// Every workflow's meta in the given scopes.
    fn metas(&self, scopes: &[String]) -> Vec<Value> {
        let mut out = vec![];
        for sc in scopes {
            let Ok(rd) = std::fs::read_dir(self.root.join(sc).join("workflows")) else { continue };
            for e in rd.flatten() {
                if let Some(m) = read(&e.path().join("meta.json")) { if m["deleted"] != json!(true) { out.push(m); } }
            }
        }
        out
    }

    fn meta_by_id(&self, id: &str) -> Option<Value> {
        if !safe(id) { return None; }
        let rd = std::fs::read_dir(&self.root).ok()?;
        for e in rd.flatten() {
            let p = e.path().join("workflows").join(id).join("meta.json");
            if p.exists() { return read(&p).filter(|m| m["deleted"] != json!(true)); }
        }
        None
    }

    fn save_meta(&self, m: &Value) -> bool { write(&self.wf_dir(s(m, "scope"), s(m, "id")).join("meta.json"), m) }

    fn audit(&self, scope: &str, who: &str, what: &str, more: Value) {
        let mut e = json!({ "at": now_ms(), "by": who, "what": what });
        if let (Some(o), Value::Object(m)) = (e.as_object_mut(), more) { for (k, v) in m { o.insert(k, v); } }
        append_line(&self.root.join(scope).join("audit.ndjson"), &e.to_string());
    }

    /// What a person may do with a workflow (spec 9.2).
    pub fn rights(&self, c: &Caller, m: &Value) -> Rights {
        let scope = s(m, "scope");
        let u = c.user.as_str();
        let mut r = if let Some(owner) = scope.strip_prefix("me-") {
            // a personal scope is its owner's alone
            if owner == u { Rights { run: true, view: true, edit: true, publish: true, admin: true } } else { Rights::default() }
        } else {
            let member = self.dir.scopes_of(u).iter().any(|x| x == scope);
            let owner = s(m, "owner") == u;
            let perms = &m["perms"];
            let listed = |right: &str| perms.get(right).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).any(|x| x == u || x == "*" || (member && matches!(x, "members" | "team" | "scope"))));
            let has = |right: &str, default: bool| owner || listed(right).unwrap_or(default);
            Rights {
                run: has("run", member),
                view: has("view", member),
                edit: has("edit", false),
                publish: has("publish", false),
                admin: has("admin", false) || (member && self.dir.is_admin(u)),
            }
        };
        // the stronger rights hold the weaker ones
        r.publish |= r.admin;
        r.edit |= r.publish;
        r.view |= r.edit || r.run;
        r.run |= r.edit;
        if c.run_only { r.edit = false; r.publish = false; r.admin = false; }
        r
    }

    fn version_doc(&self, m: &Value, v: u32) -> Option<Document> {
        doc::from_value(read(&self.wf_dir(s(m, "scope"), s(m, "id")).join(format!("v{v}.json")))?).ok()
    }
    fn draft_doc(&self, m: &Value, user: &str) -> Option<(Document, String)> {
        let v = read(&self.wf_dir(s(m, "scope"), s(m, "id")).join(format!("draft-{user}.json")))?;
        let etag = v["etag"].as_str().unwrap_or("").to_string();
        Some((doc::from_value(v["doc"].clone()).ok()?, etag))
    }
    fn published(&self, m: &Value) -> Option<(u32, Document)> {
        let v = m["published"].as_u64()? as u32;
        Some((v, self.version_doc(m, v)?))
    }
    fn resolve_range(&self, m: &Value, range: Option<&str>) -> Option<(u32, Document)> {
        match range.map(str::trim).filter(|r| !r.is_empty()) {
            None | Some("published") => self.published(m),
            Some(r) => {
                let v = r.parse::<u32>().ok().or_else(|| m["tags"][r].as_u64().map(|x| x as u32))?;
                Some((v, self.version_doc(m, v)?))
            }
        }
    }

    /// A workflow by id, or by name in the caller's scopes (nearest first; `scope` narrows it).
    fn find(&self, c: &Caller, id: Option<&str>, name: Option<&str>, scope: Option<&str>) -> Option<Value> {
        if let Some(id) = id.filter(|x| !x.is_empty()) { return self.meta_by_id(id); }
        let name = name.filter(|x| !x.is_empty())?;
        let (scope_part, bare) = match name.split_once('/') { Some((a, b)) if scope.is_none() => (Some(a.to_string()), b.to_string()), _ => (scope.map(str::to_string), name.to_string()) };
        let scopes = self.dir.scopes_of(&c.user);
        for sc in &scopes {
            if scope_part.as_ref().is_some_and(|p| p != sc && sc.split_once('-').map(|x| x.1) != Some(p.as_str())) { continue; }
            if let Some(m) = self.metas(std::slice::from_ref(sc)).into_iter().find(|m| s(m, "name") == bare) { return Some(m); }
        }
        None
    }

    /// The route `/api/hyperwork/<path>`.
    pub async fn handle(&self, c: &Caller, method: &str, path: &str, query: &HashMap<String, String>, body: &Value) -> Answer {
        if !self.dir.exists(&c.user) { return bad(401, "Not signed in."); }
        let q = |k: &str| query.get(k).map(String::as_str).filter(|x| !x.is_empty());
        let b = |k: &str| body.get(k).and_then(Value::as_str).filter(|x| !x.is_empty());
        let writes = matches!(path, "save" | "publish" | "promote" | "delete" | "restore" | "perms" | "import" | "policy" | "selection" | "ask") || ["law", "prompt", "group", "kind", "trigger"].iter().any(|k| path == *k || path.starts_with(&format!("{k}/")));
        if c.run_only && method == "POST" && writes { return bad(403, "This client may list, view and run workflows, not change them."); }
        match (method, path) {
            ("GET", "triggers") | ("POST", "trigger") | ("POST", "trigger/pause") | ("POST", "trigger/resume") | ("POST", "trigger/delete") => self.triggers_route(c, method, path, query, body),
            ("POST", "ask") => self.ask(c, b("id"), &body["doc"], b("message").unwrap_or(""), &body["selection"]).await,
            ("GET", "laws") | ("GET", "prompts") | ("GET", "groups") => self.shelf_get(c, Shelf::of(path), q("scope"), q("id"), q("version").and_then(|v| v.parse().ok())),
            ("POST", "law") | ("POST", "prompt") | ("POST", "group") => self.shelf_save(c, Shelf::of(path), body.get("item").unwrap_or(body), b("scope")),
            ("POST", "law/delete") | ("POST", "prompt/delete") | ("POST", "group/delete") => self.shelf_delete(c, Shelf::of(path), b("id")),
            ("GET", "list") => self.list(c, q("q").unwrap_or(""), q("scope"), q("since").and_then(|x| x.parse().ok())),
            ("GET", "get") => self.get(c, q("id"), q("name"), q("scope"), q("version").or(q("channel"))),
            ("GET", "versions") => self.with(c, q("id"), |r| r.view, |m| j(200, json!({ "versions": m["versions"], "published": m["published"], "tags": m["tags"] }))),
            ("GET", "diff") => self.diff(c, q("id"), q("a"), q("b")),
            ("GET", "kinds") => {
                let mut kinds: Vec<Value> = self.engine.registry().kinds().map(|k| { let mut v = serde_json::to_value(k).unwrap_or(Value::Null); v["trust"] = json!("built-in"); v["runnable"] = json!(self.engine.can_run(&k.kind)); v }).collect();
                let scopes: Vec<String> = self.dir.scopes_of(&c.user).into_iter().filter(|x| q("scope").is_none_or(|w| w == x)).collect();
                for r in self.kind_records(&scopes) {
                    let mut v = r["manifest"].clone();
                    for k in ["trust", "hash", "author", "scope", "reviewers", "at"] { v[k] = r[k].clone(); }
                    v["runnable"] = json!(self.trust_ok(&r, &c.user).is_ok());
                    kinds.push(v);
                }
                j(200, json!({ "kinds": kinds }))
            }
            ("POST", "kind") => self.kind_publish(c, &body["manifest"], b("scope")).await,
            ("POST", "kind/review") => self.kind_review(c, b("kind").unwrap_or(""), b("version").unwrap_or(""), b("scope")),
            ("POST", "kind/block") => self.kind_block(c, b("kind").unwrap_or(""), b("scope")),
            ("POST", "selection") => self.selection(c, &body["doc"], &body["nodes"], b("name").unwrap_or(""), b("title").unwrap_or("")).await,
            ("GET", "runs") => self.runs(c, q("workflow"), q("mine") == Some("1"), q("status")),
            ("GET", "run") => match self.run_meta_for(c, q("id").unwrap_or("")) { Ok(m) => j(200, m), Err(e) => e },
            ("GET", "log") => self.log(c, q("id").unwrap_or(""), q("from").and_then(|x| x.parse().ok()).unwrap_or(0), q("follow") == Some("1")),
            ("POST", "validate") => match doc::from_value(body["doc"].clone()) {
                Ok(d) => j(200, json!({ "problems": self.problems_in(&d, &c.user) })),
                Err(e) => bad(400, e.to_string()),
            },
            ("POST", "save") => self.save(c, b("id"), &body["doc"], b("etag"), b("scope")),
            ("POST", "publish") => self.publish(c, b("id"), b("message").unwrap_or("")),
            ("POST", "restore") => self.restore(c, b("id"), body["version"].as_u64()),
            ("POST", "perms") => self.with(c, b("id"), |r| r.admin, |mut m| {
                m["perms"] = body["perms"].clone();
                m["rev"] = json!(self.bump());
                self.save_meta(&m);
                self.audit(s(&m, "scope"), &c.user, "perms", json!({ "id": m["id"], "perms": m["perms"] }));
                j(200, json!({ "ok": true, "perms": m["perms"] }))
            }),
            ("POST", "delete") => self.with(c, b("id"), |r| r.admin, |mut m| {
                m["deleted"] = json!(true);
                m["rev"] = json!(self.bump());
                self.save_meta(&m);
                self.audit(s(&m, "scope"), &c.user, "delete", json!({ "id": m["id"], "name": m["name"] }));
                j(200, json!({ "ok": true }))
            }),
            ("POST", "promote") => self.promote(c, b("id"), b("scope")),
            ("POST", "run") => self.start_run(c, body),
            ("POST", "command") => self.command(c, b("line").unwrap_or(""), b("project")),
            ("POST", "cancel") => match self.run_meta_for(c, b("id").unwrap_or("")) {
                Ok(m) if s(&m, "owner") == c.user || self.meta_by_id(s(&m["workflow"], "id")).is_some_and(|w| self.rights(c, &w).admin) => {
                    match lock(&self.runs).get(s(&m, "id")) { Some(l) => { l.cancel.cancel(); j(200, json!({ "ok": true })) } None => bad(409, "That run is not running.") }
                }
                Ok(_) => bad(403, "Only the person who started a run (or an admin of its workflow) may cancel it."),
                Err(e) => e,
            },
            ("POST", "answer") => self.answer(c, b("run").unwrap_or(""), b("node").or(b("key")).unwrap_or(""), body.get("answers").cloned().unwrap_or(Value::Null)).await,
            ("POST", "replay") => self.replay(c, b("id").unwrap_or("")),
            ("POST", "export") => self.with(c, b("id"), |r| r.view, |m| match self.published(&m).or_else(|| self.draft_doc(&m, &c.user).map(|d| (0, d.0))) {
                Some((v, d)) => {
                    let (laws, groups, prompts) = self.used_by(&d, s(&m, "owner"), s(&m, "scope"));
                    j(200, json!({ "bundle": { "schema": 1, "workflow": doc::to_value(&d), "version": v, "laws": laws, "groups": groups, "prompts": prompts, "kinds": [] } }))
                }
                None => bad(404, "That workflow has no version to export."),
            }),
            ("POST", "import") => self.import(c, &body["bundle"], b("scope")),
            ("GET", "policy") | ("POST", "policy") => self.policy(c, method, q("scope").or(b("scope")), &body["policy"]),
            _ => bad(404, "Unknown endpoint"),
        }
    }

    fn with(&self, c: &Caller, id: Option<&str>, need: fn(&Rights) -> bool, f: impl FnOnce(Value) -> Answer) -> Answer {
        let Some(m) = id.and_then(|i| self.meta_by_id(i)) else { return bad(404, "No such workflow.") };
        let r = self.rights(c, &m);
        if !(r.view || r.run) { return bad(404, "No such workflow."); }
        if !need(&r) { return bad(403, "You do not have the right for that on this workflow."); }
        f(m)
    }

    fn list(&self, c: &Caller, q: &str, scope: Option<&str>, since: Option<u64>) -> Answer {
        let scopes: Vec<String> = self.dir.scopes_of(&c.user).into_iter().filter(|x| scope.is_none_or(|w| w == x)).collect();
        let mut rows: Vec<(u8, usize, u64, Value)> = vec![];
        for m in self.metas(&scopes) {
            let r = self.rights(c, &m);
            if !(r.view || r.run) { continue; }
            if since.is_some_and(|x| m["rev"].as_u64().unwrap_or(0) <= x) { continue; }
            let pub_doc = self.published(&m);
            let draft = r.edit && self.draft_doc(&m, &c.user).is_some();
            if pub_doc.is_none() && !draft { continue; }
            let Some(rk) = rank(s(&m, "name"), q) else { continue };
            let near = scopes.iter().position(|x| x == s(&m, "scope")).unwrap_or(usize::MAX);
            let d = pub_doc.as_ref().map(|x| &x.1);
            let unrunnable = match d { Some(d) => self.engine.preflight(d).first().and_then(|p| p["message"].as_str().map(str::to_string)), None => Some("not published yet (only your draft)".into()) };
            let row = json!({
                "id": m["id"], "name": m["name"], "title": d.and_then(|d| d.title.clone()), "description": d.and_then(|d| d.description.clone()), "scope": m["scope"],
                "version": pub_doc.as_ref().map(|x| x.0), "labels": d.and_then(|d| d.labels.clone()), "runOn": d.and_then(|d| d.run_on.clone()), "needsProject": d.and_then(|d| d.needs_project),
                "rights": r.json(), "draft": draft, "unrunnable": unrunnable, "updatedAt": m["updatedAt"],
            });
            rows.push((rk, near, m["updatedAt"].as_u64().unwrap_or(0), row));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0).then(if q.is_empty() { b.2.cmp(&a.2) } else { a.1.cmp(&b.1) }).then_with(|| s(&a.3, "name").cmp(s(&b.3, "name"))));
        // the same name in two scopes: both are listed, the farther one as scope/name
        let mut seen: Vec<String> = vec![];
        let items: Vec<Value> = rows.into_iter().map(|(_, _, _, mut row)| {
            let n = s(&row, "name").to_string();
            if seen.contains(&n) { row["shown"] = json!(format!("{}/{n}", s(&row, "scope").split_once('-').map(|x| x.1).unwrap_or(s(&row, "scope")))); } else { row["shown"] = json!(n.clone()); seen.push(n); }
            row
        }).collect();
        j(200, json!({ "items": items, "rev": self.rev.load(Ordering::SeqCst) }))
    }

    fn get(&self, c: &Caller, id: Option<&str>, name: Option<&str>, scope: Option<&str>, version: Option<&str>) -> Answer {
        let Some(m) = self.find(c, id, name, scope) else { return bad(404, "No such workflow.") };
        let r = self.rights(c, &m);
        if !(r.view || r.run) { return bad(404, "No such workflow."); }
        let (d, v, etag) = match version {
            Some("draft") => {
                if !r.edit { return bad(403, "Only an editor sees a draft."); }
                match self.draft_doc(&m, &c.user) { Some((d, e)) => (d, Value::Null, e), None => return bad(404, "You have no draft of this workflow.") }
            }
            Some(x) => match self.resolve_range(&m, Some(x)) { Some((v, d)) => (d, json!(v), String::new()), None => return bad(404, "No such version.") },
            None => match (r.edit.then(|| self.draft_doc(&m, &c.user)).flatten(), self.published(&m)) {
                (Some((d, e)), _) => (d, Value::Null, e),
                (None, Some((v, d))) => (d, json!(v), String::new()),
                (None, None) => return bad(404, "That workflow has no published version."),
            },
        };
        let mut meta = m.clone();
        if let Some(o) = meta.as_object_mut() { o.remove("drafts"); }
        j(200, json!({ "doc": doc::to_value(&d), "version": v, "etag": etag, "rights": r.json(), "meta": meta }))
    }

    fn save(&self, c: &Caller, id: Option<&str>, docv: &Value, etag: Option<&str>, scope: Option<&str>) -> Answer {
        let _w = lock(&self.write);
        let mut dv = docv.clone();
        if !dv.is_object() { return bad(400, "`doc` must be a workflow document."); }
        let existing = id.and_then(|i| self.meta_by_id(i));
        if id.is_some() && existing.is_none() { return bad(404, "No such workflow."); }
        let mut m = match existing {
            Some(m) => {
                if !self.rights(c, &m).edit { return bad(403, "You may not edit this workflow."); }
                if let (Some(e), Some((_, cur))) = (etag, self.draft_doc(&m, &c.user)) { if e != cur { return j(409, json!({ "error": "Your draft changed since you opened it (another tab or device saved it).", "etag": cur })); } }
                m
            }
            None => {
                let sc = scope.map(str::to_string).unwrap_or_else(|| format!("me-{}", c.user));
                if !self.dir.scopes_of(&c.user).contains(&sc) || !safe(&sc) { return bad(403, "You may not make workflows in that scope."); }
                let new_id = format!("wf-{:012x}", (now_ms() ^ ((self.rev.load(Ordering::SeqCst) + 1) << 44)) & 0xffff_ffff_ffff);
                json!({ "id": new_id, "scope": sc, "owner": c.user, "createdAt": now_ms(), "versions": [], "published": null, "tags": {}, "perms": null })
            }
        };
        let name = dv.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if name.is_empty() || !name.chars().all(|ch| ch.is_alphanumeric() || "-_.".contains(ch)) { return bad(400, "A workflow needs a `name` of letters, digits and - _ . (it is what /hyperwork types)."); }
        if self.metas(&[s(&m, "scope").to_string()]).iter().any(|x| s(x, "name") == name && x["id"] != m["id"]) { return j(409, json!({ "error": format!("`{name}` is taken in this scope.") })); }
        dv["id"] = m["id"].clone();
        dv["scope"] = m["scope"].clone();
        dv["owner"] = m["owner"].clone();
        if dv.get("schema").is_none() { dv["schema"] = json!(pgm_workflow_internal::SCHEMA); }
        if dv.get("nodes").is_none() { dv["nodes"] = json!([]); }
        if dv.get("edges").is_none() { dv["edges"] = json!([]); }
        let d = match doc::from_value(dv) { Ok(d) => d, Err(e) => return bad(400, e.to_string()) };
        let etag = format!("{:016x}", { use std::hash::{Hash, Hasher}; let mut h = std::collections::hash_map::DefaultHasher::new(); doc::write(&d).hash(&mut h); now_ms().hash(&mut h); h.finish() });
        m["name"] = json!(name);
        m["updatedAt"] = json!(now_ms());
        m["rev"] = json!(self.bump());
        m["drafts"][&c.user] = json!({ "etag": etag, "at": now_ms() });
        if !write(&self.wf_dir(s(&m, "scope"), s(&m, "id")).join(format!("draft-{}.json", c.user)), &json!({ "etag": etag, "doc": doc::to_value(&d) })) || !self.save_meta(&m) { return bad(500, "Could not save the draft."); }
        j(200, json!({ "ok": true, "id": m["id"], "etag": etag, "problems": self.problems_in(&d, s(&m, "owner")) }))
    }

    fn publish(&self, c: &Caller, id: Option<&str>, message: &str) -> Answer {
        let _w = lock(&self.write);
        let Some(mut m) = id.and_then(|i| self.meta_by_id(i)) else { return bad(404, "No such workflow.") };
        let r = self.rights(c, &m);
        if !(r.view || r.run) { return bad(404, "No such workflow."); }
        if !r.publish { return bad(403, "You may not publish this workflow."); }
        if message.trim().is_empty() { return bad(400, "Publishing needs a message (what changed)."); }
        let Some((mut d, _)) = self.draft_doc(&m, &c.user) else { return bad(409, "You have no draft to publish.") };
        let problems: Vec<Value> = self.problems_in(&d, s(&m, "owner")).into_iter().filter(|p| p["level"] == "error").collect();
        if !problems.is_empty() { return j(422, json!({ "error": "The draft has errors.", "problems": problems })); }
        let next = m["versions"].as_array().map(|a| a.iter().filter_map(|v| v["version"].as_u64()).max().unwrap_or(0)).unwrap_or(0) as u32 + 1;
        let at = now_ms();
        d.version = Some(next);
        d.published_at = Some(at);
        d.published_by = Some(c.user.clone());
        d.message = Some(message.to_string());
        if !write(&self.wf_dir(s(&m, "scope"), s(&m, "id")).join(format!("v{next}.json")), &doc::to_value(&d)) { return bad(500, "Could not write the version."); }
        m["published"] = json!(next);
        if !m["versions"].is_array() { m["versions"] = json!([]); }
        m["versions"].as_array_mut().unwrap().push(json!({ "version": next, "at": at, "by": c.user, "message": message }));
        m["updatedAt"] = json!(at);
        m["rev"] = json!(self.bump());
        self.save_meta(&m);
        self.audit(s(&m, "scope"), &c.user, "publish", json!({ "id": m["id"], "name": m["name"], "version": next, "message": message }));
        j(200, json!({ "ok": true, "version": next }))
    }

    fn restore(&self, c: &Caller, id: Option<&str>, version: Option<u64>) -> Answer {
        let Some(m) = id.and_then(|i| self.meta_by_id(i)) else { return bad(404, "No such workflow.") };
        if !self.rights(c, &m).edit { return bad(403, "You may not edit this workflow."); }
        let Some(d) = version.and_then(|v| self.version_doc(&m, v as u32)) else { return bad(404, "No such version.") };
        let d = pgm_workflow_internal::version::restore(&d, now_ms());
        self.save(c, Some(s(&m, "id")), &doc::to_value(&d), None, None)
    }

    fn diff(&self, c: &Caller, id: Option<&str>, a: Option<&str>, b: Option<&str>) -> Answer {
        self.with(c, id, |r| r.view, |m| {
            let pick = |x: Option<&str>| -> Option<Document> { match x { Some("draft") => self.draft_doc(&m, &c.user).map(|d| d.0), x => self.resolve_range(&m, x).map(|d| d.1) } };
            match (pick(a), pick(b)) { (Some(x), Some(y)) => j(200, json!({ "diff": diff(&x, &y).to_json(), "summary": diff(&x, &y).summary() })), _ => bad(404, "No such version.") }
        })
    }

    fn promote(&self, c: &Caller, id: Option<&str>, target: Option<&str>) -> Answer {
        let Some(m) = id.and_then(|i| self.meta_by_id(i)) else { return bad(404, "No such workflow.") };
        if !self.rights(c, &m).publish { return bad(403, "You may not promote this workflow."); }
        let Some(t) = target.filter(|t| safe(t) && self.dir.scopes_of(&c.user).iter().any(|x| x == t)) else { return bad(403, "You are not in that scope.") };
        if self.metas(&[t.to_string()]).iter().any(|x| x["name"] == m["name"]) { return j(409, json!({ "error": format!("`{}` is taken in {t}.", s(&m, "name")) })); }
        let _w = lock(&self.write);
        let new_id = format!("{}-{}", s(&m, "id"), t.split_once('-').map(|x| x.1).unwrap_or(t));
        let (from, to) = (self.wf_dir(s(&m, "scope"), s(&m, "id")), self.wf_dir(t, &new_id));
        let _ = std::fs::create_dir_all(&to);
        for e in std::fs::read_dir(&from).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with('v') && n.ends_with(".json") {
                if let Some(mut v) = read(&e.path()) { v["id"] = json!(new_id); v["scope"] = json!(t); write(&to.join(&n), &v); }
            }
        }
        let mut nm = m.clone();
        nm["id"] = json!(new_id);
        nm["scope"] = json!(t);
        nm["promotedFrom"] = json!({ "id": m["id"], "scope": m["scope"] });
        if let Some(o) = nm.as_object_mut() { o.remove("drafts"); }
        nm["rev"] = json!(self.bump());
        self.save_meta(&nm);
        self.audit(t, &c.user, "promote", json!({ "id": new_id, "from": m["id"], "fromScope": m["scope"] }));
        j(200, json!({ "ok": true, "id": new_id, "scope": t }))
    }

    fn policy(&self, c: &Caller, method: &str, scope: Option<&str>, policy: &Value) -> Answer {
        let Some(sc) = scope.filter(|x| safe(x)) else { return bad(400, "Which scope?") };
        if !self.dir.scopes_of(&c.user).iter().any(|x| x == sc) { return bad(404, "No such scope."); }
        let f = self.root.join(sc).join("policy.json");
        if method == "GET" { return j(200, json!({ "policy": read(&f).unwrap_or(json!({})) })); }
        let admin = sc.strip_prefix("me-") == Some(c.user.as_str()) || self.dir.is_admin(&c.user);
        if !admin { return bad(403, "Only an admin changes a scope's policy."); }
        if !policy.is_object() { return bad(400, "`policy` must be an object."); }
        write(&f, policy);
        self.audit(sc, &c.user, "policy", json!({ "policy": policy }));
        j(200, json!({ "ok": true }))
    }

    /// A scope's policy (spec 9.3), `{}` when it has none.
    pub fn scope_policy(&self, scope: &str) -> Value { if safe(scope) { read(&self.root.join(scope).join("policy.json")).unwrap_or(json!({})) } else { json!({}) } }

    // ------------------------------------------------------------ laws, saved prompts, groups (T-202, spec 6)

    fn shelf_dir(&self, scope: &str, sh: Shelf) -> PathBuf { self.root.join(scope).join(sh.folder()) }

    /// Every current item of a kind in the given scopes (nearest scope first), each tagged with its scope.
    fn shelf_items(&self, scopes: &[String], sh: Shelf) -> Vec<Value> {
        let mut out = vec![];
        for sc in scopes {
            let mut here: Vec<Value> = std::fs::read_dir(self.shelf_dir(sc, sh)).into_iter().flatten().flatten()
                .filter(|e| { let n = e.file_name().to_string_lossy().to_string(); n.ends_with(".json") && !n.contains(".v") && !n.contains(".deleted") })
                .filter_map(|e| read(&e.path())).map(|mut v| { v["scope"] = json!(sc); v }).collect();
            here.sort_by(|a, b| s(a, "name").cmp(s(b, "name")));
            out.extend(here);
        }
        out
    }

    /// May this person change the laws / prompts / groups of a scope? Their own personal scope; another scope they are
    /// in when they are an admin or the scope's policy names them in `lawEditors`.
    fn may_shelve(&self, c: &Caller, scope: &str) -> bool {
        if scope.strip_prefix("me-") == Some(c.user.as_str()) { return true; }
        if scope.starts_with("me-") || !self.dir.scopes_of(&c.user).iter().any(|x| x == scope) { return false; }
        self.dir.is_admin(&c.user) || self.scope_policy(scope)["lawEditors"].as_array().is_some_and(|a| a.iter().any(|x| x.as_str() == Some(c.user.as_str())))
    }

    fn shelf_get(&self, c: &Caller, sh: Shelf, scope: Option<&str>, id: Option<&str>, version: Option<u32>) -> Answer {
        let scopes: Vec<String> = self.dir.scopes_of(&c.user).into_iter().filter(|x| scope.is_none_or(|w| w == x)).collect();
        let items = self.shelf_items(&scopes, sh);
        let Some(id) = id else { return j(200, json!({ "items": items })) };
        let Some(it) = items.into_iter().find(|x| s(x, "id") == id) else { return bad(404, format!("No such {}.", sh.one())) };
        match version {
            None => j(200, json!({ "item": it })),
            Some(v) => match read(&self.shelf_dir(s(&it, "scope"), sh).join(format!("{id}.v{v}.json"))) { Some(mut x) => { x["scope"] = it["scope"].clone(); j(200, json!({ "item": x })) } None => bad(404, "No such version.") },
        }
    }

    fn shelf_save(&self, c: &Caller, sh: Shelf, item: &Value, scope: Option<&str>) -> Answer {
        let _w = lock(&self.write);
        let mut it = item.clone();
        if !it.is_object() { return bad(400, format!("A {} is an object.", sh.one())); }
        let existing = it.get("id").and_then(Value::as_str).and_then(|id| self.shelf_items(&self.dir.scopes_of(&c.user), sh).into_iter().find(|x| s(x, "id") == id));
        let sc = match &existing { Some(e) => s(e, "scope").to_string(), None => scope.map(str::to_string).unwrap_or_else(|| format!("me-{}", c.user)) };
        if !safe(&sc) || !self.may_shelve(c, &sc) { return bad(403, format!("You may not change the {}s of that scope.", sh.one())); }
        if it.get("id").and_then(Value::as_str).is_none_or(str::is_empty) { it["id"] = json!(format!("{}{:010x}", sh.prefix(), (now_ms() << 4 ^ self.rev.fetch_add(1, Ordering::SeqCst)) & 0xff_ffff_ffff)); }
        let id = s(&it, "id").to_string();
        if !safe(&id) { return bad(400, "That id cannot be a file name."); }
        let version = existing.as_ref().and_then(|e| e["version"].as_u64()).unwrap_or(0) + 1;
        it["version"] = json!(version);
        it["scope"] = json!(sc);
        it["owner"] = existing.as_ref().map(|e| e["owner"].clone()).unwrap_or(json!(c.user));
        it["updatedAt"] = json!(now_ms());
        let problems = sh.problems(&it);
        if !problems.is_empty() { return j(400, json!({ "error": format!("That {} has problems.", sh.one()), "problems": problems })); }
        let dirp = self.shelf_dir(&sc, sh);
        if !write(&dirp.join(format!("{id}.v{version}.json")), &it) || !write(&dirp.join(format!("{id}.json")), &it) { return bad(500, "Could not save it."); }
        self.audit(&sc, &c.user, &format!("{}-save", sh.one()), json!({ "id": id, "version": version }));
        j(200, json!({ "ok": true, "id": id, "version": version, "item": it }))
    }

    fn shelf_delete(&self, c: &Caller, sh: Shelf, id: Option<&str>) -> Answer {
        let Some(it) = id.and_then(|id| self.shelf_items(&self.dir.scopes_of(&c.user), sh).into_iter().find(|x| s(x, "id") == id)) else { return bad(404, format!("No such {}.", sh.one())) };
        let sc = s(&it, "scope").to_string();
        if !self.may_shelve(c, &sc) { return bad(403, format!("You may not change the {}s of that scope.", sh.one())); }
        // the versions stay (the audit); a group's members are never touched
        let dirp = self.shelf_dir(&sc, sh);
        let _ = std::fs::rename(dirp.join(format!("{}.json", s(&it, "id"))), dirp.join(format!("{}.deleted-{}.json", s(&it, "id"), stamp())));
        self.audit(&sc, &c.user, &format!("{}-delete", sh.one()), json!({ "id": it["id"] }));
        j(200, json!({ "ok": true }))
    }

    /// The laws a run of `owner`'s in `scope` may use (every one they can see) and the scope's standing laws.
    fn law_book_for(&self, owner: &str, scope: &str) -> crate::host::LawBook {
        let mut scopes = self.dir.scopes_of(owner);
        if !scopes.iter().any(|x| x == scope) { scopes.push(scope.to_string()); }
        let laws = self.shelf_items(&scopes, Shelf::Laws).into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
        let groups = self.shelf_items(&scopes, Shelf::Groups).into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
        let defaults = self.scope_policy(scope)["laws"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        crate::host::LawBook { laws, groups, defaults }
    }

    /// A saved prompt as `owner` sees it: `name` or id (its latest version) or `name@3` (that version); its slots
    /// filled with their defaults when they all have one.
    fn saved_prompt_for(&self, owner: &str, name: &str) -> Option<String> {
        let (n, v) = match name.rsplit_once('@') { Some((a, b)) if b.parse::<u32>().is_ok() => (a, b.parse::<u32>().ok()), _ => (name, None) };
        let it = self.shelf_items(&self.dir.scopes_of(owner), Shelf::Prompts).into_iter().find(|x| s(x, "name") == n || s(x, "id") == n)?;
        let it = match v { None => it, Some(v) => read(&self.shelf_dir(s(&it, "scope"), Shelf::Prompts).join(format!("{}.v{v}.json", s(&it, "id"))))? };
        let p: pgm_workflow_internal::shapes::Prompt = serde_json::from_value(it.clone()).ok()?;
        Some(p.fill(&serde_json::Map::new()).unwrap_or(p.text))
    }

    /// The problems of a document in its scope: validation with the store's laws and prompts, then what the engine
    /// cannot run.
    fn problems_in(&self, d: &Document, owner: &str) -> Vec<Value> {
        let info = StoreScope { svc: self, owner };
        let (eng, kind_problems) = self.engine_for(owner, d);
        // a custom kind that may not run here first (it is the reason validation then calls the kind unknown)
        let mut out: Vec<Value> = kind_problems.into_iter().map(|mut p| { p["level"] = json!("error"); p }).collect();
        out.extend(validate(d, &Options::new(eng.registry()).scope(&info)).issues.into_iter().map(|i| serde_json::to_value(i).unwrap_or(Value::Null)));
        for p in eng.preflight(d) { if !out.iter().any(|x| x["code"] == p["code"] && x["message"] == p["message"]) { let mut p = p; p["level"] = json!("error"); out.push(p); } }
        out
    }

    /// The laws, groups and saved prompts a document uses (for an export bundle).
    fn used_by(&self, d: &Document, owner: &str, scope: &str) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
        let mut scopes = self.dir.scopes_of(owner);
        if !scopes.iter().any(|x| x == scope) { scopes.push(scope.to_string()); }
        let mut ids: Vec<String> = vec![];
        let mut add = |l: &Option<Vec<String>>| for x in l.iter().flatten() { let x = x.trim_start_matches('-').to_string(); if !ids.contains(&x) { ids.push(x); } };
        add(&d.defaults.as_ref().and_then(|x| x.laws.clone()));
        for sct in d.sections.iter().flatten() { add(&sct.laws); }
        let all_nodes: Vec<&pgm_workflow_internal::Node> = d.nodes.iter().chain(d.subgraphs.iter().flat_map(|m| m.values().flat_map(|g| g.nodes.iter()))).collect();
        for n in &all_nodes { add(&n.laws); }
        let groups: Vec<Value> = self.shelf_items(&scopes, Shelf::Groups).into_iter().filter(|g| ids.iter().any(|i| i == s(g, "id"))).collect();
        for g in &groups { for m in g["members"].as_array().into_iter().flatten().filter_map(Value::as_str) { if !ids.iter().any(|i| i == m) { ids.push(m.to_string()); } } }
        let laws: Vec<Value> = self.shelf_items(&scopes, Shelf::Laws).into_iter().filter(|l| ids.iter().any(|i| i == s(l, "id"))).collect();
        let mut refs: Vec<String> = vec![];
        fn scan(v: &Value, refs: &mut Vec<String>) {
            match v {
                Value::String(t) if t.contains("{{") => { if let Ok(tp) = pgm_workflow_internal::expr::parse_template(t) { for r in tp.prompt_refs() { let r = r.split('@').next().unwrap_or(r).to_string(); if !refs.contains(&r) { refs.push(r); } } } }
                Value::Array(a) => for x in a { scan(x, refs) },
                Value::Object(o) => for x in o.values() { scan(x, refs) },
                _ => {}
            }
        }
        for n in &all_nodes { if let Some(cfg) = &n.config { scan(cfg, &mut refs); } for p in n.prompts.iter().flatten() { if !refs.contains(p) { refs.push(p.clone()); } } }
        let prompts: Vec<Value> = self.shelf_items(&scopes, Shelf::Prompts).into_iter().filter(|p| refs.iter().any(|r| r == s(p, "name") || r == s(p, "id"))).collect();
        (laws, groups, prompts)
    }

    /// An exported bundle into a scope: its laws, groups and prompts are checked and added where the scope lacks them
    /// (by id), then the workflow becomes a new draft there.
    fn import(&self, c: &Caller, bundle: &Value, scope: Option<&str>) -> Answer {
        let sc = scope.map(str::to_string).unwrap_or_else(|| format!("me-{}", c.user));
        let mut problems = vec![];
        for (key, sh) in [("laws", Shelf::Laws), ("groups", Shelf::Groups), ("prompts", Shelf::Prompts)] {
            for it in bundle[key].as_array().into_iter().flatten() { for p in sh.problems(it) { problems.push(json!({ "id": it["id"], "problem": p })); } }
        }
        if !problems.is_empty() { return j(400, json!({ "error": "The bundle has problems.", "problems": problems })); }
        for (key, sh) in [("laws", Shelf::Laws), ("groups", Shelf::Groups), ("prompts", Shelf::Prompts)] {
            let have: Vec<String> = self.shelf_items(&self.dir.scopes_of(&c.user), sh).iter().map(|x| s(x, "id").to_string()).collect();
            for it in bundle[key].as_array().into_iter().flatten().filter(|it| !have.iter().any(|h| h == s(it, "id"))) {
                let mut it = it.clone();
                if let Some(o) = it.as_object_mut() { o.remove("version"); o.remove("scope"); o.remove("owner"); }
                if let Answer::Json(st, v) = self.shelf_save(c, sh, &it, Some(&sc)) { if st != 200 { return j(st, v); } }
            }
        }
        let mut wf = bundle["workflow"].clone();
        if let Some(o) = wf.as_object_mut() { o.remove("id"); o.remove("version"); o.remove("publishedAt"); o.remove("publishedBy"); }
        self.save(c, None, &wf, None, Some(&sc))
    }

    // ------------------------------------------------------------ the AI side chat (T-206, spec 10.1)

    /// The side chat next to the editor: a plain AI call (the person's own AI, through their host) about one workflow,
    /// told the node kinds this person may use. It answers with text and, when asked for a change, a **patch** (the
    /// editor's commands, crate::patch), tried on the document here so its problems come with it. It writes nothing,
    /// runs nothing, publishes nothing: the person accepts the patch into their draft, or not. With no AI it says so.
    async fn ask(&self, c: &Caller, id: Option<&str>, docv: &Value, message: &str, selection: &Value) -> Answer {
        if message.trim().is_empty() { return bad(400, "Ask something."); }
        let d = if docv.is_object() {
            let mut dv = docv.clone();
            if dv.get("id").is_none() { dv["id"] = json!("wf-draft"); }
            match doc::from_value(dv) { Ok(d) => d, Err(e) => return bad(400, e.to_string()) }
        } else {
            let Some(m) = id.and_then(|i| self.meta_by_id(i)) else { return bad(404, "No such workflow.") };
            let r = self.rights(c, &m);
            if !r.view { return bad(404, "No such workflow."); }
            match self.draft_doc(&m, &c.user).map(|x| x.0).or_else(|| self.published(&m).map(|x| x.1)) { Some(d) => d, None => return bad(404, "That workflow has no version.") }
        };
        let (eng, _) = self.engine_for(&c.user, &d);
        let catalogue: Vec<Value> = eng.registry().kinds().filter(|k| eng.can_run(&k.kind) || matches!(k.kind.as_str(), "note" | "comment")).map(|k| json!({
            "kind": k.kind, "title": k.title, "category": k.category,
            "in": k.ports.inputs.iter().map(|p| format!("{}:{}{}", p.name, p.ty, if p.required == Some(true) { " (required)" } else { "" })).collect::<Vec<_>>(),
            "out": k.ports.outputs.iter().map(|p| format!("{}:{}", p.name, p.ty)).collect::<Vec<_>>(),
            "config": k.config.as_ref().and_then(|s| s.get("properties")).and_then(Value::as_object).map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
        })).collect();
        let system = "You help a person design a Hyper Workflow in VibeRoller: a graph of nodes joined by wires (\"node.port\" to \"node.port\"). You read the workflow, explain it and suggest changes. A change is a patch: a list of commands, each one of {\"type\":\"add\",\"kind\":K,\"id\":ID,\"x\":X,\"y\":Y,\"props\":{config}}, {\"type\":\"connect\",\"from\":\"a.port\",\"to\":\"b.port\"}, {\"type\":\"disconnect\",\"from\":\"a.port\",\"to\":\"b.port\"}, {\"type\":\"setProp\",\"ids\":[ID],\"props\":{config}}, {\"type\":\"delete\",\"ids\":[ID]}, {\"type\":\"move\",\"ids\":[ID],\"dx\":X,\"dy\":Y}. Use only the node kinds and ports listed. You never run, publish or save anything: the person accepts or rejects your patch. Answer with JSON only: {\"text\": \"your answer\", \"patch\": [commands, or empty]}.";
        let user = format!("Node kinds:\n{}\n\nThe workflow:\n{}\n\nSelected: {}\n\n{message}", serde_json::to_string(&catalogue).unwrap_or_default(), doc::write(&d), if selection.is_null() { "nothing".into() } else { selection.to_string() });
        let req = ModelRequest { run: format!("ask-{}", c.user), key: "side-chat".into(), node: "side-chat".into(), attempt: 1, model: None,
            messages: vec![json!({ "role": "system", "content": system }), json!({ "role": "user", "content": user })], options: json!({ "responseFormat": "json" }) };
        let a = match self.dir.host_for(&c.user).call_model(req, Cancel::new()).await {
            Ok(a) => a,
            Err(e) if matches!(e.code.as_str(), "engine.unavailable" | "model.unreachable" | "model.rate-limited" | "engine.model-unreachable") =>
                return j(200, json!({ "noAi": true, "text": format!("No AI is available to you right now ({}); the editor works without it.", e.message), "patch": [] })),
            Err(e) => return bad(502, e.message),
        };
        let v = crate::kinds::ai::json_in(&a.text).unwrap_or(Value::Null);
        let text = v.get("text").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| a.text.clone());
        let patch: Vec<Value> = v.get("patch").and_then(Value::as_array).cloned().unwrap_or_default();
        let problems: Vec<Value> = if patch.is_empty() { vec![] } else {
            match crate::patch::apply(&d, &patch) {
                Ok(nd) => self.problems_in(&nd, &c.user).into_iter().filter(|p| p["level"] == "error").collect(),
                Err((i, m)) => vec![json!({ "code": "patch", "index": i, "message": m, "level": "error" })],
            }
        };
        j(200, json!({ "text": text, "patch": patch, "problems": problems }))
    }

    // ------------------------------------------------------------ custom node kinds (T-203, spec 11)

    fn kind_dir(&self, scope: &str, kind: &str) -> PathBuf { self.root.join(scope).join("kinds").join(kind.replace('/', "__")) }

    /// Every stored version of every custom kind in the given scopes (nearest scope first, newest version first), each
    /// `{manifest, hash, author, at, trust, reviewers, scope}`.
    fn kind_records(&self, scopes: &[String]) -> Vec<Value> {
        let mut out = vec![];
        for sc in scopes {
            let mut here = vec![];
            for kd in std::fs::read_dir(self.root.join(sc).join("kinds")).into_iter().flatten().flatten() {
                for f in std::fs::read_dir(kd.path()).into_iter().flatten().flatten() {
                    if let Some(mut r) = read(&f.path()) { r["scope"] = json!(sc); here.push(r); }
                }
            }
            here.sort_by(|a, b| s(&a["manifest"], "kind").cmp(s(&b["manifest"], "kind")).then_with(|| semver(s(&b["manifest"], "version")).cmp(&semver(s(&a["manifest"], "version")))));
            out.extend(here);
        }
        out
    }

    /// May this person run (or add) this version? `blocked`: nobody; `author-only` (not reviewed yet): its author;
    /// `reviewed`: everyone who sees it.
    fn trust_ok(&self, r: &Value, user: &str) -> Result<(), Value> {
        let kind = s(&r["manifest"], "kind");
        match s(r, "trust") {
            "blocked" => Err(json!({ "code": "engine.kind-blocked", "message": format!("the node kind `{kind}` is blocked") })),
            "reviewed" => Ok(()),
            _ if s(r, "author") == user => Ok(()),
            _ => Err(json!({ "code": "engine.kind-untrusted", "message": format!("the node kind `{kind}` is not reviewed yet: only its author ({}) may run it", s(r, "author")) })),
        }
    }

    /// The engine for a document of this person: the built-ins plus every custom kind it uses (and those use), each
    /// the newest stored version of the major its nodes pin, if they may run it and its hash still matches.
    pub fn engine_for(&self, user: &str, d: &Document) -> (Engine, Vec<Value>) {
        let mut eng = self.engine.clone();
        let mut problems = vec![];
        let records = self.kind_records(&self.dir.scopes_of(user));
        if records.is_empty() { return (eng, problems); }
        let mut todo: Vec<(String, u32)> = d.nodes.iter().chain(d.subgraphs.iter().flat_map(|m| m.values().flat_map(|g| g.nodes.iter()))).map(|n| (n.kind.clone(), n.kind_version.unwrap_or(1))).collect();
        let mut done: Vec<String> = vec![];
        while let Some((kind, major)) = todo.pop() {
            if done.contains(&kind) || self.engine.registry().manifest(&kind).is_some() { continue; }
            done.push(kind.clone());
            let Some(r) = records.iter().find(|r| s(&r["manifest"], "kind") == kind && semver(s(&r["manifest"], "version")).0 == major) else { continue };
            if let Err(p) = self.trust_ok(r, user) { problems.push(p); continue; }
            let Ok(m) = serde_json::from_value::<KindManifest>(r["manifest"].clone()) else { continue };
            match eng.install(m.clone(), r["hash"].as_str()) {
                Ok(()) => { if let Some(g) = eng.custom_graph(&kind) { for n in g["nodes"].as_array().into_iter().flatten() { todo.push((s(n, "kind").to_string(), n["kindVersion"].as_u64().unwrap_or(1) as u32)); } } }
                Err(e) => problems.push(json!({ "code": if e[0].starts_with("engine.kind-hash") { "engine.kind-hash" } else { "engine.kind" }, "message": e.join("; ") })),
            }
        }
        (eng, problems)
    }

    /// Publishes a version of a custom kind into a scope (spec 11.4): the manifest is checked, its version must be newer
    /// than the scope's, its tests run (and must pass); it starts `author-only` until it is reviewed.
    async fn kind_publish(&self, c: &Caller, mv: &Value, scope: Option<&str>) -> Answer {
        let Ok(m) = serde_json::from_value::<KindManifest>(mv.clone()) else { return bad(400, "`manifest` is not a node kind manifest.") };
        let sc = scope.map(str::to_string).unwrap_or_else(|| format!("me-{}", c.user));
        if !safe(&sc) || !(sc.strip_prefix("me-") == Some(c.user.as_str()) || (!sc.starts_with("me-") && self.dir.scopes_of(&c.user).contains(&sc))) { return bad(403, "You may not add node kinds to that scope."); }
        let mut problems = m.problems();
        if self.engine.registry().manifest(&m.kind).is_some() { problems.push(format!("`{}` is a built-in kind", m.kind)); }
        if !problems.is_empty() { return j(400, json!({ "error": "The manifest has problems.", "problems": problems })); }
        let program = crate::custom::needs_two(&m);
        if !program { if let Err(p) = Engine::with_builtins().install(m.clone(), None) { return j(400, json!({ "error": "The kind cannot be installed.", "problems": p })); } }
        let existing = self.kind_records(std::slice::from_ref(&sc));
        if let Some(r) = existing.iter().find(|r| s(&r["manifest"], "kind") == m.kind) {
            if semver(&m.version) <= semver(s(&r["manifest"], "version")) { return j(409, json!({ "error": format!("`{}` has version {} here already: publish a newer one", m.kind, s(&r["manifest"], "version")) })); }
        }
        if !program {
            let others: Vec<KindManifest> = self.kind_records(&self.dir.scopes_of(&c.user)).into_iter().filter_map(|r| serde_json::from_value(r["manifest"].clone()).ok()).collect();
            let failures = crate::custom::run_tests(&m, &others).await;
            if !failures.is_empty() { return j(422, json!({ "error": "Its tests fail.", "failures": failures })); }
        }
        let h = crate::custom::hash(&m);
        let rec = json!({ "manifest": m, "hash": h, "author": c.user, "at": now_ms(), "trust": "author-only", "reviewers": [] });
        if !write(&self.kind_dir(&sc, &m.kind).join(format!("{}.json", m.version)), &rec) { return bad(500, "Could not save the kind."); }
        self.audit(&sc, &c.user, "kind-publish", json!({ "kind": m.kind, "version": m.version, "hash": h }));
        j(200, json!({ "ok": true, "kind": m.kind, "version": m.version, "hash": h, "trust": "author-only" }))
    }

    /// A review (spec 11.4): by someone who is not the author and may publish in the scope (an admin, or named in its
    /// `lawEditors`); one makes it `reviewed`, a sandboxed program needs two.
    fn kind_review(&self, c: &Caller, kind: &str, version: &str, scope: Option<&str>) -> Answer {
        let scopes = self.dir.scopes_of(&c.user);
        let Some(mut r) = self.kind_records(&scopes).into_iter().find(|r| s(&r["manifest"], "kind") == kind && (version.is_empty() || s(&r["manifest"], "version") == version) && scope.is_none_or(|x| s(r, "scope") == x)) else { return bad(404, "No such node kind.") };
        let sc = s(&r, "scope").to_string();
        if s(&r, "author") == c.user { return bad(403, "A kind is reviewed by someone other than its author."); }
        if !self.may_shelve(c, &sc) { return bad(403, "Only the scope's admins (or its lawEditors) review its node kinds."); }
        if s(&r, "trust") == "blocked" { return bad(409, "That kind is blocked."); }
        if !r["reviewers"].as_array().is_some_and(|a| a.iter().any(|x| x.as_str() == Some(c.user.as_str()))) {
            if !r["reviewers"].is_array() { r["reviewers"] = json!([]); }
            r["reviewers"].as_array_mut().unwrap().push(json!(c.user));
        }
        let m: Option<KindManifest> = serde_json::from_value(r["manifest"].clone()).ok();
        let need = if m.as_ref().is_some_and(crate::custom::needs_two) { 2 } else { 1 };
        if r["reviewers"].as_array().map_or(0, |a| a.len()) >= need { r["trust"] = json!("reviewed"); }
        let v = s(&r["manifest"], "version").to_string();
        if let Some(o) = r.as_object_mut() { o.remove("scope"); }
        write(&self.kind_dir(&sc, kind).join(format!("{v}.json")), &r);
        self.audit(&sc, &c.user, "kind-review", json!({ "kind": kind, "version": v, "trust": r["trust"] }));
        j(200, json!({ "ok": true, "trust": r["trust"], "reviewers": r["reviewers"] }))
    }

    /// Blocks every version of a kind in a scope (an admin): it can be neither added nor run.
    fn kind_block(&self, c: &Caller, kind: &str, scope: Option<&str>) -> Answer {
        let Some(sc) = scope.filter(|x| safe(x)) else { return bad(400, "Which scope?") };
        if !(self.dir.is_admin(&c.user) && self.dir.scopes_of(&c.user).iter().any(|x| x == sc)) { return bad(403, "Only an admin blocks a node kind."); }
        let recs: Vec<Value> = self.kind_records(&[sc.to_string()]).into_iter().filter(|r| s(&r["manifest"], "kind") == kind).collect();
        if recs.is_empty() { return bad(404, "No such node kind."); }
        for mut r in recs {
            r["trust"] = json!("blocked");
            let v = s(&r["manifest"], "version").to_string();
            if let Some(o) = r.as_object_mut() { o.remove("scope"); }
            write(&self.kind_dir(sc, kind).join(format!("{v}.json")), &r);
        }
        self.audit(sc, &c.user, "kind-block", json!({ "kind": kind }));
        j(200, json!({ "ok": true }))
    }

    /// "New node from selection" (spec 11.1): the selected nodes of a document become a `graph` kind
    /// (`<user>/<name>`), published in the person's own scope; the answer has the manifest and the document rewired to
    /// use it (the editor shows it; nothing else is saved).
    async fn selection(&self, c: &Caller, docv: &Value, nodes: &Value, name: &str, title: &str) -> Answer {
        let mut dv = docv.clone();
        if dv.is_object() && dv.get("id").is_none() { dv["id"] = json!("wf-selection"); }
        let Ok(d) = doc::from_value(dv) else { return bad(400, "`doc` is not a workflow document.") };
        let ids: Vec<String> = nodes.as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        if name.is_empty() || !name.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_".contains(ch)) { return bad(400, "The new kind needs a `name` of letters, digits, - and _."); }
        let kind = format!("{}/{name}", c.user);
        let (eng, _) = self.engine_for(&c.user, &d);
        let (m, nd) = match crate::custom::from_selection(&d, eng.registry(), &ids, &kind, if title.is_empty() { name } else { title }) { Ok(x) => x, Err(e) => return bad(400, e) };
        match Box::pin(self.kind_publish(c, &serde_json::to_value(&m).unwrap_or(Value::Null), None)).await {
            Answer::Json(200, v) => j(200, json!({ "ok": true, "kind": kind, "manifest": m, "hash": v["hash"], "doc": doc::to_value(&nd) })),
            other => other,
        }
    }

    // ------------------------------------------------------------ runs

    fn run_meta(&self, run: &str) -> Option<Value> { if safe(run) { read(&self.run_dir(run).join("run.json")) } else { None } }

    fn run_meta_for(&self, c: &Caller, run: &str) -> Result<Value, Answer> {
        let Some(m) = self.run_meta(run) else { return Err(bad(404, "No such run.")) };
        let may = s(&m, "owner") == c.user || self.meta_by_id(s(&m["workflow"], "id")).is_some_and(|w| self.rights(c, &w).view);
        if !may { return Err(bad(404, "No such run.")); }
        Ok(m)
    }

    fn runs(&self, c: &Caller, workflow: Option<&str>, mine: bool, status: Option<&str>) -> Answer {
        let mut out = vec![];
        for e in std::fs::read_dir(self.root.join("runs")).into_iter().flatten().flatten() {
            let Some(m) = read(&e.path().join("run.json")) else { continue };
            if workflow.is_some_and(|w| s(&m["workflow"], "id") != w && s(&m["workflow"], "name") != w) { continue; }
            if mine && s(&m, "owner") != c.user { continue; }
            if status.is_some_and(|x| s(&m, "status") != x) { continue; }
            if s(&m, "owner") != c.user && !self.meta_by_id(s(&m["workflow"], "id")).is_some_and(|w| self.rights(c, &w).view) { continue; }
            out.push(m);
        }
        out.sort_by(|a, b| b["at"].as_u64().cmp(&a["at"].as_u64()));
        j(200, json!({ "runs": out }))
    }

    fn start_run(&self, c: &Caller, body: &Value) -> Answer {
        let Some(m) = self.find(c, body["id"].as_str(), body["name"].as_str(), body["scope"].as_str()) else { return bad(404, "No such workflow.") };
        let r = self.rights(c, &m);
        if !(r.view || r.run) { return bad(404, "No such workflow."); }
        if !r.run { return bad(403, "You may not run this workflow."); }
        let draft = body["draft"] == json!(true);
        let (version, d) = if draft {
            if !r.edit { return bad(403, "Only an editor test-runs a draft."); }
            match self.draft_doc(&m, &c.user) { Some((d, _)) => (None, d), None => return bad(404, "You have no draft of this workflow.") }
        } else {
            let range = body["version"].as_u64().map(|v| v.to_string()).or_else(|| body["version"].as_str().map(str::to_string));
            match self.resolve_range(&m, range.as_deref()) { Some((v, d)) => (Some(v), d), None => return bad(404, "That workflow has no such published version.") }
        };
        let project = body["project"].as_str().filter(|p| !p.is_empty()).map(str::to_string);
        let mut problems: Vec<Value> = self.problems_in(&d, &c.user).into_iter().filter(|p| p["level"] == "error").collect();
        if d.needs_project == Some(true) && project.is_none() { problems.push(json!({ "code": "engine.no-project", "message": "this workflow runs against a project: say which (--project)" })); }
        if !problems.is_empty() { return j(422, json!({ "error": "This workflow cannot run here.", "problems": problems })); }
        let mut input = RunInput::prompt(body["prompt"].as_str().unwrap_or("")).sender("person", &c.user);
        // a trigger starts its runs as the workflow's owner and says so (spec 9.2)
        if body["sender"].is_object() { input.sender = body["sender"].clone(); }
        input.inputs = body["inputs"].as_object().cloned().unwrap_or_default();
        input.files = body["files"].as_array().cloned().unwrap_or_default();
        input.project = project;
        let run = format!("hr-{:012x}", (now_ms() << 8 ^ self.rev.fetch_add(1, Ordering::SeqCst)) & 0xffff_ffff_ffff);
        let rm = json!({ "id": run, "owner": c.user, "workflow": { "id": m["id"], "name": m["name"], "scope": m["scope"], "version": version, "draft": draft }, "status": "running", "at": now_ms(), "runner": "hub", "doc": doc::to_value(&d), "trigger": body["trigger"] });
        write(&self.run_dir(&run).join("run.json"), &rm);
        self.drive(run.clone(), c.user.clone(), Arc::new(d), Drive::Start(input));
        j(202, json!({ "run": run, "runner": "hub" }))
    }

    /// `/hyperwork <name> [--project P] [--on runner] [--version V] <prompt>` as typed in a product (spec 10.2): parsed
    /// with the one parser, the name found in the caller's scopes (nearest first), the run started. A name that matches
    /// nothing runs nothing: 404 with the nearest names. `project` is the product's current project (the line's
    /// `--project` wins).
    fn command(&self, c: &Caller, line: &str, project: Option<&str>) -> Answer {
        let cmd = match crate::command::parse(line) {
            crate::command::Parsed::Command(cmd) => cmd,
            crate::command::Parsed::Bad(m) => return bad(400, m),
            crate::command::Parsed::Typing(_) | crate::command::Parsed::No => return bad(400, "Usage: /hyperwork <name> [--project P] [--on runner] [--version V] <prompt>"),
        };
        if cmd.on.as_deref().is_some_and(|o| !matches!(o, "hub" | "any")) { return bad(400, format!("This hub runs workflows on the hub; `--on {}` runs it there (jcode, HPM or the gateway).", cmd.on.as_deref().unwrap_or(""))); }
        if self.find(c, None, Some(&cmd.name), None).is_none() {
            let names: Vec<String> = self.metas(&self.dir.scopes_of(&c.user)).into_iter().filter(|m| { let r = self.rights(c, m); r.run || r.view }).map(|m| s(&m, "name").to_string()).collect();
            let near = crate::command::nearest(names.iter().map(String::as_str), &cmd.name, 5);
            return j(404, json!({ "error": format!("No workflow is called `{}`.", cmd.name), "nearest": near }));
        }
        let mut body = json!({ "name": cmd.name, "prompt": cmd.prompt });
        if let Some(p) = cmd.project.as_deref().or(project) { body["project"] = json!(p); }
        if let Some(v) = &cmd.version { body["version"] = json!(v); }
        self.start_run(c, &body)
    }

    fn replay(&self, c: &Caller, run: &str) -> Answer {
        let old = match self.run_meta_for(c, run) { Ok(m) => m, Err(e) => return e };
        let Ok(d) = doc::from_value(old["doc"].clone()) else { return bad(409, "That run's document is gone.") };
        let id = format!("{run}-replay-{:06x}", now_ms() & 0xff_ffff);
        let mut rm = old.clone();
        rm["id"] = json!(id);
        rm["owner"] = json!(c.user);
        rm["status"] = json!("running");
        rm["at"] = json!(now_ms());
        rm["replayOf"] = json!(run);
        write(&self.run_dir(&id).join("run.json"), &rm);
        self.drive(id.clone(), c.user.clone(), Arc::new(d), Drive::Replay(read_lines(&self.run_dir(run).join("log.ndjson"))));
        j(202, json!({ "run": id }))
    }

    async fn answer(&self, c: &Caller, run: &str, key: &str, answers: Value) -> Answer {
        let m = match self.run_meta_for(c, run) { Ok(m) => m, Err(e) => return e };
        if s(&m, "owner") != c.user && !self.dir.is_admin(&c.user) { return bad(403, "Only the person who started the run answers its questions."); }
        if s(&m, "status") != "waiting" { return bad(409, "That run is not waiting for an answer."); }
        let log = read_lines(&self.run_dir(run).join("log.ndjson"));
        let p = crate::log::Prior::from_log(log.clone());
        if !p.waits.contains_key(key) { return bad(404, "The run does not wait on that question."); }
        if p.answers.contains_key(key) { return bad(409, "That question is answered already."); }
        let Ok(d) = doc::from_value(m["doc"].clone()) else { return bad(409, "That run's document is gone.") };
        self.set_status(run, "running");
        self.drive(run.to_string(), s(&m, "owner").to_string(), Arc::new(d), Drive::Answer(log, key.to_string(), answers, if s(&m, "owner") == c.user { "user".into() } else { c.user.clone() }));
        j(200, json!({ "ok": true }))
    }

    fn log(&self, c: &Caller, run: &str, from: u64, follow: bool) -> Answer {
        if let Err(e) = self.run_meta_for(c, run) { return e; }
        let (tx, rx) = mpsc::channel::<String>(1024);
        let past = read_lines(&self.run_dir(run).join("log.ndjson"));
        let live = if follow { lock(&self.runs).get(run).map(|l| l.tx.subscribe()) } else { None };
        let last = past.iter().filter_map(|e| e["seq"].as_u64()).max().unwrap_or(0);
        let ended = past.iter().any(|e| e["type"] == "run_end");
        tokio::spawn(async move {
            for e in past.iter().filter(|e| e["seq"].as_u64().unwrap_or(0) >= from) { if tx.send(format!("{e}\n")).await.is_err() { return; } }
            if ended { return; }
            let Some(mut live) = live else { return };
            loop {
                match live.recv().await {
                    Ok(e) => {
                        if e["seq"].as_u64().unwrap_or(0) <= last || e["seq"].as_u64().unwrap_or(0) < from { continue; }
                        let end = e["type"] == "run_end" || e["type"] == "snapshot";
                        if tx.send(format!("{e}\n")).await.is_err() || end { return; }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                }
            }
        });
        Answer::Lines(rx)
    }

    fn set_status(&self, run: &str, status: &str) {
        if let Some(mut m) = self.run_meta(run) { m["status"] = json!(status); m["updatedAt"] = json!(now_ms()); write(&self.run_dir(run).join("run.json"), &m); }
    }

    fn drive(&self, run: String, owner: String, d: Arc<Document>, how: Drive) {
        let Some(me) = self.me.upgrade() else { return };
        let (tx, _) = broadcast::channel(1024);
        let live = Arc::new(Live { cancel: Cancel::new(), tx, secrets: Arc::new(Mutex::new(vec![])) });
        lock(&self.runs).insert(run.clone(), live.clone());
        let scope = self.run_meta(&run).map(|m| s(&m["workflow"], "scope").to_string()).unwrap_or_default();
        let host: Arc<dyn WorkflowHost> = Arc::new(RunHost { inner: self.dir.host_for(&owner), svc: self.me.clone(), run: run.clone(), owner: owner.clone(), scope, live: live.clone() });
        self.dir.changed(&owner, &json!({ "type": "hyper_run", "run": run, "status": "running" }));
        // the engine with the custom kinds this person may run (a problem with one shows as the run's preflight)
        let (e, _) = self.engine_for(&owner, &d);
        tokio::spawn(async move {
            let opts = RunOptions { run_id: Some(run.clone()), runner: Some("hub".into()), cancel: live.cancel.clone(), ..Default::default() };
            let rep: RunReport = match how {
                Drive::Start(input) => e.start(d, input, host, opts).await,
                Drive::Resume(log) => e.resume(d, log, host, opts).await,
                Drive::Answer(log, key, answers, by) => e.answer(d, log, &key, answers, &by, host, opts).await,
                Drive::Replay(log) => e.replay(d, log, host, opts).await,
            };
            me.finished(&run, &owner, &rep);
        });
    }

    fn finished(&self, run: &str, owner: &str, rep: &RunReport) {
        lock(&self.runs).remove(run);
        if let Some(mut m) = self.run_meta(run) {
            m["status"] = json!(rep.status);
            m["outcome"] = json!(rep.outcome);
            m["result"] = rep.result.clone().unwrap_or(Value::Null);
            m["error"] = rep.error.clone().unwrap_or(Value::Null);
            m["waiting"] = json!(rep.waiting);
            m["totals"] = rep.totals.clone();
            m["problems"] = json!(rep.problems);
            m["updatedAt"] = json!(now_ms());
            write(&self.run_dir(run).join("run.json"), &m);
        }
        self.dir.changed(owner, &json!({ "type": "hyper_run", "run": run, "status": rep.status, "outcome": rep.outcome, "waiting": rep.waiting }));
        self.trigger_finished(run, owner, rep);
    }

    /// After a start (the hub restarted): every run that was running goes on from its log; a waiting run whose
    /// deadline has passed is driven so its timeout applies. Call it once at start and then now and then (`tick`).
    pub fn resume_all(&self, include_running: bool) {
        for e in std::fs::read_dir(self.root.join("runs")).into_iter().flatten().flatten() {
            let Some(m) = read(&e.path().join("run.json")) else { continue };
            let run = s(&m, "id").to_string();
            if lock(&self.runs).contains_key(&run) { continue; }
            let due = s(&m, "status") == "waiting" && m["waiting"].as_array().is_some_and(|w| w.iter().any(|x| x["deadline"].as_u64().is_some_and(|d| d <= now_ms())));
            if !(include_running && s(&m, "status") == "running" || due) { continue; }
            let Ok(d) = doc::from_value(m["doc"].clone()) else { continue };
            let log = read_lines(&e.path().join("log.ndjson"));
            if log.is_empty() { self.set_status(&run, "failed"); continue; }
            self.drive(run, s(&m, "owner").to_string(), Arc::new(d), Drive::Resume(log));
        }
    }

    /// The deadlines of waiting runs (call every few seconds).
    /// Every few seconds: the deadlines of waiting runs, and the timers that are due (T-211).
    pub fn tick(&self) { self.resume_all(false); self.tick_at(now_ms()); }

    /// The published workflow a `sub-workflow` node of a run in `scope`, owned by `owner`, may run.
    fn workflow_for(&self, owner: &str, scope: &str, name: &str, range: Option<&str>) -> Option<Document> {
        let c = Caller { user: owner.to_string(), run_only: false };
        let m = self.find(&c, None, Some(name), None).or_else(|| self.metas(&[scope.to_string()]).into_iter().find(|m| s(m, "name") == name))?;
        if !self.rights(&c, &m).run { return None; }
        self.resolve_range(&m, range).map(|x| x.1)
    }

    fn write_event(&self, run: &str, live: &Live, ev: &Value) {
        let mut text = ev.to_string();
        let mut secrets = self.dir.secret_values();
        secrets.extend(lock(&live.secrets).iter().cloned());
        for sec in secrets.iter().filter(|x| x.len() >= 4) {
            let quoted = serde_json::to_string(sec).unwrap_or_default();
            let inner = quoted.trim_matches('"');
            text = text.replace(inner, "***");
        }
        append_line(&self.run_dir(run).join("log.ndjson"), &text);
        let clean: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let _ = live.tx.send(clean);
    }
}

/// The three kinds of item next to the workflows (spec 6.3).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Shelf { Laws, Prompts, Groups }

impl Shelf {
    fn of(path: &str) -> Shelf {
        match path.split('/').next().unwrap_or("") { "law" | "laws" => Shelf::Laws, "prompt" | "prompts" => Shelf::Prompts, _ => Shelf::Groups }
    }
    fn folder(self) -> &'static str { match self { Shelf::Laws => "laws", Shelf::Prompts => "prompts", Shelf::Groups => "groups" } }
    fn one(self) -> &'static str { match self { Shelf::Laws => "law", Shelf::Prompts => "prompt", Shelf::Groups => "group" } }
    fn prefix(self) -> &'static str { match self { Shelf::Laws => "law-", Shelf::Prompts => "pr-", Shelf::Groups => "grp-" } }
    fn problems(self, v: &Value) -> Vec<String> {
        use pgm_workflow_internal::shapes::{Group, Law, Prompt};
        let shape = |e: serde_json::Error| vec![format!("not a {}: {e}", self.one())];
        match self {
            Shelf::Laws => serde_json::from_value::<Law>(v.clone()).map(|x| x.problems()).unwrap_or_else(shape),
            Shelf::Prompts => serde_json::from_value::<Prompt>(v.clone()).map(|x| x.problems()).unwrap_or_else(shape),
            Shelf::Groups => serde_json::from_value::<Group>(v.clone()).map(|x| x.problems()).unwrap_or_else(shape),
        }
    }
}

/// What validation asks about laws and prompts, answered from the store as a person sees it.
struct StoreScope<'a> { svc: &'a Service, owner: &'a str }

impl pgm_workflow_internal::ScopeInfo for StoreScope<'_> {
    fn law_visible(&self, id: &str) -> bool {
        let id = id.trim_start_matches('-');
        let scopes = self.svc.dir.scopes_of(self.owner);
        self.svc.shelf_items(&scopes, Shelf::Laws).iter().chain(self.svc.shelf_items(&scopes, Shelf::Groups).iter()).any(|x| s(x, "id") == id)
    }
    fn prompt_visible(&self, id: &str) -> bool { self.svc.saved_prompt_for(self.owner, id).is_some() }
}

enum Drive {
    Start(RunInput),
    Resume(Vec<Value>),
    Answer(Vec<Value>, String, Value, String),
    Replay(Vec<Value>),
}

/// The host a run gets: the hub's, with the service's own parts around it (the log, snapshots, the store, secrets
/// remembered for scrubbing).
struct RunHost {
    inner: Arc<dyn WorkflowHost>,
    svc: Weak<Service>,
    run: String,
    owner: String,
    scope: String,
    live: Arc<Live>,
}

impl WorkflowHost for RunHost {
    fn call_model<'a>(&'a self, req: ModelRequest, cancel: Cancel) -> BoxFut<'a, Result<ModelAnswer, NodeError>> { self.inner.call_model(req, cancel) }
    fn call_tool<'a>(&'a self, name: String, args: Value, ctx: ToolCtx, cancel: Cancel) -> BoxFut<'a, Result<Value, NodeError>> {
        // the scope's policy names the tools its runs may use (spec 9.3), else the hub's default list: a workflow cannot
        // grant itself a tool
        let allowed = match self.svc.upgrade() {
            Some(svc) => match svc.scope_policy(&self.scope).get("tools").and_then(Value::as_array) {
                Some(a) => a.iter().any(|t| t.as_str() == Some(name.as_str())),
                None => svc.dir.default_tools().contains(&name),
            },
            None => false,
        };
        if !allowed { return Box::pin(async move { Err(NodeError::fatal("engine.tool-denied", format!("the tool `{name}` is not allowed in this workflow's scope"))) }); }
        self.inner.call_tool(name, args, ctx, cancel)
    }
    fn http<'a>(&'a self, req: HttpRequest, cancel: Cancel) -> BoxFut<'a, Result<HttpAnswer, NodeError>> {
        for (_, v) in &req.headers { if v.len() >= 4 { lock(&self.live.secrets).push(v.clone()); } }
        self.inner.http(req, cancel)
    }
    fn run_agent<'a>(&'a self, spec: Value, cancel: Cancel) -> BoxFut<'a, Result<Value, NodeError>> { self.inner.run_agent(spec, cancel) }
    fn ask_person(&self, ask: &Ask) -> Result<String, NodeError> {
        let r = self.inner.ask_person(ask).unwrap_or_else(|_| format!("{}:{}", ask.run, ask.key));
        if let Some(svc) = self.svc.upgrade() { svc.dir.changed(&self.owner, &json!({ "type": "hyper_question", "run": self.run, "key": ask.key, "node": ask.node, "what": ask.what, "questions": ask.questions, "deadline": ask.deadline, "ref": r })); }
        Ok(r)
    }
    fn emit(&self, run: &str, key: &str, message: &str) {
        self.inner.emit(run, key, message);
        if let Some(svc) = self.svc.upgrade() { svc.dir.changed(&self.owner, &json!({ "type": "hyper_emit", "run": run, "key": key, "message": message })); }
    }
    fn saved_prompt(&self, name: &str) -> Option<String> { self.svc.upgrade().and_then(|s| s.saved_prompt_for(&self.owner, name)).or_else(|| self.inner.saved_prompt(name)) }
    fn law_book(&self) -> Option<crate::host::LawBook> { Some(self.svc.upgrade()?.law_book_for(&self.owner, &self.scope)) }
    fn decision_model(&self) -> Option<Value> { self.svc.upgrade()?.scope_policy(&self.scope).get("decisionModel").cloned().filter(Value::is_object).or_else(|| self.inner.decision_model()) }
    fn workflow(&self, name: &str, range: Option<&str>) -> Option<Document> { self.svc.upgrade()?.workflow_for(&self.owner, &self.scope, name, range) }
    fn project(&self, name: &str) -> Option<Value> { self.inner.project(name) }
    fn secret(&self, name: &str) -> Option<String> {
        let v = self.inner.secret(name)?;
        lock(&self.live.secrets).push(v.clone());
        Some(v)
    }
    fn log(&self, event: &Value) {
        // a sub-workflow's events are its own run's: each run id has its own log
        let run = event.get("run").and_then(Value::as_str).unwrap_or(&self.run).to_string();
        if let Some(svc) = self.svc.upgrade() {
            if run != self.run && svc.run_meta(&run).is_none() {
                let parent = svc.run_meta(&self.run).unwrap_or(Value::Null);
                write(&svc.run_dir(&run).join("run.json"), &json!({ "id": run, "owner": self.owner, "workflow": { "name": event["workflow"]["name"], "version": event["workflow"]["version"], "scope": self.scope }, "status": "child", "parent": self.run, "at": now_ms(), "doc": Value::Null, "parentWorkflow": parent["workflow"] }));
            }
            svc.write_event(&run, &self.live, event);
        }
        self.inner.log(event);
    }
    fn save_snapshot(&self, run: &str, snapshot: &Value) {
        if let Some(svc) = self.svc.upgrade() { write(&svc.run_dir(run).join("snapshot.json"), snapshot); }
    }
    fn now_ms(&self) -> u64 { self.inner.now_ms() }
}
