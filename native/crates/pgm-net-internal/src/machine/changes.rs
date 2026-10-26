//! Scope changes (spec 5.6, R333): who may change the scope file, and how.
//!
//! - **Widen only on the machine.** A proposal from the web that adds a folder, raises `ro` to `rw`, picks a wider
//!   preset, switches a tool on or asks for a grant is **stored as pending** (`<state>/scope-pending.json`) and
//!   changes nothing (the scope file stays byte-identical) until [`ScopeStore::approve`] with a
//!   [`LocalApproval`] (the machine's screen or terminal). A pending proposal older than a day cannot be approved.
//! - **Narrow from the web at once.** Removing a folder, `rw` to `ro`, a narrower preset, a tool off, a grant
//!   revoked: [`ScopeStore::from_web`] applies it immediately and writes an audit line with the old and new scope
//!   hash and the approver `hub-narrowing`. A preset that cannot be ranked (`custom`) counts as widening.
//! - **Grants** (`grants[]`) are time-limited: [`ScopeStore::expire_grants`] (call it every few seconds and before
//!   a long call) drops the expired ones from the file, audits it and calls back for each so the jobs running on
//!   that path are stopped. The evaluator itself already refuses an expired grant on the clock it is given, so a
//!   late sweep never leaves access open. A "this run" grant carries the run id and [`ScopeStore::end_run`] ends it.
//! - **No tool path.** There is no function here (or anywhere) that takes a tool call and changes the scope; the
//!   scope file, the pending files, `KILL`, the audit files and the elevation queue are safety-floor paths
//!   ([`state_floor`]), so no tool can read or write them under any preset, `unrestricted` included. The tests
//!   prove it for every op and through a symlink.
//!
//! The wire for the hub (the routes that carry a proposal to the machine) is the hub routes' task; the proposal is
//! plain JSON ([`Proposal::from_json`]).

use super::audit::{AuditLog, Event, Verdict};
use super::elevate::QUEUE_FILE;
use super::floor::Floor;
use super::kill::{LocalApproval, KILL_FILE};
use super::scope::{parse_iso8601, Access, Preset, Scope};
use pgm_core_internal::crypto::sha256_hex;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const PENDING_FILE: &str = "scope-pending.json";
/// A pending proposal can be approved for a day.
pub const PENDING_TTL_SECS: i64 = 86_400;

/// The machine's floor: the four paths it was built with plus every state file the tools must never touch:
/// `KILL`, the elevation queue, the pending-change file (rotated and sidecar files of the audit log are covered by
/// the floor's own "`name.` + anything" rule).
pub fn state_floor(floor: Floor, state_dir: &Path) -> Floor {
    floor.with_extra(state_dir.join(KILL_FILE)).with_extra(state_dir.join(QUEUE_FILE)).with_extra(state_dir.join(PENDING_FILE))
}

// ------------------------------------------------------------------ proposals

#[derive(Clone, Debug, PartialEq)]
pub enum Proposal {
    AddPath { path: String, access: Access },
    RemovePath { path: String },
    /// `path: None` means every folder.
    SetAccess { path: Option<String>, access: Access },
    SetPreset(Preset),
    /// `tool`: `files`, `shell`, `processes`, `fetch`, `elevate`, `containers`.
    SetTool { tool: String, enabled: bool },
    AddGrant { path: String, access: Access, expires_at: Option<i64>, run: Option<String>, note: String },
    RevokeGrant { id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Narrow,
    Widen,
    /// It would change nothing.
    Same,
}

fn access_of(v: Option<&Value>) -> Result<Access, String> {
    match v.and_then(|x| x.as_str()) {
        Some("ro") => Ok(Access::Ro),
        Some("rw") => Ok(Access::Rw),
        _ => Err("access must be \"ro\" or \"rw\"".into()),
    }
}

fn text(v: &Value, k: &str) -> Result<String, String> {
    v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string()).ok_or_else(|| format!("{k} is needed"))
}

const TOOLS: [&str; 6] = ["files", "shell", "processes", "fetch", "elevate", "containers"];

impl Proposal {
    /// `{"kind": "add-path", "path": "...", "access": "ro"}` and the like. Unknown kinds are an error, never "ignore".
    pub fn from_json(v: &Value) -> Result<Proposal, String> {
        Ok(match v.get("kind").and_then(|k| k.as_str()).ok_or("kind is needed")? {
            "add-path" => Proposal::AddPath { path: text(v, "path")?, access: access_of(v.get("access"))? },
            "remove-path" => Proposal::RemovePath { path: text(v, "path")? },
            "set-access" => Proposal::SetAccess { path: v.get("path").and_then(|x| x.as_str()).map(|s| s.to_string()), access: access_of(v.get("access"))? },
            "set-preset" => Proposal::SetPreset(Preset::parse(&text(v, "preset")?).ok_or("unknown preset")?),
            "set-tool" => {
                let tool = text(v, "tool")?;
                if !TOOLS.contains(&tool.as_str()) {
                    return Err(format!("unknown tool {tool}"));
                }
                Proposal::SetTool { tool, enabled: v.get("enabled").and_then(|x| x.as_bool()).ok_or("enabled is needed")? }
            }
            "add-grant" => Proposal::AddGrant {
                path: text(v, "path")?,
                access: access_of(v.get("access"))?,
                expires_at: v.get("expiresAt").and_then(|x| x.as_i64()),
                run: v.get("run").and_then(|x| x.as_str()).map(|s| s.to_string()),
                note: v.get("note").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            },
            "revoke-grant" => Proposal::RevokeGrant { id: text(v, "id")? },
            k => return Err(format!("unknown kind {k}")),
        })
    }

