//! The scope model and its evaluator (linked-machines.md section 5, R321).
//!
//! A [`Scope`] is the document that lives on the machine (the 5.3 JSON). It answers `allow(op, path)`:
//! 1. the safety floor first (token, device key, scope file, audit log: never, under any scope);
//! 2. `unrestricted` allows everything else;
//! 3. the tool gate (files / shell / processes / network switches);
//! 4. deny wins over allow: any deny path, deny glob or default-deny entry that matches refuses;
//! 5. the system rules on `full`-style scopes: raw device files denied, `/dev`, `/sys` and `/proc` never written
//!    (`/proc` stays readable, so process tools work);
//! 6. the most specific `paths` entry (or valid grant) wins, not the most permissive one; `ro` allows read / list.
//!
//! Fail closed: a file that does not parse is "nothing allowed" ([`Scope::load`], [`Scope::parse`]); a scope with a
//! newer `v` than [`SCOPE_VERSION`] is refused whole. Unknown keys are ignored.

use super::floor::{default_deny_globs, default_deny_paths, glob_matches, Floor};
use super::paths::{
    canonicalize, open_verified, parse_abs, Flavor, NormPath, OpenMode, PathError, Resolved,
};
use serde_json::{json, Map, Value};
use std::path::Path;

/// The newest scope document version this library reads.
pub const SCOPE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Ro,
    Rw,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Ro => "ro",
            Access::Rw => "rw",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Read,
    Write,
    Delete,
    List,
    /// Start a process (the path is its working folder or program).
    Exec,
    /// A shell call (the path is its cwd).
    Shell,
    /// A network call (no path; see [`Scope::check_net`] for what it may send).
    Net,
    /// List / kill processes (no path).
    Process,
}

impl Op {
    pub fn name(self) -> &'static str {
        match self {
            Op::Read => "read",
            Op::Write => "write",
            Op::Delete => "delete",
            Op::List => "list",
            Op::Exec => "exec",
            Op::Shell => "shell",
            Op::Net => "net",
            Op::Process => "process",
        }
    }
    pub fn parse(s: &str) -> Option<Op> {
        Some(match s {
            "read" => Op::Read,
            "write" => Op::Write,
            "delete" => Op::Delete,
            "list" => Op::List,
            "exec" => Op::Exec,
            "shell" => Op::Shell,
            "net" => Op::Net,
            "process" => Op::Process,
            _ => return None,
        })
    }
    fn changes_tree(self) -> bool {
        matches!(self, Op::Write | Op::Delete)
    }
    fn needs_path(self) -> bool {
        !matches!(self, Op::Net | Op::Process)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Folder,
    Folders,
    ReadOnly,
    Full,
    FullRo,
    DenyList,
    NoShell,
    NoNetwork,
    Unrestricted,
    Custom,
}

impl Preset {
    pub fn as_str(self) -> &'static str {
        match self {
            Preset::Folder => "folder",
            Preset::Folders => "folders",
            Preset::ReadOnly => "read-only",
            Preset::Full => "full",
            Preset::FullRo => "full-ro",
            Preset::DenyList => "deny-list",
            Preset::NoShell => "no-shell",
            Preset::NoNetwork => "no-network",
            Preset::Unrestricted => "unrestricted",
            Preset::Custom => "custom",
        }
    }
    pub fn parse(s: &str) -> Option<Preset> {
        Some(match s {
            "folder" => Preset::Folder,
            "folders" => Preset::Folders,
            "read-only" => Preset::ReadOnly,
            "full" => Preset::Full,
            "full-ro" => Preset::FullRo,
            "deny-list" => Preset::DenyList,
            "no-shell" => Preset::NoShell,
            "no-network" => Preset::NoNetwork,
            "unrestricted" => Preset::Unrestricted,
            "custom" => Preset::Custom,
            _ => return None,
        })
    }
    /// Does this preset carry the default deny list (7.6)? `folder`, `custom` and `unrestricted` do not.
    fn default_deny(self) -> bool {
        !matches!(self, Preset::Folder | Preset::Custom | Preset::Unrestricted)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub allow: bool,
    pub reason: String,
}

impl Decision {
    fn yes(reason: impl Into<String>) -> Decision {
        Decision { allow: true, reason: reason.into() }
    }
    fn no(reason: impl Into<String>) -> Decision {
        Decision { allow: false, reason: reason.into() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeError {
    /// Not JSON, or not an object.
    Json(String),
    /// `v` newer than this library reads: refused whole.
    TooNew { found: u64, supported: u32 },
    /// A field of the wrong type, or a path that cannot be read.
    Invalid(String),
    Io(String),
}

impl std::fmt::Display for ScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScopeError::Json(e) => write!(f, "scope is not valid JSON: {e}"),
            ScopeError::TooNew { found, supported } => {
                write!(f, "this scope needs a newer app (scope v{found}, this app reads up to v{supported})")
            }
            ScopeError::Invalid(e) => write!(f, "invalid scope: {e}"),
            ScopeError::Io(e) => write!(f, "scope file: {e}"),
        }
    }
}
impl std::error::Error for ScopeError {}

#[derive(Clone, Debug)]
enum Root {
    /// `"*"`: the whole system (`/`, or every local drive).
    Any,
    Path(NormPath),
}

#[derive(Clone, Debug)]
struct PathEntry {
    root: Root,
    access: Access,
    follow_links: bool,
    /// Some(id) for a time-limited grant.
    grant: Option<String>,
    expires_at: Option<i64>,
}

#[derive(Clone, Debug)]
enum DenyEntry {
    Path(NormPath),
    Glob(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillMode {
    None,
    OwnChildren,
    Any,
}

#[derive(Clone, Debug)]
pub struct ShellCfg {
    pub enabled: bool,
    pub cwd_confined: bool,
    pub allow_commands: Option<Vec<String>>,
    pub deny_commands: Vec<String>,
    pub timeout_sec: u64,
    pub max_output_kb: u64,
}

#[derive(Clone, Debug)]
pub struct Tools {
    pub files: bool,
    pub shell: ShellCfg,
    pub proc_list: bool,
    pub proc_kill: KillMode,
    pub proc_start: bool,
    pub net_fetch: bool,
    pub shell_network: String,
    pub containers_re: bool,
    pub elevate: bool,
}

impl Default for Tools {
    fn default() -> Tools {
        Tools {
            files: true,
            shell: ShellCfg {
                enabled: true,
                cwd_confined: true,
                allow_commands: None,
                deny_commands: vec![],
                timeout_sec: 600,
                max_output_kb: 512,
            },
            proc_list: true,
            proc_kill: KillMode::OwnChildren,
            proc_start: true,
            net_fetch: false,
            shell_network: "default".into(),
            containers_re: false,
            elevate: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Scope {
    pub v: u32,
    pub revision: u64,
    pub owner: String,
    pub preset: Preset,
    pub unattended: bool,
    pub unrestricted: bool,
    pub tools: Tools,
    flavor: Flavor,
    home: Option<String>,
    paths: Vec<PathEntry>,
    deny: Vec<DenyEntry>,
    defaults: Vec<DenyEntry>,
    default_deny: bool,
    floor: Floor,
    /// Why the scope is "nothing allowed" (a bad or missing file), for the machine's screen and log.
    pub error: Option<String>,
}

// ---------------------------------------------------------------- JSON helpers

fn bad<T>(msg: impl Into<String>) -> Result<T, ScopeError> {
    Err(ScopeError::Invalid(msg.into()))
}

fn get_bool(o: &Map<String, Value>, k: &str, default: bool) -> Result<bool, ScopeError> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => bad(format!("{k} must be true or false")),
    }
}

fn get_u64(o: &Map<String, Value>, k: &str, default: u64) -> Result<u64, ScopeError> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_u64().map_or_else(|| bad(format!("{k} must be a number")), Ok),
    }
}

fn get_str<'a>(o: &'a Map<String, Value>, k: &str) -> Result<Option<&'a str>, ScopeError> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => bad(format!("{k} must be text")),
    }
}

fn get_obj<'a>(o: &'a Map<String, Value>, k: &str) -> Result<Option<&'a Map<String, Value>>, ScopeError> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => Ok(Some(m)),
        Some(_) => bad(format!("{k} must be an object")),
    }
}

fn get_arr<'a>(o: &'a Map<String, Value>, k: &str) -> Result<Option<&'a Vec<Value>>, ScopeError> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => Ok(Some(a)),
        Some(_) => bad(format!("{k} must be a list")),
    }
}

fn str_list(o: &Map<String, Value>, k: &str) -> Result<Option<Vec<String>>, ScopeError> {
    match get_arr(o, k)? {
        None => Ok(None),
        Some(a) => a
            .iter()
            .map(|v| v.as_str().map(|s| s.to_string()).map_or_else(|| bad(format!("{k} must hold text")), Ok))
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
    }
}

fn parse_access(s: Option<&str>) -> Result<Access, ScopeError> {
    match s {
        None | Some("ro") => Ok(Access::Ro),
        Some("rw") => Ok(Access::Rw),
        Some(o) => bad(format!("access must be \"ro\" or \"rw\", not {o:?}")),
    }
}

fn parse_root(flavor: Flavor, s: &str) -> Result<Root, ScopeError> {
    if s == "*" {
        return Ok(Root::Any);
    }
    match parse_abs(flavor, s) {
        Ok(p) => Ok(Root::Path(p)),
        Err(e) => bad(format!("path {s:?}: {e}")),
    }
}

/// Days since 1970-01-01 of a civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `2026-10-25T22:00:00Z` (also `+hh:mm` offsets and fractional seconds) to Unix seconds.
pub fn parse_iso8601(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b't' || b[10] == b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).map_or(false, |c| c.is_ascii_digit()) {
            i += 1;
        }
    }
    let off = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == b.len() && b[i + 3] == b':' => {
            let oh = s.get(i + 1..i + 3)?.parse::<i64>().ok()?;
            let om = s.get(i + 4..i + 6)?.parse::<i64>().ok()?;
            let t = oh * 3600 + om * 60;
            if *sign == b'+' { t } else { -t }
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - off)
}