    pub fn to_json(&self) -> Value {
        match self {
            Proposal::AddPath { path, access } => json!({"kind": "add-path", "path": path, "access": access.as_str()}),
            Proposal::RemovePath { path } => json!({"kind": "remove-path", "path": path}),
            Proposal::SetAccess { path, access } => json!({"kind": "set-access", "path": path, "access": access.as_str()}),
            Proposal::SetPreset(p) => json!({"kind": "set-preset", "preset": p.as_str()}),
            Proposal::SetTool { tool, enabled } => json!({"kind": "set-tool", "tool": tool, "enabled": enabled}),
            Proposal::AddGrant { path, access, expires_at, run, note } => json!({"kind": "add-grant", "path": path, "access": access.as_str(), "expiresAt": expires_at, "run": run, "note": note}),
            Proposal::RevokeGrant { id } => json!({"kind": "revoke-grant", "id": id}),
        }
    }

    /// For the prompt on the machine and the audit line.
    pub fn describe(&self) -> String {
        match self {
            Proposal::AddPath { path, access } => format!("add {path} ({})", access.as_str()),
            Proposal::RemovePath { path } => format!("remove {path}"),
            Proposal::SetAccess { path, access } => format!("make {} {}", path.as_deref().unwrap_or("every folder"), access.as_str()),
            Proposal::SetPreset(p) => format!("preset {}", p.as_str()),
            Proposal::SetTool { tool, enabled } => format!("tool {tool} {}", if *enabled { "on" } else { "off" }),
            Proposal::AddGrant { path, access, expires_at, .. } => format!("grant {path} ({}) {}", access.as_str(), expires_at.map_or("until removed".to_string(), |t| format!("until {}", iso8601(t)))),
            Proposal::RevokeGrant { id } => format!("revoke grant {id}"),
        }
    }
}

/// How wide a preset reaches; `None` for `custom` (cannot be compared, so never a narrowing).
fn rank(p: Preset) -> Option<u8> {
    Some(match p {
        Preset::ReadOnly => 1,
        Preset::Folder => 2,
        Preset::Folders => 3,
        Preset::FullRo => 4,
        Preset::NoShell | Preset::NoNetwork => 5,
        Preset::DenyList | Preset::Full => 6,
        Preset::Unrestricted => 7,
        Preset::Custom => return None,
    })
}

fn same_path(a: &str, b: &str) -> bool {
    let t = |s: &str| {
        let s = s.trim_end_matches(['/', '\\']);
        if cfg!(windows) { s.to_lowercase() } else { s.to_string() }
    };
    t(a) == t(b)
}

fn paths_mut(doc: &mut Value) -> &mut Vec<Value> {
    if !doc.get("paths").is_some_and(|p| p.is_array()) {
        doc["paths"] = json!([]);
    }
    doc["paths"].as_array_mut().unwrap()
}

fn doc_preset(doc: &Value) -> Preset {
    doc.get("preset").and_then(|p| p.as_str()).and_then(Preset::parse).unwrap_or(Preset::Folder)
}

fn tool_on(doc: &Value, tool: &str) -> bool {
    let b = |p: &str, d: bool| doc.pointer(p).and_then(|x| x.as_bool()).unwrap_or(d);
    match tool {
        "files" => b("/tools/files", true),
        "shell" => b("/tools/shell/enabled", true),
        "processes" => b("/tools/processes/list", true) || b("/tools/processes/start", true),
        "fetch" => b("/tools/network/fetch", false),
        "elevate" => b("/tools/elevate", false),
        "containers" => b("/tools/containers/re", false),
        _ => false,
    }
}

fn set_tool(doc: &mut Value, tool: &str, on: bool) {
    fn obj<'a>(doc: &'a mut Value, k: &str) -> &'a mut Map<String, Value> {
        if !doc.get(k).is_some_and(|t| t.is_object()) {
            doc[k] = json!({});
        }
        doc[k].as_object_mut().unwrap()
    }
    fn sub<'a>(tools: &'a mut Map<String, Value>, name: &str) -> &'a mut Map<String, Value> {
        let e = tools.entry(name.to_string()).or_insert_with(|| json!({}));
        if !e.is_object() {
            *e = json!({});
        }
        e.as_object_mut().unwrap()
    }
    let tools = obj(doc, "tools");
    match tool {
        "files" => {
            tools.insert("files".into(), json!(on));
        }
        "elevate" => {
            tools.insert("elevate".into(), json!(on));
        }
        "shell" => {
            sub(tools, "shell").insert("enabled".into(), json!(on));
        }
        "fetch" => {
            sub(tools, "network").insert("fetch".into(), json!(on));
        }
        "containers" => {
            sub(tools, "containers").insert("re".into(), json!(on));
        }
        "processes" => {
            let m = sub(tools, "processes");
            m.insert("list".into(), json!(on));
            m.insert("start".into(), json!(on));
            if !on {
                m.insert("kill".into(), json!("none"));
            }
        }
        _ => {}
    }
}