fn default_home() -> Option<String> {
    let k = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(k).ok().filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------- construction

impl Scope {
    /// Nothing allowed (the answer to a missing or bad file).
    pub fn nothing() -> Scope {
        Scope::nothing_with(Flavor::native(), None)
    }

    fn nothing_with(flavor: Flavor, error: Option<String>) -> Scope {
        let mut tools = Tools::default();
        tools.files = false;
        tools.shell.enabled = false;
        tools.proc_list = false;
        tools.proc_kill = KillMode::None;
        tools.proc_start = false;
        Scope {
            v: SCOPE_VERSION,
            revision: 0,
            owner: String::new(),
            preset: Preset::Custom,
            unattended: false,
            unrestricted: false,
            tools,
            flavor,
            home: None,
            paths: vec![],
            deny: vec![],
            defaults: vec![],
            default_deny: false,
            floor: Floor::empty(),
            error,
        }
    }

    /// The default scope at link time: one folder, read-write, shell confined to it (13.2).
    pub fn default_folder(folder: &str) -> Scope {
        Scope::preset(Preset::Folder, &[(folder, Access::Rw)], &[])
    }

    /// A ready-made scope. `paths` are `(path, access)` (the path `"*"` means the whole system); `deny` are extra
    /// deny entries (a glob when it contains `*` or `?`, else a path). A bad argument gives "nothing allowed".
    pub fn preset(preset: Preset, paths: &[(&str, Access)], deny: &[&str]) -> Scope {
        Scope::preset_with(Flavor::native(), preset, paths, deny)
    }

    pub fn preset_with(flavor: Flavor, preset: Preset, paths: &[(&str, Access)], deny: &[&str]) -> Scope {
        let doc = json!({
            "v": SCOPE_VERSION,
            "preset": preset.as_str(),
            "paths": paths.iter().map(|(p, a)| json!({"path": p, "access": a.as_str()})).collect::<Vec<_>>(),
            "deny": deny.iter().map(|d| if d.contains(['*', '?']) { json!({"glob": d}) } else { json!({"path": d}) }).collect::<Vec<_>>(),
        });
        Scope::parse_with(&doc.to_string(), flavor)
    }

    /// Read the scope file: a missing or unreadable file, bad JSON, a bad field or a newer `v` all mean "nothing
    /// allowed" (with `error` saying why).
    pub fn load(file: &Path) -> Scope {
        match std::fs::read_to_string(file) {
            Ok(s) => Scope::parse(&s),
            Err(e) => Scope::nothing_with(Flavor::native(), Some(ScopeError::Io(e.to_string()).to_string())),
        }
    }

    /// [`Scope::try_parse`], failing closed.
    pub fn parse(json: &str) -> Scope {
        Scope::parse_with(json, Flavor::native())
    }

    pub fn parse_with(json: &str, flavor: Flavor) -> Scope {
        match Scope::try_parse_with(json, flavor) {
            Ok(s) => s,
            Err(e) => Scope::nothing_with(flavor, Some(e.to_string())),
        }
    }

    pub fn try_parse(json: &str) -> Result<Scope, ScopeError> {
        Scope::try_parse_with(json, Flavor::native())
    }

    pub fn try_parse_with(json: &str, flavor: Flavor) -> Result<Scope, ScopeError> {
        let val: Value = serde_json::from_str(json).map_err(|e| ScopeError::Json(e.to_string()))?;
        let o = val.as_object().ok_or_else(|| ScopeError::Json("not an object".into()))?;
        let v = match o.get("v") {
            None | Some(Value::Null) => 1,
            Some(x) => x.as_u64().map_or_else(|| bad("v must be a number"), Ok)?,
        };
        if v > SCOPE_VERSION as u64 {
            return Err(ScopeError::TooNew { found: v, supported: SCOPE_VERSION });
        }
        let mut s = Scope::nothing_with(flavor, None);
        s.v = v as u32;
        s.revision = get_u64(o, "revision", 0)?;
        s.owner = get_str(o, "owner")?.unwrap_or("").to_string();
        s.unattended = get_bool(o, "unattended", false)?;
        s.unrestricted = get_bool(o, "unrestricted", false)?;
        s.preset = match get_str(o, "preset")? {
            None => Preset::Folder,
            Some(p) => Preset::parse(p).unwrap_or(Preset::Custom),
        };
        if let Some(a) = get_arr(o, "paths")? {
            for e in a {
                let m = e.as_object().map_or_else(|| bad("a paths entry must be an object"), Ok)?;
                let path = get_str(m, "path")?.map_or_else(|| bad("a paths entry needs a path"), Ok)?;
                s.paths.push(PathEntry {
                    root: parse_root(flavor, path)?,
                    access: parse_access(get_str(m, "access")?)?,
                    follow_links: get_bool(m, "followLinks", false)?,
                    grant: None,
                    expires_at: None,
                });
            }
        }
        if let Some(a) = get_arr(o, "deny")? {
            for e in a {
                let m = e.as_object().map_or_else(|| bad("a deny entry must be an object"), Ok)?;
                match (get_str(m, "path")?, get_str(m, "glob")?) {
                    (Some(p), None) => match parse_root(flavor, p)? {
                        Root::Path(n) => s.deny.push(DenyEntry::Path(n)),
                        Root::Any => return bad("a deny entry cannot be \"*\" (use no paths instead)"),
                    },
                    (None, Some(g)) => s.deny.push(DenyEntry::Glob(g.to_string())),
                    _ => return bad("a deny entry needs exactly one of path or glob"),
                }
            }
        }
        s.tools = parse_tools(o)?;
        if let Some(a) = get_arr(o, "grants")? {
            for g in a {
                let m = g.as_object().map_or_else(|| bad("a grant must be an object"), Ok)?;
                let id = get_str(m, "id")?.unwrap_or("").to_string();
                let what = get_obj(m, "what")?.map_or_else(|| bad("a grant needs what"), Ok)?;
                let path = get_str(what, "path")?.map_or_else(|| bad("a grant needs what.path"), Ok)?;
                // an expiry that cannot be read means the grant never applies
                let expires_at = match m.get("expiresAt") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(t)) => Some(parse_iso8601(t).unwrap_or(i64::MIN)),
                    Some(Value::Number(n)) => Some(n.as_i64().unwrap_or(i64::MIN)),
                    Some(_) => Some(i64::MIN),
                };
                s.paths.push(PathEntry {
                    root: parse_root(flavor, path)?,
                    access: parse_access(get_str(what, "access")?)?,
                    follow_links: false,
                    grant: Some(id),
                    expires_at,
                });
            }
        }
        s.apply_preset();
        s.home = default_home();
        s.finish();
        Ok(s)
    }

    fn apply_preset(&mut self) {
        let has_any = |s: &Scope| s.paths.iter().any(|e| e.grant.is_none() && matches!(e.root, Root::Any));
        let add_any = |s: &mut Scope, a: Access| {
            if !has_any(s) {
                s.paths.push(PathEntry { root: Root::Any, access: a, follow_links: false, grant: None, expires_at: None });
            }
        };
        let force_ro = |s: &mut Scope| {
            for e in &mut s.paths {
                e.access = Access::Ro;
            }
        };
        let no_shell = |s: &mut Scope| {
            s.tools.shell.enabled = false;
            s.tools.proc_kill = KillMode::None;
            s.tools.proc_start = false;
            s.tools.proc_list = false;
        };
        self.default_deny = self.preset.default_deny();
        match self.preset {
            Preset::Folder => {
                // one folder: the first entry of the document, nothing else
                let mut first = None;
                self.paths.retain(|e| {
                    if e.grant.is_some() {
                        return true;
                    }
                    if first.is_none() {
                        first = Some(());
                        true
                    } else {
                        false
                    }
                });
            }
            Preset::Folders | Preset::Custom => {}
            Preset::ReadOnly => {
                force_ro(self);
                no_shell(self);
            }
            Preset::Full => add_any(self, Access::Rw),
            Preset::FullRo => {
                add_any(self, Access::Ro);
                force_ro(self);
                no_shell(self);
            }
            Preset::DenyList => {
                if self.paths.iter().all(|e| e.grant.is_some()) {
                    add_any(self, Access::Rw);
                }
            }
            Preset::NoShell => no_shell(self),
            Preset::NoNetwork => {
                self.tools.net_fetch = false;
                self.tools.shell_network = "off".into();
            }
            Preset::Unrestricted => self.unrestricted = true,
        }
        if self.preset == Preset::NoShell {
            self.tools.shell.enabled = false;
        }
    }

    /// Build the default deny entries from the flavor and the home folder.
    fn finish(&mut self) {
        self.defaults.clear();
        if !self.default_deny {
            return;
        }
        for g in default_deny_globs() {
            self.defaults.push(DenyEntry::Glob(g.to_string()));
        }
        if let Some(h) = &self.home {
            for p in default_deny_paths(self.flavor, h) {
                if let Ok(n) = parse_abs(self.flavor, &p) {
                    self.defaults.push(DenyEntry::Path(n));
                }
            }
        }
    }

    /// The machine's safety floor (its token, key, scope file, audit log). Applies under every scope.
    pub fn with_floor(mut self, floor: Floor) -> Scope {
        self.floor = floor;
        self
    }

    /// The person's home folder, for the default `~/.ssh`-style deny entries (default: the environment's).
    pub fn with_home(mut self, home: &str) -> Scope {
        self.home = Some(home.to_string());
        self.finish();
        self
    }

    pub fn flavor(&self) -> Flavor {
        self.flavor
    }
    pub fn floor(&self) -> &Floor {
        &self.floor
    }
    /// True for a scope that is "nothing allowed" because its file was missing, bad or too new.
    pub fn failed(&self) -> bool {
        self.error.is_some()
    }
}

fn parse_tools(o: &Map<String, Value>) -> Result<Tools, ScopeError> {
    let mut t = Tools::default();
    let Some(m) = get_obj(o, "tools")? else { return Ok(t) };
    t.files = get_bool(m, "files", true)?;
    match m.get("shell") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => t.shell.enabled = *b,
        Some(Value::Object(s)) => {
            t.shell.enabled = get_bool(s, "enabled", true)?;
            t.shell.cwd_confined = get_str(s, "cwd")?.map_or(true, |c| c != "any");
            t.shell.allow_commands = str_list(s, "allowCommands")?;
            t.shell.deny_commands = str_list(s, "denyCommands")?.unwrap_or_default();
            t.shell.timeout_sec = get_u64(s, "timeoutSec", 600)?;
            t.shell.max_output_kb = get_u64(s, "maxOutputKB", 512)?;
        }
        Some(_) => return bad("tools.shell must be true / false or an object"),
    }
    match m.get("processes") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => {
            t.proc_list = *b;
            t.proc_start = *b;
            if !*b {
                t.proc_kill = KillMode::None;
            }
        }
        Some(Value::Object(p)) => {
            t.proc_list = get_bool(p, "list", true)?;
            t.proc_start = get_bool(p, "start", true)?;
            t.proc_kill = match p.get("kill") {
                None | Some(Value::Null) => KillMode::OwnChildren,
                Some(Value::Bool(false)) => KillMode::None,
                Some(Value::String(s)) if s == "own-children" => KillMode::OwnChildren,
                Some(Value::String(s)) if s == "any" => KillMode::Any,
                Some(Value::String(s)) if s == "none" => KillMode::None,
                Some(_) => return bad("tools.processes.kill must be \"own-children\", \"any\" or \"none\""),
            };
        }
        Some(_) => return bad("tools.processes must be true / false or an object"),
    }
    if let Some(n) = get_obj(m, "network")? {
        t.net_fetch = get_bool(n, "fetch", false)?;
        t.shell_network = get_str(n, "shellNetwork")?.unwrap_or("default").to_string();
    }
    if let Some(c) = get_obj(m, "containers")? {
        t.containers_re = get_bool(c, "re", false)?;
    }
    t.elevate = get_bool(m, "elevate", false)?;
    Ok(t)
}

// ---------------------------------------------------------------- evaluation

impl Scope {
    fn entry_valid(e: &PathEntry, now: i64) -> bool {
        e.expires_at.map_or(true, |x| now < x)
    }

    fn root_matches(&self, root: &Root, p: &NormPath) -> bool {
        match root {
            Root::Any => !p.prefix().starts_with("\\\\"),
            Root::Path(r) => p.starts_with(r),
        }
    }

    fn specificity(root: &Root) -> usize {
        match root {
            Root::Any => 0,
            Root::Path(r) => 1 + r.depth(),
        }
    }

    /// The most specific valid entry for `p`; on a tie the stricter one. `via_links` also lets entries with
    /// `followLinks` match (used for the lexical spelling of a path whose real path is elsewhere).
    fn best_entry(&self, p: &NormPath, now: i64, only_follow: bool) -> Option<&PathEntry> {
        let mut best: Option<&PathEntry> = None;
        for e in &self.paths {
            if !Scope::entry_valid(e, now) || !self.root_matches(&e.root, p) || (only_follow && !e.follow_links) {
                continue;
            }
            best = match best {
                None => Some(e),
                Some(b) => {
                    let (sb, se) = (Scope::specificity(&b.root), Scope::specificity(&e.root));
                    if se > sb || (se == sb && e.access == Access::Ro && b.access == Access::Rw) {
                        Some(e)
                    } else {
                        Some(b)
                    }
                }
            };
        }
        best
    }

    fn denied_by(&self, p: &NormPath) -> Option<String> {
        for (list, tag) in [(&self.deny, "deny entry"), (&self.defaults, "default deny list")] {
            for d in list.iter() {
                match d {
                    DenyEntry::Path(r) if p.starts_with(r) => return Some(format!("{tag} {}", r.display())),
                    DenyEntry::Glob(g) if glob_matches(self.flavor, g, p) => return Some(format!("{tag} {g}")),
                    _ => {}
                }
            }
        }
        None
    }

    /// `/dev`, `/sys`, `/proc` rules for the system-wide scopes (Linux flavor; `unrestricted` lifts them).
    fn system_rule(&self, op: Op, p: &NormPath) -> Option<String> {
        if self.flavor != Flavor::Posix {
            return None;
        }
        let parts = p.parts();
        let first = parts.first().map(|s| s.as_str()).unwrap_or("");
        let second = parts.get(1).map(|s| s.as_str()).unwrap_or("");
        match first {
            "dev" => {
                let is_null = parts.len() == 2 && second == "null";
                if is_null {
                    return None;
                }
                if op.changes_tree() && second != "shm" {
                    return Some("/dev is read-only".into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileTypeExt;
                    if let Ok(md) = std::fs::symlink_metadata(p.to_path_buf()) {
                        let t = md.file_type();
                        if t.is_block_device() || t.is_char_device() {
                            return Some("raw device files are hidden".into());
                        }
                    }
                }
                None
            }
            "sys" if op.changes_tree() => Some("/sys is read-only".into()),
            "proc" => {
                if op.changes_tree() {
                    return Some("/proc is read-only".into());
                }
                let mem = second == "kcore" && parts.len() == 2 || parts.get(2).map(|s| s.as_str()) == Some("mem") && parts.len() == 3;
                if mem {
                    return Some("raw memory files are hidden".into());
                }
                None
            }
            _ => None,
        }
    }

    fn tool_gate(&self, op: Op) -> Result<(), &'static str> {
        let t = &self.tools;
        let ok = match op {
            Op::Read | Op::Write | Op::Delete | Op::List => t.files,
            Op::Shell => t.shell.enabled,
            Op::Exec => t.proc_start,
            Op::Process => t.proc_list || t.proc_start || t.proc_kill != KillMode::None,
            Op::Net => t.net_fetch,
        };
        if ok {
            Ok(())
        } else {
            Err(match op {
                Op::Read | Op::Write | Op::Delete | Op::List => "file tools are off in this scope",
                Op::Shell => "the shell is off in this scope",
                Op::Exec | Op::Process => "process tools are off in this scope",
                Op::Net => "network tools are off in this scope",
            })
        }
    }

    /// The question every tool asks: may `op` touch this path (a real, absolute path as the OS spells it)? `now` is
    /// Unix seconds, passed in so grants expire on the caller's clock. Ops without a path (Net, Process) ignore it.
    pub fn allow(&self, op: Op, path: &str, now: i64) -> Decision {
        if !op.needs_path() {
            return self.decide(op, None, None, now);
        }
        match parse_abs(self.flavor, path) {
            Ok(p) => self.decide(op, Some(&p), None, now),
            Err(e) => Decision::no(format!("refused path: {e}")),
        }
    }

    /// As [`Scope::allow`] for a path already parsed. `lexical` is the spelling before links were resolved: the
    /// floor and the deny rules look at both spellings.
    pub fn allow_norm(&self, op: Op, real: &NormPath, lexical: Option<&NormPath>, now: i64) -> Decision {
        self.decide(op, Some(real), lexical, now)
    }

    fn decide(&self, op: Op, real: Option<&NormPath>, lexical: Option<&NormPath>, now: i64) -> Decision {
        if self.failed() {
            return Decision::no(format!("nothing is allowed: {}", self.error.as_deref().unwrap_or("")));
        }
        let spellings: Vec<&NormPath> = real.into_iter().chain(lexical).collect();
        // 1. the safety floor, under every scope
        if spellings.iter().any(|p| self.floor.protects(p)) {
            return Decision::no("safety floor: the machine's own token, key, scope and audit log are never reachable");
        }
        // 2. unrestricted
        if self.unrestricted {
            return Decision::yes("unrestricted (the safety floor still applies)");
        }
        // 3. the tool gate
        if let Err(why) = self.tool_gate(op) {
            return Decision::no(why);
        }
        let Some(real) = real else {
            return Decision::yes(format!("{} tools are on", op.name()));
        };
        // 4. deny beats allow
        for p in &spellings {
            if let Some(why) = self.denied_by(p) {
                return Decision::no(format!("denied by {why}"));
            }
        }
        // 5. system rules
        for p in &spellings {
            if let Some(why) = self.system_rule(op, p) {
                return Decision::no(why);
            }
        }
        // 6. the most specific entry
        let entry = self.best_entry(real, now, false).or_else(|| lexical.and_then(|l| self.best_entry(l, now, true)));
        let Some(e) = entry else {
            return Decision::no(format!("{} is outside the scope; ask the owner to allow it", real.display()));
        };
        let what = match &e.root {
            Root::Any => "the whole system".to_string(),
            Root::Path(r) => r.display(),
        };
        if op.changes_tree() && e.access == Access::Ro {
            return Decision::no(format!("{what} is read-only in this scope"));
        }
        let grant = e.grant.as_ref().map_or(String::new(), |g| format!(" (grant {g})"));
        Decision::yes(format!("{} allowed in {what} ({}){grant}", op.name(), e.access.as_str()))
    }