/// Is this a widening or a narrowing against the current document?
pub fn classify(doc: &Value, p: &Proposal) -> Kind {
    match p {
        Proposal::AddPath { .. } | Proposal::AddGrant { .. } => Kind::Widen,
        Proposal::RemovePath { .. } | Proposal::RevokeGrant { .. } => Kind::Narrow,
        Proposal::SetAccess { path, access } => {
            let entries: Vec<&Value> = doc.get("paths").and_then(|a| a.as_array()).map(|a| a.iter().filter(|e| path.as_ref().is_none_or(|p| e["path"].as_str().is_some_and(|x| same_path(x, p)))).collect()).unwrap_or_default();
            if entries.is_empty() {
                // nothing to narrow; asking to make an unknown folder rw is a new folder
                return if *access == Access::Rw { Kind::Widen } else { Kind::Same };
            }
            let any_rw = entries.iter().any(|e| e["access"].as_str() == Some("rw"));
            let any_ro = entries.iter().any(|e| e["access"].as_str() != Some("rw"));
            match access {
                Access::Rw if any_ro => Kind::Widen,
                Access::Ro if any_rw => Kind::Narrow,
                _ => Kind::Same,
            }
        }
        Proposal::SetPreset(new) => {
            let old = doc_preset(doc);
            if *new == old {
                return Kind::Same;
            }
            match (rank(*new), rank(old)) {
                (Some(n), Some(o)) if n < o => Kind::Narrow,
                _ => Kind::Widen,
            }
        }
        Proposal::SetTool { tool, enabled } => {
            let now = tool_on(doc, tool);
            if now == *enabled {
                Kind::Same
            } else if *enabled {
                Kind::Widen
            } else {
                Kind::Narrow
            }
        }
    }
}