    /// May a network call be made, sending these local files (a body, an upload) and this payload? The floor's
    /// files and registered secrets are never sent, whatever the scope.
    pub fn check_net(&self, files: &[&str], payload: &[u8], now: i64) -> Decision {
        if !self.floor.payload_sendable(payload) {
            return Decision::no("safety floor: a machine secret is in the payload");
        }
        for f in files {
            match parse_abs(self.flavor, f) {
                Ok(p) if self.floor.file_sendable(&p) => {}
                Ok(_) => return Decision::no("safety floor: that file is never sent"),
                Err(e) => return Decision::no(format!("refused path: {e}")),
            }
        }
        self.allow(Op::Net, "", now)
    }

    /// May this shell command line run? Applies `denyCommands` (substring, case-insensitive, whitespace folded) and
    /// `allowCommands` (the first word must be listed). Advisory by nature (5.5).
    pub fn command_allowed(&self, command: &str) -> Decision {
        if !self.unrestricted && !self.tools.shell.enabled {
            return Decision::no("the shell is off in this scope");
        }
        if self.unrestricted {
            return Decision::yes("unrestricted");
        }
        let folded = command.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        for d in &self.tools.shell.deny_commands {
            let d = d.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
            if !d.is_empty() && folded.contains(&d) {
                return Decision::no(format!("command denied by {d:?}"));
            }
        }
        if let Some(list) = &self.tools.shell.allow_commands {
            let first = folded.split(' ').next().unwrap_or("");
            if !list.iter().any(|a| a.to_lowercase() == first) {
                return Decision::no("command is not on the allow list");
            }
        }
        Decision::yes("command allowed")
    }

    /// Process kill: `own-children` kills only what a tool started; `any` needs a system-wide scope.
    pub fn kill_any_allowed(&self) -> bool {
        self.unrestricted
            || (self.tools.proc_kill == KillMode::Any && self.paths.iter().any(|e| matches!(e.root, Root::Any) && e.grant.is_none()))
    }

    fn has_paths(&self, now: i64) -> bool {
        self.unrestricted || self.paths.iter().any(|e| Scope::entry_valid(e, now))
    }

    fn can_write(&self, now: i64) -> bool {
        self.unrestricted || self.paths.iter().any(|e| Scope::entry_valid(e, now) && e.access == Access::Rw)
    }

    /// The machine tools this scope exposes (names as in 6.1). A tool that cannot ever be allowed is not listed, so
    /// `no-shell` removes the shell tools and a read-only scope shows no write tools.
    pub fn tool_names(&self, now: i64) -> Vec<&'static str> {
        let mut v = vec!["machine_info"];
        if self.failed() {
            return v;
        }
        let t = &self.tools;
        let any = |ok: bool| self.unrestricted || ok;
        if any(t.files) && self.has_paths(now) {
            v.extend(["list", "read", "stat", "search", "transfer_get"]);
            if self.can_write(now) {
                v.extend(["write", "delete", "mkdir", "move", "copy", "transfer_put"]);
            }
        }
        if any(t.shell.enabled) && self.has_paths(now) {
            v.extend(["shell_run", "job_start", "job_read", "job_stop"]);
        }
        if any(t.proc_list) {
            v.push("proc_list");
        }
        if self.unrestricted || t.proc_kill != KillMode::None {
            v.push("proc_kill");
        }
        if any(t.net_fetch) {
            v.push("fetch");
        }
        if any(t.containers_re) {
            v.extend(["re_list", "re_run", "re_logs", "re_stop"]);
        }
        if t.elevate {
            v.push("elevate");
        }
        v
    }

    pub fn has_tool(&self, name: &str, now: i64) -> bool {
        self.tool_names(now).contains(&name)
    }

    /// The folder `""` means: the first read-write entry, else the first entry (the "selected folder").
    pub fn selected_folder(&self, now: i64) -> Option<String> {
        let own = |e: &&PathEntry| e.grant.is_none() && Scope::entry_valid(e, now);
        let pick = self.paths.iter().filter(own).find(|e| e.access == Access::Rw && matches!(e.root, Root::Path(_))).or_else(|| self.paths.iter().filter(own).find(|e| matches!(e.root, Root::Path(_))));
        match pick.map(|e| &e.root) {
            Some(Root::Path(p)) => Some(p.display()),
            _ => None,
        }
    }

    /// The scope in words, for the card, the tray and `machine_info` (5.6).
    pub fn summary(&self, now: i64) -> String {
        if let Some(e) = &self.error {
            return format!("nothing allowed ({e})");
        }
        let shell = if self.unrestricted {
            "shell"
        } else if self.tools.shell.enabled {
            "shell (advisory)"
        } else {
            "no shell"
        };
        if self.unrestricted {
            return format!("unrestricted: no limits except the safety floor, {shell}");
        }
        let valid: Vec<&PathEntry> = self.paths.iter().filter(|e| Scope::entry_valid(e, now)).collect();
        if valid.is_empty() {
            return "nothing allowed (no folders)".into();
        }
        let list: Vec<String> = valid
            .iter()
            .map(|e| {
                let p = match &e.root {
                    Root::Any => "the whole system".to_string(),
                    Root::Path(r) => r.display(),
                };
                format!("{p} ({})", if e.access == Access::Rw { "read-write" } else { "read-only" })
            })
            .collect();
        let head = if valid.len() == 1 && matches!(self.preset, Preset::Folder) { "one folder" } else { "folders" };
        format!("{head}: {}; {shell}", list.join(", "))
    }
}

// ---------------------------------------------------------------- path checks against the real file system

impl Scope {
    /// Canonicalise `given` (relative to `base`, the selected folder) against the real file system and ask the
    /// scope about the real path and the typed spelling. The one door every file tool goes through.
    pub fn authorize(&self, op: Op, base: &Path, given: &str, now: i64) -> Result<Resolved, Decision> {
        let r = canonicalize(base, given, op.changes_tree()).map_err(|e: PathError| Decision::no(format!("refused path: {e}")))?;
        let d = self.allow_norm(op, &r.real, Some(&r.lexical), now);
        if d.allow {
            Ok(r)
        } else {
            Err(d)
        }
    }