fn apply(doc: &mut Value, p: &Proposal, now: i64, grant_id: &str) -> Result<(), String> {
    match p {
        Proposal::AddPath { path, access } => {
            // the `folder` preset keeps one folder only: a second one means `folders`
            if doc_preset(doc) == Preset::Folder && !paths_mut(doc).is_empty() {
                doc["preset"] = json!("folders");
            }
            let arr = paths_mut(doc);
            match arr.iter_mut().find(|e| e["path"].as_str().is_some_and(|x| same_path(x, path))) {
                Some(e) => e["access"] = json!(access.as_str()),
                None => arr.push(json!({"path": path, "access": access.as_str()})),
            }
        }
        Proposal::RemovePath { path } => {
            let arr = paths_mut(doc);
            let n = arr.len();
            arr.retain(|e| !e["path"].as_str().is_some_and(|x| same_path(x, path)));
            if arr.len() == n {
                return Err(format!("{path} is not in the scope"));
            }
        }
        Proposal::SetAccess { path, access } => {
            for e in paths_mut(doc).iter_mut() {
                if path.as_ref().is_none_or(|p| e["path"].as_str().is_some_and(|x| same_path(x, p))) {
                    e["access"] = json!(access.as_str());
                }
            }
        }
        Proposal::SetPreset(p) => doc["preset"] = json!(p.as_str()),
        Proposal::SetTool { tool, enabled } => set_tool(doc, tool, *enabled),
        Proposal::AddGrant { path, access, expires_at, run, note } => {
            if !doc.get("grants").is_some_and(|g| g.is_array()) {
                doc["grants"] = json!([]);
            }
            let mut g = json!({"id": grant_id, "what": {"path": path, "access": access.as_str()}, "note": note, "grantedAt": now});
            if let Some(t) = expires_at {
                g["expiresAt"] = json!(t);
            }
            if let Some(r) = run {
                g["run"] = json!(r);
            }
            doc["grants"].as_array_mut().unwrap().push(g);
        }
        Proposal::RevokeGrant { id } => {
            let arr = doc.get_mut("grants").and_then(|g| g.as_array_mut()).ok_or("no grants")?;
            let n = arr.len();
            arr.retain(|g| g["id"].as_str() != Some(id));
            if arr.len() == n {
                return Err(format!("no grant {id}"));
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ the store

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied { revision: u64, old_hash: String, new_hash: String },
    /// Stored, nothing changed; the person must approve on the machine.
    Pending { id: String },
    NoChange,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingChange {
    pub id: String,
    pub t: i64,
    pub account: String,
    pub proposal: Proposal,
}

/// A grant that fell away: stop the jobs running on `path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpiredGrant {
    pub id: String,
    pub path: String,
    pub access: String,
    pub run: Option<String>,
}

pub struct ScopeStore<'a> {
    scope_file: PathBuf,
    pending_file: PathBuf,
    audit: Option<&'a AuditLog>,
    lock: Mutex<()>,
}

pub fn iso8601(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn hash_file(p: &Path) -> String {
    std::fs::read(p).map(|b| sha256_hex(&b)).unwrap_or_else(|_| "none".into())
}

fn short(h: &str) -> &str {
    &h[..h.len().min(12)]
}

fn expiry_of(g: &Value) -> Option<i64> {
    match g.get("expiresAt") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => Some(n.as_i64().unwrap_or(i64::MIN)),
        Some(Value::String(s)) => Some(parse_iso8601(s).unwrap_or(i64::MIN)),
        Some(_) => Some(i64::MIN),
    }
}

impl<'a> ScopeStore<'a> {
    pub fn open(scope_file: impl AsRef<Path>, state_dir: impl AsRef<Path>, audit: Option<&'a AuditLog>) -> ScopeStore<'a> {
        ScopeStore { scope_file: scope_file.as_ref().to_path_buf(), pending_file: state_dir.as_ref().join(PENDING_FILE), audit, lock: Mutex::new(()) }
    }

    pub fn scope_hash(&self) -> String {
        hash_file(&self.scope_file)
    }

    fn log(&self, now: i64, actor: &str, op: &str, what: &str, v: Verdict, why: &str) {
        if let Some(a) = self.audit {
            let _ = a.append(Event::new(now, actor, op, what, v, why));
        }
    }

    fn read_doc(&self) -> Result<Value, String> {
        let t = std::fs::read_to_string(&self.scope_file).map_err(|e| format!("scope file: {e}"))?;
        let v: Value = serde_json::from_str(&t).map_err(|e| format!("scope file: {e}"))?;
        if !v.is_object() {
            return Err("scope file is not an object".into());
        }
        Ok(v)
    }

    fn write_doc(&self, mut doc: Value, now: i64) -> Result<u64, String> {
        // refuse to write a scope that would not parse: the machine must never lock itself out by a bad edit
        let rev = doc.get("revision").and_then(|r| r.as_u64()).unwrap_or(0) + 1;
        doc["revision"] = json!(rev);
        doc["updatedAt"] = json!(iso8601(now));
        let s = doc.to_string();
        Scope::try_parse(&s).map_err(|e| format!("the change would make the scope invalid: {e}"))?;
        atomic_write(&self.scope_file, s.as_bytes())?;
        Ok(rev)
    }

    fn do_apply(&self, p: &Proposal, now: i64, actor: &str, op: &str, approver: &str) -> Result<Outcome, String> {
        let mut doc = self.read_doc()?;
        let old = self.scope_hash();
        let gid = format!("g{now}-{}", doc.get("grants").and_then(|g| g.as_array()).map_or(0, |g| g.len()) + 1);
        apply(&mut doc, p, now, &gid)?;
        let rev = self.write_doc(doc, now)?;
        let new = self.scope_hash();
        self.log(now, actor, op, &p.describe(), Verdict::Allow, &format!("scope {} -> {} (revision {rev}), approved by {approver}", short(&old), short(&new)));
        Ok(Outcome::Applied { revision: rev, old_hash: old, new_hash: new })
    }

    /// A proposal that arrived from the web. Narrowing applies now (audited as `hub-narrowing`); anything that
    /// widens is stored as pending and **changes nothing**.
    pub fn from_web(&self, account: &str, p: Proposal, now: i64) -> Result<Outcome, String> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let doc = self.read_doc()?;
        match classify(&doc, &p) {
            Kind::Same => Ok(Outcome::NoChange),
            Kind::Narrow => self.do_apply(&p, now, account, "scope.narrow", "hub-narrowing"),
            Kind::Widen => {
                let mut items = self.load_pending();
                let id = format!("p{now}-{}", items.len() + 1);
                items.push(PendingChange { id: id.clone(), t: now, account: account.into(), proposal: p.clone() });
                self.save_pending(&items)?;
                self.log(now, account, "scope.propose", &p.describe(), Verdict::Prompt, &format!("widening from the web, waiting for approval on the machine ({id})"));
                Ok(Outcome::Pending { id })
            }
        }
    }

    pub fn pending(&self) -> Vec<PendingChange> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.load_pending()
    }

    /// The person approves a pending widening, on the machine.
    pub fn approve(&self, id: &str, by: &LocalApproval, now: i64) -> Result<Outcome, String> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut items = self.load_pending();
        let i = items.iter().position(|p| p.id == id).ok_or_else(|| format!("no pending change {id}"))?;
        let item = items.remove(i);
        self.save_pending(&items)?;
        if now - item.t > PENDING_TTL_SECS {
            self.log(now, by.by(), "scope.expired", &item.proposal.describe(), Verdict::Deny, &format!("proposal {id} was too old to approve"));
            return Err("that proposal is too old; ask again".into());
        }
        self.do_apply(&item.proposal, now, &item.account, "scope.widen", by.by())
    }

    pub fn reject(&self, id: &str, by: &LocalApproval, now: i64) -> Result<(), String> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut items = self.load_pending();
        let i = items.iter().position(|p| p.id == id).ok_or_else(|| format!("no pending change {id}"))?;
        let item = items.remove(i);
        self.save_pending(&items)?;
        self.log(now, by.by(), "scope.reject", &item.proposal.describe(), Verdict::Deny, &format!("proposal {id} rejected on the machine"));
        Ok(())
    }

    /// The machine's own screen / terminal: any change at once (widening included), named in the audit line.
    pub fn apply_local(&self, p: Proposal, by: &LocalApproval, now: i64) -> Result<Outcome, String> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let doc = self.read_doc()?;
        if classify(&doc, &p) == Kind::Same {
            return Ok(Outcome::NoChange);
        }
        let op = if classify(&doc, &p) == Kind::Narrow { "scope.narrow" } else { "scope.widen" };
        self.do_apply(&p, now, by.by(), op, by.by())
    }

    /// Drop the grants whose time has passed; `stop_jobs` is called for each so the jobs on it are told to stop.
    pub fn expire_grants(&self, now: i64, stop_jobs: &mut dyn FnMut(&ExpiredGrant)) -> Result<Vec<ExpiredGrant>, String> {
        self.drop_grants(now, "expired", &|g| expiry_of(g).is_some_and(|t| t <= now), stop_jobs)
    }

    /// A "this run" grant ends with its run.
    pub fn end_run(&self, run: &str, now: i64, stop_jobs: &mut dyn FnMut(&ExpiredGrant)) -> Result<Vec<ExpiredGrant>, String> {
        self.drop_grants(now, "the run ended", &|g| g.get("run").and_then(|r| r.as_str()) == Some(run), stop_jobs)
    }

    fn drop_grants(&self, now: i64, why: &str, gone: &dyn Fn(&Value) -> bool, stop_jobs: &mut dyn FnMut(&ExpiredGrant)) -> Result<Vec<ExpiredGrant>, String> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut doc = self.read_doc()?;
        let mut out = vec![];
        if let Some(arr) = doc.get_mut("grants").and_then(|g| g.as_array_mut()) {
            arr.retain(|g| {
                if gone(g) {
                    out.push(ExpiredGrant {
                        id: g["id"].as_str().unwrap_or("").to_string(),
                        path: g.pointer("/what/path").and_then(|p| p.as_str()).unwrap_or("").to_string(),
                        access: g.pointer("/what/access").and_then(|p| p.as_str()).unwrap_or("ro").to_string(),
                        run: g.get("run").and_then(|r| r.as_str()).map(|s| s.to_string()),
                    });
                    false
                } else {
                    true
                }
            });
        }
        if out.is_empty() {
            return Ok(out);
        }
        let old = self.scope_hash();
        let rev = self.write_doc(doc, now)?;
        let new = self.scope_hash();
        for g in &out {
            self.log(now, "machine", "scope.grant-end", &format!("{} ({})", g.path, g.id), Verdict::Allow, &format!("grant {why}; jobs on it stopped; scope {} -> {} (revision {rev})", short(&old), short(&new)));
            stop_jobs(g);
        }
        Ok(out)
    }

    fn load_pending(&self) -> Vec<PendingChange> {
        let Ok(t) = std::fs::read_to_string(&self.pending_file) else { return vec![] };
        let Ok(Value::Array(a)) = serde_json::from_str::<Value>(&t) else { return vec![] };
        a.iter()
            .filter_map(|v| Some(PendingChange { id: v.get("id")?.as_str()?.into(), t: v.get("t")?.as_i64()?, account: v.get("account")?.as_str()?.into(), proposal: Proposal::from_json(v.get("proposal")?).ok()? }))
            .collect()
    }

    fn save_pending(&self, items: &[PendingChange]) -> Result<(), String> {
        let v: Vec<Value> = items.iter().map(|p| json!({"id": p.id, "t": p.t, "account": p.account, "proposal": p.proposal.to_json(), "summary": p.proposal.describe()})).collect();
        atomic_write(&self.pending_file, Value::Array(v).to_string().as_bytes())
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::machine::scope::Op;

    const NOW: i64 = 1_790_000_000;

    struct World {
        dir: PathBuf,
        scope: PathBuf,
        log: AuditLog,
    }

    fn world(tag: &str, scope_json: &str) -> World {
        let dir = std::env::temp_dir().join(format!("pgm-chg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("state")).unwrap();
        std::fs::create_dir_all(dir.join("home/a")).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let scope = dir.join("state/scope.json");
        std::fs::write(&scope, scope_json.replace("HOME", dir.join("home").to_str().unwrap())).unwrap();
        let log = AuditLog::open(dir.join("state/machine-audit.jsonl")).unwrap();
        World { dir, scope, log }
    }
    fn me() -> LocalApproval {
        LocalApproval::from_machine("machine-ui")
    }
    const TWO: &str = r#"{"v":1,"revision":4,"owner":"sponge","preset":"folders","paths":[{"path":"HOME/a","access":"rw"},{"path":"HOME/b","access":"ro"}]}"#;

    #[test]
    fn r333_a_widening_proposal_changes_nothing_until_the_machine_approves() {
        let w = world("widen", TWO);
        let st = ScopeStore::open(&w.scope, w.dir.join("state"), Some(&w.log));
        let before = std::fs::read(&w.scope).unwrap();
        let h0 = st.scope_hash();
        let add = Proposal::AddPath { path: "/srv/data".into(), access: Access::Ro };
        let rw = Proposal::SetAccess { path: Some(w.dir.join("home/b").to_str().unwrap().into()), access: Access::Rw };
        let wide = Proposal::SetPreset(Preset::Full);
        let tool = Proposal::SetTool { tool: "fetch".into(), enabled: true };
        let grant = Proposal::AddGrant { path: "/dl".into(), access: Access::Ro, expires_at: Some(NOW + 3600), run: None, note: "x".into() };
        let mut ids = vec![];
        for p in [add, rw, wide, tool, grant] {
            match st.from_web("sponge", p.clone(), NOW).unwrap() {
                Outcome::Pending { id } => ids.push(id),
                o => panic!("{p:?} gave {o:?}"),
            }
        }
        assert_eq!(std::fs::read(&w.scope).unwrap(), before, "the scope file is byte-identical");
        assert_eq!(st.scope_hash(), h0);
        let s = Scope::load(&w.scope);
        assert!(!s.allow(Op::Read, "/srv/data/x", NOW).allow);
        assert_eq!(st.pending().len(), 5);
        // approve one on the machine
        let o = st.approve(&ids[0], &me(), NOW + 10).unwrap();
        let Outcome::Applied { revision, old_hash, new_hash } = o else { panic!() };
        assert_eq!((revision, old_hash.as_str()), (5, h0.as_str()));
        assert_ne!(new_hash, h0);
        assert!(Scope::load(&w.scope).allow(Op::Read, "/srv/data/x", NOW).allow);
        assert_eq!(st.pending().len(), 4);
        // reject another: nothing changes
        let h1 = st.scope_hash();
        st.reject(&ids[1], &me(), NOW + 11).unwrap();
        assert_eq!(st.scope_hash(), h1);
        assert!(st.approve(&ids[1], &me(), NOW + 12).is_err(), "gone");
        // a proposal too old cannot be approved
        assert!(st.approve(&ids[2], &me(), NOW + PENDING_TTL_SECS + 1).is_err());
        assert_eq!(st.scope_hash(), h1);
        // the audit log has the proposals, the approval and the rejection
        let ops: Vec<String> = w.log.records().iter().map(|r| r.op.clone()).collect();
        assert_eq!(ops.iter().filter(|o| *o == "scope.propose").count(), 5);
        for o in ["scope.widen", "scope.reject", "scope.expired"] {
            assert!(ops.contains(&o.to_string()), "{o}");
        }
        assert!(w.log.verify().ok);
        // a bad file is an error and the pending list stays readable
        assert!(ScopeStore::open(w.dir.join("none.json"), w.dir.join("state"), None).from_web("a", Proposal::SetPreset(Preset::Full), NOW).is_err());
    }

    #[test]
    fn r333_a_narrowing_from_the_web_applies_at_once_and_is_audited() {
        let w = world("narrow", TWO);
        let st = ScopeStore::open(&w.scope, w.dir.join("state"), Some(&w.log));
        let a = w.dir.join("home/a");
        let a = a.to_str().unwrap();
        assert!(Scope::load(&w.scope).allow(Op::Write, &format!("{a}/f"), NOW).allow);
        let o = st.from_web("sponge", Proposal::SetAccess { path: None, access: Access::Ro }, NOW).unwrap();
        assert!(matches!(o, Outcome::Applied { revision: 5, .. }));
        let s = Scope::load(&w.scope);
        assert!(!s.allow(Op::Write, &format!("{a}/f"), NOW).allow, "read-only now");
        assert!(s.allow(Op::Read, &format!("{a}/f"), NOW).allow);
        assert!(st.pending().is_empty());
        // remove a folder
        let o = st.from_web("sponge", Proposal::RemovePath { path: a.into() }, NOW + 1).unwrap();
        assert!(matches!(o, Outcome::Applied { revision: 6, .. }));
        assert!(!Scope::load(&w.scope).allow(Op::Read, &format!("{a}/f"), NOW).allow);
        assert!(st.from_web("sponge", Proposal::RemovePath { path: a.into() }, NOW + 2).is_err(), "already gone");
        // tools off and a narrower preset
        let d = st.from_web("sponge", Proposal::SetTool { tool: "shell".into(), enabled: false }, NOW + 3).unwrap();
        assert!(matches!(d, Outcome::Applied { .. }));
        assert!(!Scope::load(&w.scope).tools.shell.enabled);
        assert_eq!(st.from_web("sponge", Proposal::SetTool { tool: "shell".into(), enabled: false }, NOW + 4).unwrap(), Outcome::NoChange);
        let d = st.from_web("sponge", Proposal::SetPreset(Preset::ReadOnly), NOW + 5).unwrap();
        assert!(matches!(d, Outcome::Applied { .. }));
        // the audit: who, old hash and new hash
        let recs = w.log.records();
        let narrows: Vec<_> = recs.iter().filter(|r| r.op == "scope.narrow").collect();
        assert_eq!(narrows.len(), 4);
        assert!(narrows[0].reason.contains("hub-narrowing") && narrows[0].reason.contains("->") && narrows[0].reason.contains("revision 5"));
        assert_eq!(narrows[0].actor, "sponge");
        assert!(w.log.verify().ok);
    }

    #[test]
    fn r333_classification_never_lets_a_widening_pass_as_a_narrowing() {
        let doc: Value = serde_json::from_str(&TWO.replace("HOME", "/h")).unwrap();
        let k = |p: Proposal| classify(&doc, &p);
        assert_eq!(k(Proposal::SetPreset(Preset::Folder)), Kind::Narrow);
        assert_eq!(k(Proposal::SetPreset(Preset::Folders)), Kind::Same);
        assert_eq!(k(Proposal::SetPreset(Preset::Unrestricted)), Kind::Widen);
        assert_eq!(k(Proposal::SetPreset(Preset::Custom)), Kind::Widen, "custom cannot be ranked");
        assert_eq!(k(Proposal::SetPreset(Preset::NoShell)), Kind::Widen);
        assert_eq!(k(Proposal::SetAccess { path: None, access: Access::Rw }), Kind::Widen, "b is ro");
        assert_eq!(k(Proposal::SetAccess { path: Some("/h/a".into()), access: Access::Rw }), Kind::Same);
        assert_eq!(k(Proposal::SetAccess { path: Some("/h/a/".into()), access: Access::Ro }), Kind::Narrow);
        assert_eq!(k(Proposal::SetAccess { path: Some("/elsewhere".into()), access: Access::Rw }), Kind::Widen);
        assert_eq!(k(Proposal::SetTool { tool: "elevate".into(), enabled: true }), Kind::Widen);
        assert_eq!(k(Proposal::SetTool { tool: "fetch".into(), enabled: true }), Kind::Widen);
        assert_eq!(k(Proposal::SetTool { tool: "shell".into(), enabled: true }), Kind::Same);
        assert_eq!(k(Proposal::AddPath { path: "/h/a".into(), access: Access::Rw }), Kind::Widen);
        // the wire form: unknown kinds and tools are refused, not ignored
        for bad in [json!({"kind": "set-owner", "owner": "x"}), json!({"kind": "set-tool", "tool": "audit", "enabled": false}), json!({"kind": "add-path", "path": "/x"}), json!({"path": "/x"}), json!({"kind": "set-preset", "preset": "all"})] {
            assert!(Proposal::from_json(&bad).is_err(), "{bad}");
        }
        for p in [Proposal::AddPath { path: "/x".into(), access: Access::Rw }, Proposal::SetPreset(Preset::Full), Proposal::SetTool { tool: "processes".into(), enabled: false }, Proposal::RevokeGrant { id: "g".into() }] {
            assert_eq!(Proposal::from_json(&p.to_json()).unwrap(), p);
        }
    }

    #[test]
    fn r333_a_local_widening_with_a_second_folder_works_under_the_folder_preset() {
        let w = world("folder", r#"{"preset":"folder","paths":[{"path":"HOME/a","access":"rw"}]}"#);
        let st = ScopeStore::open(&w.scope, w.dir.join("state"), Some(&w.log));
        let b = w.dir.join("home/b");
        let p = Proposal::AddPath { path: b.to_str().unwrap().into(), access: Access::Ro };
        let Outcome::Pending { id } = st.from_web("sponge", p, NOW).unwrap() else { panic!() };
        st.approve(&id, &me(), NOW + 1).unwrap();
        let s = Scope::load(&w.scope);
        assert!(s.allow(Op::Read, b.join("x").to_str().unwrap(), NOW).allow);
        assert!(s.allow(Op::Write, w.dir.join("home/a/x").to_str().unwrap(), NOW).allow);
        // apply_local widens at once and names the surface
        let c = w.dir.join("home/c");
        st.apply_local(Proposal::AddPath { path: c.to_str().unwrap().into(), access: Access::Rw }, &LocalApproval::from_machine("terminal"), NOW + 2).unwrap();
        assert!(Scope::load(&w.scope).allow(Op::Write, c.join("x").to_str().unwrap(), NOW).allow);
        assert_eq!(w.log.records().last().unwrap().actor, "terminal");
        // a change that would not parse is refused whole and leaves the file alone
        let before = std::fs::read(&w.scope).unwrap();
        assert!(st.apply_local(Proposal::AddPath { path: "relative/not/absolute".into(), access: Access::Ro }, &me(), NOW + 3).is_err());
        assert_eq!(std::fs::read(&w.scope).unwrap(), before);
    }

    #[test]
    fn r333_a_grant_expires_on_time_and_a_running_job_on_it_is_stopped() {
        let w = world("grant", r#"{"preset":"folders","paths":[{"path":"HOME/a","access":"rw"}]}"#);
        let st = ScopeStore::open(&w.scope, w.dir.join("state"), Some(&w.log));
        let dl = w.dir.join("home/dl");
        let dls = dl.to_str().unwrap();
        // the web asks, the machine approves: a one-hour grant, and a "this run" grant
        let g1 = Proposal::AddGrant { path: dls.into(), access: Access::Ro, expires_at: Some(NOW + 3600), run: None, note: "one-off".into() };
        let g2 = Proposal::AddGrant { path: w.dir.join("home/tmp").to_str().unwrap().into(), access: Access::Rw, expires_at: None, run: Some("run-9".into()), note: "this run".into() };
        for g in [g1, g2] {
            let Outcome::Pending { id } = st.from_web("sponge", g, NOW).unwrap() else { panic!() };
            st.approve(&id, &me(), NOW + 1).unwrap();
        }
        let s = Scope::load(&w.scope);
        assert!(s.allow(Op::Read, &format!("{dls}/f"), NOW + 3599).allow);
        // on the clock, with no sweep at all: refused at the expiry second
        assert!(!s.allow(Op::Read, &format!("{dls}/f"), NOW + 3600).allow);
        // nothing is due early
        let mut stopped: Vec<ExpiredGrant> = vec![];
        assert!(st.expire_grants(NOW + 100, &mut |g| stopped.push(g.clone())).unwrap().is_empty());
        assert!(stopped.is_empty());
        // the sweep after the time: removed from the file, audited, the job callback called with the path
        let gone = st.expire_grants(NOW + 3601, &mut |g| stopped.push(g.clone())).unwrap();
        assert_eq!(gone.len(), 1);
        assert_eq!(stopped[0].path, dls);
        assert_eq!(stopped[0].access, "ro");
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&w.scope).unwrap()).unwrap();
        assert_eq!(doc["grants"].as_array().unwrap().len(), 1);
        assert!(!Scope::load(&w.scope).allow(Op::Read, &format!("{dls}/f"), NOW + 3601).allow);
        // the run grant ends with its run, not before
        assert!(st.end_run("run-8", NOW + 3602, &mut |_| panic!("not this run")).unwrap().is_empty());
        let tmpf = w.dir.join("home/tmp/f");
        assert!(Scope::load(&w.scope).allow(Op::Write, tmpf.to_str().unwrap(), NOW + 3602).allow);
        let mut ended = vec![];
        st.end_run("run-9", NOW + 3603, &mut |g| ended.push(g.path.clone())).unwrap();
        assert_eq!(ended.len(), 1);
        assert!(!Scope::load(&w.scope).allow(Op::Write, tmpf.to_str().unwrap(), NOW + 3603).allow);
        assert!(w.log.records().iter().filter(|r| r.op == "scope.grant-end").count() == 2);
        assert!(w.log.verify().ok);
        // a grant with an unreadable expiry never applies and is swept
        std::fs::write(&w.scope, r#"{"preset":"folders","paths":[],"grants":[{"id":"x","what":{"path":"/dl","access":"ro"},"expiresAt":"soon"}]}"#).unwrap();
        let mut n = 0;
        st.expire_grants(NOW, &mut |_| n += 1).unwrap();
        assert_eq!(n, 1);
        // revoking a grant from the web is a narrowing
        let w2 = world("grant2", r#"{"preset":"folders","paths":[],"grants":[{"id":"g1","what":{"path":"/dl","access":"ro"}}]}"#);
        let st2 = ScopeStore::open(&w2.scope, w2.dir.join("state"), Some(&w2.log));
        assert!(matches!(st2.from_web("sponge", Proposal::RevokeGrant { id: "g1".into() }, NOW).unwrap(), Outcome::Applied { .. }));
    }

    #[test]
    fn r333_no_tool_can_read_or_change_the_scope_or_its_neighbours() {
        let w = world("floor", TWO);
        let state = w.dir.join("state");
        for f in ["KILL", "elevate-pending.json", "scope-pending.json", "machine-audit.jsonl.head", "machine-audit.jsonl.anchor", "machine-audit.jsonl.1", "token"] {
            std::fs::write(state.join(f), "x").unwrap();
        }
        std::fs::create_dir_all(state.join("keys")).unwrap();
        let floor = state_floor(Floor::new(state.join("token"), state.join("keys"), &w.scope, state.join("machine-audit.jsonl")), &state);
        let protected = ["scope.json", "scope.json.tmp", "scope-pending.json", "KILL", "elevate-pending.json", "machine-audit.jsonl", "machine-audit.jsonl.head", "machine-audit.jsonl.anchor", "machine-audit.jsonl.1", "token", "keys/device.key"];
        #[cfg(unix)]
        std::os::unix::fs::symlink(state.join("scope.json"), w.dir.join("home/a/innocent.txt")).unwrap();
        let docs = [r#"{"preset":"unrestricted"}"#, r#"{"preset":"full"}"#, r#"{"preset":"custom","paths":[{"path":"*","access":"rw"}]}"#, r#"{"preset":"folders","paths":[{"path":"HOME","access":"rw"}]}"#];
        for d in docs {
            let s = Scope::try_parse(&d.replace("HOME", w.dir.to_str().unwrap())).unwrap().with_floor(floor.clone());
            for rel in protected {
                let abs = state.join(rel);
                for op in [Op::Read, Op::Write, Op::Delete, Op::List, Op::Exec, Op::Shell] {
                    let dec = s.allow(op, abs.to_str().unwrap(), NOW);
                    assert!(!dec.allow, "{d} {rel} {}: {}", op.name(), dec.reason);
                }
                assert!(s.open(Op::Read, &w.dir.join("home/a"), abs.to_str().unwrap(), crate::machine::paths::OpenMode::Read, NOW).is_err(), "{d} {rel}");
                assert!(s.open(Op::Write, &w.dir.join("home/a"), &format!("../../state/{rel}"), crate::machine::paths::OpenMode::WriteTruncate, NOW).is_err(), "{d} {rel}");
            }
            #[cfg(unix)]
            for op in [Op::Read, Op::Write, Op::Delete] {
                assert!(s.authorize(op, &w.dir.join("home/a"), "innocent.txt", NOW).is_err(), "link to the scope file, {d}");
            }
            // and the tool list has nothing that touches scope, audit, kill or grants
            for t in s.tool_names(NOW) {
                for bad in ["scope", "audit", "kill_switch", "stop_all", "grant", "limit", "approve"] {
                    assert!(!t.contains(bad), "tool {t}");
                }
            }
        }
        // even a scope widened to the whole system by an approved proposal keeps the floor
        let st = ScopeStore::open(&w.scope, &state, Some(&w.log));
        let Outcome::Pending { id } = st.from_web("sponge", Proposal::AddPath { path: state.to_str().unwrap().into(), access: Access::Rw }, NOW).unwrap() else { panic!() };
        st.approve(&id, &me(), NOW).unwrap();
        let s = Scope::load(&w.scope).with_floor(floor);
        assert!(!s.allow(Op::Read, state.join("scope.json").to_str().unwrap(), NOW).allow);
        assert!(!s.allow(Op::Delete, state.join("KILL").to_str().unwrap(), NOW).allow);
        assert!(s.allow(Op::List, state.to_str().unwrap(), NOW).allow, "the folder itself is only listable, never its protected files");
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(parse_iso8601(&iso8601(1_790_000_000)), Some(1_790_000_000));
        assert_eq!(parse_iso8601(&iso8601(951_782_400)), Some(951_782_400), "29 Feb 2000");
    }
}