    /// [`Scope::authorize`], then open without following a link swapped in after the check; what was really opened
    /// is judged by this scope again.
    pub fn open(&self, op: Op, base: &Path, given: &str, mode: OpenMode, now: i64) -> Result<std::fs::File, Decision> {
        let r = self.authorize(op, base, given, now)?;
        let verify = |p: &Path| match NormPath::from_native(p) {
            Ok(n) => self.allow_norm(op, &n, None, now).allow,
            Err(_) => false,
        };
        open_verified(&r, &verify, mode).map_err(|e| Decision::no(format!("refused: {e}")))
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000; // 2026-09

    fn l(preset: Preset, paths: &[(&str, Access)], deny: &[&str]) -> Scope {
        Scope::preset_with(Flavor::Posix, preset, paths, deny).with_home("/home/u")
    }
    fn w(preset: Preset, paths: &[(&str, Access)], deny: &[&str]) -> Scope {
        Scope::preset_with(Flavor::Windows, preset, paths, deny).with_home("C:\\People\\u")
    }
    fn ok(s: &Scope, op: Op, p: &str) -> bool {
        s.allow(op, p, NOW).allow
    }

    use Access::{Ro, Rw};
    use Op::*;

    /// (op, path, expected)
    type Row = (Op, &'static str, bool);

    fn run(name: &str, s: &Scope, rows: &[Row]) {
        for (op, p, want) in rows {
            let d = s.allow(*op, p, NOW);
            assert_eq!(d.allow, *want, "{name}: {} {p}: {}", op.name(), d.reason);
        }
    }

    #[test]
    fn preset_folder_is_the_default() {
        let s = Scope::default_folder("/work/proj");
        assert_eq!(s.preset, Preset::Folder);
        let s = l(Preset::Folder, &[("/work/proj", Rw), ("/other", Rw)], &[]);
        run("folder", &s, &[
            (Read, "/work/proj/a.txt", true),
            (Write, "/work/proj/sub/a.txt", true),
            (Delete, "/work/proj/a.txt", true),
            (List, "/work/proj", true),
            (Shell, "/work/proj/sub", true),
            (Shell, "/work", false),
            (Read, "/work/proj2/a", false),
            (Read, "/other/x", false),
            (Read, "/etc/passwd", false),
            (Net, "", false),
            (Process, "", true),
            (Read, "relative/path", false),
            (Read, "/work/proj/../secret", false), // `..` resolved first: /work/secret
            (Write, "/work/proj/.env", true),       // folder preset carries no default deny list
        ]);
        // the JSON default: no preset key means folder; missing v means 1; unknown keys are ignored
        let j = Scope::try_parse_with(r#"{"paths":[{"path":"/p","access":"rw"}],"future":{"x":1}}"#, Flavor::Posix).unwrap();
        assert_eq!(j.preset, Preset::Folder);
        assert_eq!(j.v, 1);
        assert!(ok(&j, Write, "/p/a"));
        assert!(ok(&j, Shell, "/p"));
    }

    #[test]
    fn preset_folders() {
        let s = l(Preset::Folders, &[("/a", Rw), ("/b", Ro)], &[]);
        run("folders", &s, &[
            (Write, "/a/x", true),
            (Write, "/b/x", false),
            (Read, "/b/x", true),
            (List, "/b", true),
            (Delete, "/b/x", false),
            (Read, "/c", false),
            (Read, "/a/.env", false),      // default deny list
            (Read, "/a/k/server.pem", false),
            (Read, "/a/id_rsa", false),
            (Read, "/a/.ssh/config", false),
            (Read, "/a/hub.json", false),
            (Read, "/a/notes.txt", true),
        ]);
    }

    #[test]
    fn preset_read_only() {
        let s = l(Preset::ReadOnly, &[("/a", Rw), ("/b", Ro)], &[]);
        run("read-only", &s, &[
            (Read, "/a/x", true),
            (Write, "/a/x", false),
            (Delete, "/a/x", false),
            (Write, "/b/x", false),
            (Shell, "/a", false),
            (Exec, "/a", false),
            (Read, "/c", false),
        ]);
        assert!(!s.has_tool("write", NOW));
        assert!(!s.has_tool("shell_run", NOW));
        assert!(s.has_tool("read", NOW));
    }

    #[test]
    fn preset_full_and_system_rules() {
        let s = l(Preset::Full, &[], &[]);
        run("full", &s, &[
            (Read, "/etc/hosts", true),
            (Write, "/tmp/x", true),
            (Delete, "/home/u/file", true),
            (Shell, "/", true),
            (Exec, "/usr/bin", true),
            (Net, "", false),
            (Process, "", true),
            (Read, "/proc/cpuinfo", true),
            (List, "/proc", true),
            (Read, "/proc/1/status", true),
            (Write, "/proc/self/oom_score_adj", false),
            (Write, "/proc/sys/kernel/hostname", false),
            (Read, "/proc/sys/kernel/hostname", true),
            (Read, "/proc/kcore", false),
            (Read, "/proc/self/mem", false),
            (Read, "/sys/class", true),
            (Write, "/sys/class/x", false),
            (Delete, "/sys/x", false),
            (Write, "/dev/sda", false),
            (Write, "/dev/null", true),
            (Read, "/dev/null", true),
            (Read, "/home/u/.ssh/id_ed25519", false),
            (Read, "/home/u/.ssh", false),
            (Read, "/srv/app/.env", false),
            (Read, "/srv/app/cert.pem", false),
            (Read, "/srv/app/.env.production", false),
            (Read, "/x/hub.json", false),
            (Read, "/home/u/.aws/credentials", false),
            (Read, "/home/u/.mozilla/firefox/p/logins.json", false),
        ]);
        #[cfg(unix)]
        {
            // existing raw device files (/dev char devices) are hidden even to read
            assert!(!ok(&s, Read, "/dev/zero"));
            assert!(!ok(&s, Read, "/dev/urandom"));
            assert!(ok(&s, List, "/dev"));
        }
        // unrestricted lifts the system rules and the default deny list
        let u = l(Preset::Unrestricted, &[], &[]);
        run("unrestricted", &u, &[
            (Write, "/sys/x", true),
            (Read, "/proc/kcore", true),
            (Write, "/dev/sda", true),
            (Read, "/home/u/.ssh/id_rsa", true),
            (Read, "/srv/.env", true),
            (Net, "", true),
            (Shell, "/anywhere", true),
        ]);
    }

    #[test]
    fn preset_full_ro() {
        let s = l(Preset::FullRo, &[], &[]);
        run("full-ro", &s, &[
            (Read, "/etc/hosts", true),
            (List, "/", true),
            (Write, "/tmp/x", false),
            (Delete, "/tmp/x", false),
            (Shell, "/tmp", false),
            (Read, "/x/.env", false),
        ]);
        assert!(!s.has_tool("write", NOW) && !s.has_tool("shell_run", NOW) && s.has_tool("list", NOW));
    }

    #[test]
    fn preset_deny_list() {
        let s = l(Preset::DenyList, &[], &["/srv/private", "**/*.secret", "/srv/lock*"]);
        run("deny-list (full + deny)", &s, &[
            (Read, "/srv/public/a", true),
            (Write, "/srv/public/a", true),
            (Read, "/srv/private/a", false),
            (List, "/srv/private", false),
            (Write, "/srv/public/x.secret", false),
            (Read, "/srv/public/x.secret.txt", true),
            (Read, "/srv/.env", false),
        ]);
        // deny-list over folders: deny beats allow even where a more specific allow sits inside the denied path
        let s = l(Preset::DenyList, &[("/a", Rw), ("/a/inner", Rw)], &["/a/private"]);
        run("deny-list (folders)", &s, &[
            (Read, "/a/x", true),
            (Read, "/a/private/x", false),
            (Read, "/b", false),
        ]);
        let s = l(Preset::DenyList, &[("/a", Rw), ("/a/private/open", Rw)], &["/a/private"]);
        assert!(!ok(&s, Read, "/a/private/open/f"), "deny beats a more specific allow");
    }

    #[test]
    fn preset_no_shell() {
        let s = l(Preset::NoShell, &[("/a", Rw)], &[]);
        run("no-shell", &s, &[
            (Write, "/a/x", true),
            (Shell, "/a", false),
            (Exec, "/a", false),
            (Process, "", false),
        ]);
        assert!(!s.has_tool("shell_run", NOW) && !s.has_tool("job_start", NOW) && !s.has_tool("proc_kill", NOW));
        assert!(s.has_tool("write", NOW) && s.has_tool("machine_info", NOW));
        assert!(!s.command_allowed("ls").allow);
    }

    #[test]
    fn preset_no_network() {
        let s = l(Preset::NoNetwork, &[("/a", Rw)], &[]);
        assert!(ok(&s, Write, "/a/x") && ok(&s, Shell, "/a"));
        assert!(!ok(&s, Net, ""));
        assert_eq!(s.tools.shell_network, "off");
        assert!(!s.has_tool("fetch", NOW));
        // network on in a document that did not ask for no-network
        let f = Scope::try_parse_with(r#"{"preset":"folders","paths":[{"path":"/a","access":"rw"}],"tools":{"network":{"fetch":true}}}"#, Flavor::Posix).unwrap();
        assert!(ok(&f, Net, "") && f.has_tool("fetch", NOW));
    }

    #[test]
    fn preset_custom_and_json() {
        let j = r#"{
            "v": 1, "revision": 12, "owner": "sponge", "preset": "custom", "updatedAt": "2026-10-25T20:11:00Z",
            "paths": [ {"path": "/home/sponge/Notes", "access": "ro"}, {"path": "/home/sponge/Notes/inbox", "access": "rw"} ],
            "deny": [ {"path": "/home/sponge/Notes/secrets"}, {"glob": "**/.env"} ],
            "tools": { "files": true,
                       "shell": {"enabled": true, "cwd": "confined", "allowCommands": null, "denyCommands": ["format", "mkfs", "rm -rf /"], "timeoutSec": 600, "maxOutputKB": 512},
                       "processes": {"list": true, "kill": "own-children", "start": true},
                       "network": {"fetch": false, "shellNetwork": "default"}, "containers": {"re": true}, "elevate": false },
            "limits": {"maxFileMB": 2048}, "unattended": false, "confirm": {"onWide": true}, "futureKey": [1,2]
        }"#;
        let s = Scope::try_parse_with(j, Flavor::Posix).unwrap();
        assert_eq!((s.revision, s.owner.as_str(), s.preset), (12, "sponge", Preset::Custom));
        run("custom", &s, &[
            (Read, "/home/sponge/Notes/a.md", true),
            (Write, "/home/sponge/Notes/a.md", false),
            (Write, "/home/sponge/Notes/inbox/a.md", true), // the more specific rw wins over the ro parent
            (Read, "/home/sponge/Notes/secrets/a", false),
            (Read, "/home/sponge/Notes/inbox/.env", false),
            (Read, "/home/sponge/Notes/key.pem", true), // custom has no default deny list
            (Net, "", false),
        ]);
        assert!(s.has_tool("re_run", NOW) && !s.has_tool("fetch", NOW) && !s.has_tool("elevate", NOW));
        assert!(!s.command_allowed("sudo mkfs.ext4 /dev/sda").allow);
        assert!(!s.command_allowed("rm   -rf  /").allow);
        assert!(s.command_allowed("ls -la").allow);
        // the other way round: a less specific rw does not beat a more specific ro
        let s = l(Preset::Custom, &[("/a", Rw), ("/a/ro", Ro)], &[]);
        assert!(ok(&s, Write, "/a/x") && !ok(&s, Write, "/a/ro/x") && ok(&s, Read, "/a/ro/x"));
        // paths: "*" is the whole system, with no default deny list under custom
        let s = Scope::try_parse_with(r#"{"preset":"custom","paths":[{"path":"*","access":"rw"}]}"#, Flavor::Posix).unwrap();
        assert!(ok(&s, Write, "/anything/.env") && !ok(&s, Write, "/sys/x"));
        // allowCommands
        let s = Scope::try_parse_with(r#"{"paths":[{"path":"/a","access":"rw"}],"tools":{"shell":{"allowCommands":["git","ls"]}}}"#, Flavor::Posix).unwrap();
        assert!(s.command_allowed("git status").allow && !s.command_allowed("curl x").allow);
    }

    #[test]
    fn bad_files_allow_nothing() {
        let good = r#"{"paths":[{"path":"/a","access":"rw"}]}"#;
        assert!(ok(&Scope::parse_with(good, Flavor::Posix), Read, "/a/x"));
        let cases: &[(&str, &str)] = &[
            ("not json", "bad json"),
            ("", "empty"),
            ("[]", "not an object"),
            ("null", "null"),
            (r#"{"paths":"x"}"#, "paths wrong type"),
            (r#"{"paths":[{"path":"relative","access":"rw"}]}"#, "relative path"),
            (r#"{"paths":[{"access":"rw"}]}"#, "entry without path"),
            (r#"{"paths":[{"path":"/a","access":"admin"}]}"#, "bad access"),
            (r#"{"v":"one","paths":[{"path":"/a","access":"rw"}]}"#, "v not a number"),
            (r#"{"v":2,"paths":[{"path":"/a","access":"rw"}]}"#, "newer v"),
            (r#"{"v":99}"#, "newer v 99"),
            (r#"{"paths":[{"path":"/a","access":"rw"}],"tools":{"files":"yes"}}"#, "files not bool"),
            (r#"{"paths":[{"path":"/a","access":"rw"}],"deny":[{"path":"/x","glob":"y"}]}"#, "deny with both"),
        ];
        for (j, name) in cases {
            let s = Scope::parse_with(j, Flavor::Posix);
            assert!(s.failed(), "{name}");
            for op in [Read, Write, Delete, List, Exec, Shell, Net, Process] {
                assert!(!s.allow(op, "/a/x", NOW).allow, "{name}: {}", op.name());
            }
            assert_eq!(s.tool_names(NOW), vec!["machine_info"], "{name}");
        }
        assert!(matches!(Scope::try_parse_with(r#"{"v":2}"#, Flavor::Posix), Err(ScopeError::TooNew { found: 2, supported: 1 })));
        assert!(Scope::parse_with(r#"{"v":2}"#, Flavor::Posix).error.unwrap().contains("newer app"));
        // a file that is missing
        let s = Scope::load(Path::new("/nonexistent/dir/scope.json"));
        assert!(s.failed() && !s.allow(Read, "/a", NOW).allow);
        // an empty paths list means nothing is reachable
        let s = Scope::try_parse_with(r#"{"paths":[]}"#, Flavor::Posix).unwrap();
        assert!(!ok(&s, Read, "/a") && !ok(&s, List, "/"));
        assert_eq!(s.tool_names(NOW).iter().filter(|t| **t == "read").count(), 0);
        // an older v is read, an unknown preset is custom
        let s = Scope::try_parse_with(r#"{"v":1,"preset":"future-thing","paths":[{"path":"/a","access":"rw"}]}"#, Flavor::Posix).unwrap();
        assert_eq!(s.preset, Preset::Custom);
    }

    #[test]
    fn grants_expire_on_the_passed_clock() {
        let j = r#"{"preset":"folders","paths":[{"path":"/a","access":"rw"}],"grants":[
            {"id":"g1","what":{"path":"/dl","access":"ro"},"expiresAt":"2026-10-25T22:00:00Z","note":"one-off"},
            {"id":"g2","what":{"path":"/forever","access":"rw"}},
            {"id":"g3","what":{"path":"/garbled","access":"rw"},"expiresAt":"tomorrow-ish"},
            {"id":"g4","what":{"path":"/num","access":"ro"},"expiresAt":1000}
        ]}"#;
        let s = Scope::try_parse_with(j, Flavor::Posix).unwrap();
        let exp = parse_iso8601("2026-10-25T22:00:00Z").unwrap();
        assert_eq!(exp, 1_792_965_600);
        assert!(s.allow(Read, "/dl/f", exp - 1).allow);
        assert!(!s.allow(Write, "/dl/f", exp - 1).allow, "a ro grant");
        assert!(!s.allow(Read, "/dl/f", exp).allow, "gone at the expiry second");
        assert!(!s.allow(Read, "/dl/f", exp + 3600).allow);
        assert!(s.allow(Write, "/forever/f", i64::MAX / 2).allow);
        assert!(!s.allow(Read, "/garbled/f", 0).allow, "an unreadable expiry never applies");
        assert!(s.allow(Read, "/num/f", 999).allow && !s.allow(Read, "/num/f", 1000).allow);
        assert!(s.allow(Read, "/a/f", exp + 99999).allow, "the paths entry stays");
        assert!(s.allow(Read, "/dl/f", exp - 1).reason.contains("grant g1"));
        // a grant widens only what it names; a deny still wins over it
        let s = Scope::try_parse_with(r#"{"preset":"folders","paths":[],"grants":[{"id":"g","what":{"path":"/dl","access":"rw"},"expiresAt":2000000000}]}"#, Flavor::Posix).unwrap().with_home("/h");
        assert!(s.allow(Write, "/dl/x", 1).allow && !s.allow(Read, "/dl/.env", 1).allow);
        // the tool list follows the clock too
        assert!(s.has_tool("write", 1) && !s.has_tool("write", 2_000_000_000) && !s.has_tool("read", 2_000_000_000));
    }

    #[test]
    fn iso_dates() {
        for (t, want) in [
            ("1970-01-01T00:00:00Z", Some(0)),
            ("2000-03-01T00:00:00Z", Some(951_868_800)),
            ("2024-02-29T12:00:00Z", Some(1_709_208_000)),
            ("2026-10-25T22:00:00+02:00", Some(1_792_965_600 - 7200)),
            ("2026-10-25T22:00:00.250Z", Some(1_792_965_600)),
            ("2026-13-25T22:00:00Z", None),
            ("2026-10-25", None),
            ("garbage", None),
        ] {
            assert_eq!(parse_iso8601(t), want, "{t}");
        }
    }

    #[test]
    fn tool_sets() {
        let names = |s: &Scope| s.tool_names(NOW);
        let f = Scope::default_folder("/a");
        let n = names(&f);
        for t in ["machine_info", "list", "read", "write", "delete", "mkdir", "shell_run", "job_start", "proc_list", "proc_kill"] {
            assert!(n.contains(&t), "folder has {t}");
        }
        for t in ["fetch", "elevate", "re_run"] {
            assert!(!n.contains(&t), "folder lacks {t}");
        }
        assert_eq!(names(&Scope::nothing()), vec!["machine_info"]);
        let u = l(Preset::Unrestricted, &[], &[]);
        for t in ["write", "shell_run", "fetch", "re_run", "proc_kill"] {
            assert!(u.has_tool(t, NOW), "unrestricted has {t}");
        }
        assert!(!u.has_tool("elevate", NOW), "elevation is only on when the scope says so");
        // files switched off removes the file tools
        let s = Scope::try_parse_with(r#"{"paths":[{"path":"/a","access":"rw"}],"tools":{"files":false}}"#, Flavor::Posix).unwrap();
        assert!(!s.has_tool("read", NOW) && s.has_tool("shell_run", NOW) && !ok(&s, Read, "/a/x"));
        // kill: own-children vs any
        assert!(!f.kill_any_allowed());
        let a = Scope::try_parse_with(r#"{"preset":"full","tools":{"processes":{"kill":"any"}}}"#, Flavor::Posix).unwrap();
        assert!(a.kill_any_allowed());
        let b = Scope::try_parse_with(r#"{"preset":"folders","paths":[{"path":"/a","access":"rw"}],"tools":{"processes":{"kill":"any"}}}"#, Flavor::Posix).unwrap();
        assert!(!b.kill_any_allowed(), "any needs a system-wide scope");
        // summary in words
        assert_eq!(Scope::preset_with(Flavor::Posix, Preset::Folder, &[("/a", Rw)], &[]).summary(NOW), "one folder: /a (read-write); shell (advisory)");
        assert!(l(Preset::NoShell, &[("/a", Ro)], &[]).summary(NOW).contains("no shell"));
        assert_eq!(f.selected_folder(NOW).as_deref(), Some(if cfg!(windows) { "/a" } else { "/a" }));
    }

    #[test]
    fn windows_scopes_on_any_host() {
        let s = w(Preset::Folders, &[("C:\\Work\\proj", Rw), ("D:\\Data", Ro), ("\\\\srv\\share\\docs", Rw)], &["C:\\Work\\proj\\secrets"]);
        run("windows folders", &s, &[
            (Read, "c:/work/PROJ/a.txt", true),
            (Write, "C:\\Work\\proj\\sub\\a.txt", true),
            (Write, "\\\\?\\C:\\Work\\proj\\a.txt", true),
            (Read, "C:\\Work\\proj\\secrets\\k", false),
            (Read, "c:\\work\\proj\\SECRETS", false),
            (Read, "C:\\Work\\proj2\\a", false),
            (Read, "E:\\Work\\proj\\a", false),          // another drive
            (Read, "D:\\Data\\f", true),
            (Write, "D:\\Data\\f", false),
            (Read, "\\\\srv\\share\\docs\\f", true),
            (Write, "\\\\SRV\\SHARE\\docs\\f", true),
            (Read, "\\\\srv\\share\\other\\f", false),
            (Read, "\\\\evil\\share\\docs\\f", false),
            (Read, "\\\\?\\UNC\\srv\\share\\docs\\f", true),
            (Read, "C:\\Work\\proj\\f.txt:stream", false), // alternate data stream
            (Read, "C:\\Work\\proj\\NUL", false),          // reserved name
            (Read, "C:\\Work\\proj\\com1.txt", false),
            (Read, "\\\\.\\PhysicalDrive0", false),        // device paths
            (Read, "\\\\?\\GLOBALROOT\\Device\\x", false),
            (Read, "C:\\Work\\proj\\..\\..\\Windows", false),
            (Read, "C:\\Work\\proj\\..\\other\\f", false),
            (Read, "C:\\Work\\proj\\.env", false),         // default deny list, case-insensitive
            (Read, "C:\\Work\\proj\\KEY.PEM", false),
            (Read, "C:\\People\\u\\.ssh\\id_rsa", false),
        ]);
        let f = w(Preset::Full, &[], &[]);
        run("windows full", &f, &[
            (Write, "C:\\Windows\\Temp\\x", true),
            (Read, "Z:\\any\\thing", true),
            (Read, "\\\\srv\\share\\x", false), // the whole system means local drives, not other machines
            (Read, "C:\\People\\u\\.ssh\\config", false),
            (Read, "C:\\People\\u\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Login Data", false),
            (Read, "C:\\x\\hub.json", false),
        ]);
    }
}
