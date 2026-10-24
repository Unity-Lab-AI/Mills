//! Tool output shaped before it enters the context. Every pass is lossless or reversible: ANSI and trailing
//! whitespace go (noise), runs of identical lines fold into one line with a count, a file read again unchanged becomes
//! a one-line reference to the turn that has it (with its hash), and long output is capped with a stash handle that
//! keeps the whole text on disk and brings any part back. Nothing a model may need to quote is rewritten.
//!
//! Structured shapers ([`Kind`], [`detect`], [`shape_structured`]) know `cargo` output, test runners (pytest, jest,
//! mocha, node:test, go test), `git diff`, `git status`, JSON and long directory listings: they keep every error,
//! failure and changed line and fold the rest (`[compiled 30 crates]`, `[120 tests ok]`, `[… 50 unchanged lines]`).
//! Whatever they fold is stashed first, so it stays fetchable; [`Shaped::passes`] says which pass ran.
use crate::short_hash;
use regex::Regex;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle { pub id: String, pub bytes: usize, pub lines: usize }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shaped {
    pub text: String,
    /// Set when the full output was stashed (the text carries the head, the tail and the handle).
    pub stashed: Option<Handle>,
    /// True when the text is a reference to an earlier, identical read.
    pub unchanged: bool,
    pub raw_bytes: usize,
    pub saved_bytes: usize,
    /// What the output was recognised as ([`detect`]); `Plain` when nothing structured applies.
    pub kind: Kind,
    /// The passes that changed the text, in order, so the ledger can say which one saved what: "clean", "fold",
    /// the kind's name when its structured pass ran ("cargo", "test-runner", "git-diff", "git-status", "json",
    /// "listing"), "cap", or "unchanged" (a repeated read).
    pub passes: Vec<&'static str>,
}

/// What a tool output is, for the structured shapers (TOKENS.md §3 shape): they keep what the model needs (failures,
/// errors, changed lines) and fold what it does not (passing tests, compile lines, unchanged context).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind { Plain, Cargo, TestRunner, GitDiff, GitStatus, Json, Listing }
impl Kind {
    /// The pass name used in [`Shaped::passes`].
    pub fn name(self) -> &'static str {
        match self { Kind::Plain => "plain", Kind::Cargo => "cargo", Kind::TestRunner => "test-runner", Kind::GitDiff => "git-diff", Kind::GitStatus => "git-status", Kind::Json => "json", Kind::Listing => "listing" }
    }
}

/// Shaping limits. `cap_bytes` = above this the output is stashed; `head` / `tail` = what stays in context around the
/// marker; `fold_at` = runs of at least this many identical lines fold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits { pub cap_bytes: usize, pub head_bytes: usize, pub tail_bytes: usize, pub fold_at: usize }
impl Default for Limits { fn default() -> Limits { Limits { cap_bytes: 24_000, head_bytes: 14_000, tail_bytes: 4_000, fold_at: 3 } } }

pub struct Shaper {
    limits: Limits,
    stash_dir: Option<PathBuf>,
    /// key (a tool + its target, e.g. "read:/src/main.rs") → (hash of the shaped output, turn it was shown)
    seen: HashMap<String, (String, usize)>,
    next_handle: usize,
    /// Simulation: stash writes are skipped but reported as successful (see [`Shaper::dry`]).
    dry: bool,
}

fn ansi() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07|\r").unwrap()) }

impl Shaper {
    pub fn new(limits: Limits, stash_dir: Option<&Path>) -> Shaper { Shaper { limits, stash_dir: stash_dir.map(Path::to_path_buf), seen: HashMap::new(), next_handle: 1, dry: false } }
    /// A shaper for the offline simulator: it shapes exactly as one with a stash directory would (lossy structured
    /// passes included) but writes nothing, so `fetch` finds nothing. Never use it where the model may call back.
    pub fn dry(limits: Limits) -> Shaper { Shaper { dry: true, ..Shaper::new(limits, None) } }
    pub fn limits(&self) -> Limits { self.limits }

    /// Noise off: ANSI escapes, carriage returns, trailing whitespace per line, more than two blank lines in a row.
    pub fn clean(text: &str) -> String {
        let no_ansi = ansi().replace_all(text, "");
        let mut out = String::with_capacity(no_ansi.len());
        let mut blank = 0;
        for line in no_ansi.lines() {
            let l = line.trim_end();
            if l.is_empty() { blank += 1; if blank > 2 { continue; } } else { blank = 0; }
            out.push_str(l); out.push('\n');
        }
        if !text.ends_with('\n') { let _ = out.pop(); }
        out
    }

    /// Runs of identical lines (≥ fold_at) become the line once plus a count.
    pub fn fold(text: &str, fold_at: usize) -> String {
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < lines.len() {
            let mut j = i + 1;
            while j < lines.len() && lines[j] == lines[i] { j += 1; }
            let n = j - i;
            if n >= fold_at.max(2) && lines[i].trim().len() > 0 {
                out.push_str(lines[i]);
                if !lines[i].ends_with('\n') { out.push('\n'); }
                out.push_str(&format!("[… the line above repeated {} times in a row]\n", n - 1));
            } else { for l in &lines[i..j] { out.push_str(l); } }
            i = j;
        }
        out
    }

    /// Shape one tool result. `key` names what was read (a path, a command) so a repeat can be recognised; `turn` is
    /// the current turn number (for "unchanged since turn N").
    pub fn shape(&mut self, key: Option<&str>, raw: &str, turn: usize) -> Shaped { self.shape_tool("", key, raw, turn) }

    /// [`Shaper::shape`] with the tool's name, so the structured shapers can tell a build log from a file read. The
    /// order: clean → fold → repeat check → [`detect`] → structured pass → cap. File reads (tool "read…", key
    /// "read:…") are never restructured: the model quotes them. A structured pass that drops text (every one except
    /// a JSON re-print without trimming) runs ONLY when the whole cleaned output is first stashed on disk, so what it
    /// folds is always fetchable; with no stash directory (or a failed write) that pass is skipped.
    pub fn shape_tool(&mut self, tool: &str, key: Option<&str>, raw: &str, turn: usize) -> Shaped {
        let raw_bytes = raw.len();
        let mut passes: Vec<&'static str> = vec![];
        let c1 = Self::clean(raw);
        if c1 != raw { passes.push("clean"); }
        let cleaned = Self::fold(&c1, self.limits.fold_at);
        if cleaned != c1 { passes.push("fold"); }
        let hash = short_hash(&cleaned);
        if let Some(k) = key {
            if let Some((h, t)) = self.seen.get(k) {
                if *h == hash {
                    let text = format!("[unchanged since turn {t}: {k} ({} bytes, {} lines, sha256 {hash}); it is already in the conversation]", cleaned.len(), cleaned.lines().count());
                    let saved = raw_bytes.saturating_sub(text.len());
                    passes.push("unchanged");
                    return Shaped { text, stashed: None, unchanged: true, raw_bytes, saved_bytes: saved, kind: Kind::Plain, passes };
                }
            }
            self.seen.insert(k.to_string(), (hash.clone(), turn));
        }
        let kind = detect(tool, key, &cleaned);
        let mut body = cleaned.clone();
        let mut handle: Option<Handle> = None;
        if kind != Kind::Plain {
            if let Some((s, lossy)) = structured(kind, &cleaned, &self.limits) {
                if !lossy {
                    body = s;
                    passes.push(kind.name());
                } else if s.len() + 200 <= cleaned.len() {
                    if let (h, true) = self.write_stash(&cleaned, turn) {
                        body = format!("{}\n[… {} output folded above; the whole output is kept as {} ({} bytes, {} lines): context_fetch(\"{}\", from_line, to_line) brings any part back]", s.trim_end(), kind.name(), h.id, h.bytes, h.lines, h.id);
                        handle = Some(h);
                        passes.push(kind.name());
                    }
                }
            }
        }
        if body.len() <= self.limits.cap_bytes {
            let saved = raw_bytes.saturating_sub(body.len());
            return Shaped { text: body, stashed: handle, unchanged: false, raw_bytes, saved_bytes: saved, kind, passes };
        }
        // cap: head + marker + tail, the whole thing stashed (the same stash the structured pass made, if any)
        let h = match handle { Some(h) => h, None => self.write_stash(&cleaned, turn).0 };
        let text = self.cap_text(&body, &h);
        passes.push("cap");
        let saved = raw_bytes.saturating_sub(text.len());
        Shaped { text, stashed: Some(h), unchanged: false, raw_bytes, saved_bytes: saved, kind, passes }
    }

    /// Hands out a handle and writes `full` under it; the bool says the file was written (false: no stash directory
    /// or the write failed, the handle is then only a name).
    fn write_stash(&mut self, full: &str, turn: usize) -> (Handle, bool) {
        let handle = Handle { id: format!("stash-{}-{}", turn, self.next_handle), bytes: full.len(), lines: full.lines().count() };
        self.next_handle += 1;
        let mut ok = self.dry;
        if let Some(dir) = &self.stash_dir { let _ = std::fs::create_dir_all(dir); ok = std::fs::write(dir.join(format!("{}.txt", handle.id)), full).is_ok(); }
        (handle, ok)
    }

    /// head + marker + tail of `t`, the marker naming the stash that holds the whole output.
    fn cap_text(&self, t: &str, handle: &Handle) -> String {
        let head_end = Self::line_boundary(t, self.limits.head_bytes.min(t.len()));
        let tail_start = Self::line_boundary(t, t.len().saturating_sub(self.limits.tail_bytes).max(head_end));
        let hidden = tail_start.saturating_sub(head_end);
        format!("{}\n[… {} bytes ({} lines) left out here; the whole output is kept as {} ({} bytes, {} lines): context_fetch(\"{}\", from_line, to_line) brings any part back]\n{}",
            t[..head_end].trim_end(), hidden, t[head_end..tail_start].lines().count(), handle.id, handle.bytes, handle.lines, handle.id, t[tail_start..].trim_end())
    }

    fn line_boundary(t: &str, at: usize) -> usize {
        if at >= t.len() { return t.len(); }
        let mut i = at; while i > 0 && !t.is_char_boundary(i) { i -= 1; }
        match t[..i].rfind('\n') { Some(n) if n > 0 => n + 1, _ => i }
    }

    /// Lines `from..=to` (1-based) of a stashed output; None when there is no such stash.
    pub fn fetch(&self, handle: &str, from: usize, to: usize) -> Option<String> {
        if !handle.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') { return None; }
        let text = std::fs::read_to_string(self.stash_dir.as_ref()?.join(format!("{handle}.txt"))).ok()?;
        let from = from.max(1); let to = to.max(from);
        Some(text.lines().skip(from - 1).take(to - from + 1).collect::<Vec<_>>().join("\n"))
    }
    /// Forget what was seen (a new session).
    pub fn reset(&mut self) { self.seen.clear(); }
}

// ---- structured shapers -------------------------------------------------------------------------------------------

fn re_status_short() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"^(?:[MADRCU?!][ MADRCU?!]|[ ][MADRCU?!]) \S").unwrap()) }
fn re_summary() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b\d+ (passed|failed)\b").unwrap()) }

/// A command hint without its tool prefix: "bash:cargo test" → "cargo test"; a bare command stays whole.
fn command_of(hint: &str) -> &str {
    match hint.split_once(':') { Some((p, rest)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') => rest.trim_start(), _ => hint.trim_start() }
}

fn is_cargo_build_line(t: &str) -> bool { t.starts_with("Compiling ") && t.contains(" v") }
fn is_cargo_ok_test(t: &str) -> bool { t.starts_with("test ") && t.ends_with(" ... ok") }

fn is_pass_line(t: &str) -> bool {
    t.starts_with("PASS ") || t.starts_with('✓') || t.starts_with('✔') || t.starts_with("--- PASS")
        || (t.starts_with("ok ") && t.split_whitespace().nth(1).map_or(false, |w| w.chars().all(|c| c.is_ascii_digit())))
        || (t.contains("::") && (t.ends_with(" PASSED") || t.contains(" PASSED ")))
}
fn is_run_noise(t: &str) -> bool { t.starts_with("=== RUN") || t.starts_with("=== PAUSE") || t.starts_with("=== CONT") }

/// What `raw` is, from the tool name, a hint (the shaper key: "bash:cargo test", "read:/src/a.rs") and the text itself.
/// File reads are always `Plain` (the model quotes them). Order: git diff, cargo, test runner, git status, JSON, listing.
pub fn detect(tool: &str, input_hint: Option<&str>, raw: &str) -> Kind {
    let t = tool.to_ascii_lowercase();
    let hint = input_hint.unwrap_or("").trim();
    if t.contains("read") || t.contains("view") || t == "cat" || hint.starts_with("read:") { return Kind::Plain; }
    let cmd = command_of(hint).to_ascii_lowercase();
    let lines: Vec<&str> = raw.lines().collect();
    if lines.iter().any(|l| l.starts_with("diff --git ")) || (lines.iter().any(|l| l.starts_with("@@ ")) && lines.iter().any(|l| l.starts_with("+++ ")) && lines.iter().any(|l| l.starts_with("--- "))) { return Kind::GitDiff; }
    let (mut cargo, mut cargo_weak, mut arrow, mut test_strong, mut test_weak, mut pathlike, mut status_short, mut nonempty) = (0, 0, false, 0, 0, 0, 0, 0);
    let mut branch = false;
    for l in &lines {
        let t = l.trim_start();
        if t.is_empty() { continue; }
        nonempty += 1;
        if is_cargo_build_line(t) || t.starts_with("error[E") || t.starts_with("test result:") || t.starts_with("error: could not compile") || (t.starts_with("Finished ") && t.contains("target(s)")) || (t.starts_with("Running ") && t.contains("target/")) { cargo += 1; }
        if t.starts_with("warning:") { cargo_weak += 1; }
        if t.starts_with("--> ") { arrow = true; }
        if t.contains("= FAILURES =") || t.contains("test session starts") || t.starts_with("Test Suites:") || (t.starts_with("Tests:") && t.contains("total")) || t.starts_with("# pass ") || t.starts_with("# fail ") || t.starts_with("--- FAIL") || t.starts_with("--- PASS") || re_summary().is_match(t) { test_strong += 1; }
        if is_pass_line(t) || t.starts_with("FAIL ") || t.starts_with('✕') || t.starts_with('✗') || t.starts_with("not ok ") { test_weak += 1; }
        if l.starts_with("On branch ") || l.starts_with("HEAD detached") { branch = true; }
        if re_status_short().is_match(l) { status_short += 1; }
        if !t.contains(char::is_whitespace) && (t.contains('/') || t.contains('.')) { pathlike += 1; }
    }
    if cargo > 0 || (cargo_weak > 0 && (arrow || cmd.starts_with("cargo"))) { return Kind::Cargo; }
    if test_strong > 0 || test_weak >= 3 { return Kind::TestRunner; }
    if branch || (nonempty >= 3 && status_short * 10 >= nonempty * 8) { return Kind::GitStatus; }
    let trimmed = raw.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('[')) && trimmed.len() > 2 && serde_json::from_str::<serde_json::Value>(trimmed.trim_end()).is_ok() { return Kind::Json; }
    let lister = cmd.starts_with("ls") || cmd.starts_with("find") || cmd.starts_with("fd ") || cmd.starts_with("git ls-files") || cmd.starts_with("tree");
    let nospace = lines.iter().filter(|l| !l.trim().is_empty() && !l.trim().contains(char::is_whitespace)).count();
    if nonempty >= 20 && nospace * 10 >= nonempty * 9 && (pathlike * 10 >= nonempty * 9 || lister) { return Kind::Listing; }
    Kind::Plain
}

/// The structured pass for `kind` on already-cleaned text; None when it would not make the text shorter. Everything
/// but a JSON re-print without trimming is LOSSY (folded passes, unchanged context, entries past the first 50): use it
/// through [`Shaper::shape_tool`], which stashes the whole output first and puts the handle in the text. What always
/// survives: every compiler error and warning block, every failing test with its message, every changed diff line.
pub fn shape_structured(kind: Kind, raw: &str, limits: &Limits) -> Option<String> { structured(kind, raw, limits).map(|(s, _)| s) }

/// (text, lossy). Lossy = something is only in the stash afterwards.
fn structured(kind: Kind, raw: &str, limits: &Limits) -> Option<(String, bool)> {
    let (out, lossy) = match kind {
        Kind::Plain => return None,
        Kind::Cargo => (fold_cargo(raw)?, true),
        Kind::TestRunner => (fold_tests(raw)?, true),
        Kind::GitDiff => (fold_diff(raw)?, true),
        Kind::GitStatus => (fold_status(raw)?, true),
        Kind::Json => json_pass(raw, limits)?,
        Kind::Listing => (fold_listing(raw)?, true),
    };
    if out.len() < raw.len() { Some((out, lossy)) } else { None }
}

/// Runs of `Compiling x vN` → `[compiled N crates]`, of `Downloaded`/`Downloading` → `[downloaded N crates]`, of
/// `test … ok` → `[N tests ok]` (runs of 2 or more). Every other line stays, so each `error` / `warning` block with its
/// `-->` and `= note` lines, `FAILED` tests, panic messages, the `failures:` section, `test result:` and `Finished` /
/// `could not compile` survive untouched.
fn fold_cargo(raw: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let class = |t: &str| -> u8 { if is_cargo_build_line(t) { 1 } else if t.starts_with("Downloaded ") || t.starts_with("Downloading ") { 2 } else if is_cargo_ok_test(t) { 3 } else { 0 } };
    let (mut out, mut changed, mut i) = (String::with_capacity(raw.len() / 2), false, 0);
    while i < lines.len() {
        let c = class(lines[i].trim_start());
        let mut j = i + 1;
        if c != 0 { while j < lines.len() && class(lines[j].trim_start()) == c { j += 1; } }
        let n = j - i;
        if c != 0 && n >= 2 {
            changed = true;
            let ind = &lines[i][..lines[i].len() - lines[i].trim_start().len()];
            match c { 1 => out.push_str(&format!("{ind}[compiled {n} crates]\n")), 2 => out.push_str(&format!("{ind}[downloaded {n} crates]\n")), _ => out.push_str(&format!("{ind}[{n} tests ok]\n")) }
        } else { for l in &lines[i..j] { out.push_str(l); out.push('\n'); } }
        i = j;
    }
    if changed { Some(out) } else { None }
}

/// pytest / jest / mocha / node:test / go test: runs of passing lines (`PASS file`, `✓ name`, `ok 3 - name`, `--- PASS`,
/// `file::test PASSED`, with `=== RUN` noise inside the run) → `[N passed]` (runs with 2 or more passes). Failures,
/// their messages, suite names around them and the summary line are untouched.
fn fold_tests(raw: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let (mut out, mut changed, mut i) = (String::with_capacity(raw.len() / 2), false, 0);
    while i < lines.len() {
        let t = lines[i].trim_start();
        if is_pass_line(t) || is_run_noise(t) {
            let mut j = i;
            let mut passes = 0;
            while j < lines.len() { let u = lines[j].trim_start(); if is_pass_line(u) { passes += 1; } else if !is_run_noise(u) { break; } j += 1; }
            if passes >= 2 { out.push_str(&format!("[{passes} passed]\n")); changed = true; } else { for l in &lines[i..j] { out.push_str(l); out.push('\n'); } }
            i = j;
        } else { out.push_str(lines[i]); out.push('\n'); i += 1; }
    }
    if changed { Some(out) } else { None }
}

fn flush_hunk(out: &mut Vec<String>, header: &mut Option<usize>, rem: &mut String, add: &mut String) {
    if let Some(h) = header.take() {
        if !rem.is_empty() && *rem == *add { out.insert(h + 1, "[whitespace-only hunk: the changed lines differ only in whitespace]".into()); }
    }
    rem.clear(); add.clear();
}

/// Headers (`diff --git`, `index`, `---`, `+++`, `@@`, mode / rename lines) and every `+` / `-` line stay; a run of more
/// than 3 unchanged context lines inside a hunk → `[… N unchanged lines]`; a hunk whose removed and added lines are
/// equal once whitespace is ignored gets a note line. Only applies when a run was folded.
fn fold_diff(raw: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let (mut out, mut changed, mut in_hunk, mut i): (Vec<String>, bool, bool, usize) = (vec![], false, false, 0);
    let (mut header, mut rem, mut add) = (None::<usize>, String::new(), String::new());
    let strip = |l: &str| l.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    while i < lines.len() {
        let l = lines[i];
        if l.starts_with("diff --git ") { flush_hunk(&mut out, &mut header, &mut rem, &mut add); in_hunk = false; out.push(l.into()); i += 1; }
        else if l.starts_with("@@") { flush_hunk(&mut out, &mut header, &mut rem, &mut add); in_hunk = true; header = Some(out.len()); out.push(l.into()); i += 1; }
        else if in_hunk && (l.starts_with('+') || l.starts_with('-')) { if l.starts_with('+') { add.push_str(&strip(&l[1..])); } else { rem.push_str(&strip(&l[1..])); } out.push(l.into()); i += 1; }
        else if in_hunk && (l.is_empty() || l.starts_with(' ')) {
            let mut j = i;
            while j < lines.len() && (lines[j].is_empty() || lines[j].starts_with(' ')) { j += 1; }
            let n = j - i;
            if n > 3 { out.push(format!("[… {n} unchanged lines]")); changed = true; } else { for l in &lines[i..j] { out.push((*l).into()); } }
            i = j;
        }
        else { if in_hunk && !l.starts_with('\\') { flush_hunk(&mut out, &mut header, &mut rem, &mut add); in_hunk = false; } out.push(l.into()); i += 1; }
    }
    flush_hunk(&mut out, &mut header, &mut rem, &mut add);
    if changed { Some(out.join("\n") + "\n") } else { None }
}

/// Over 40 entries (short `XY path` lines or the long form's tab-indented lines): the entries become one line per
/// directory with a count and a tally per status; the other lines (branch, hints) stay. At 40 or fewer: unchanged.
fn fold_status(raw: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let entry = |l: &str| -> Option<(String, String)> {
        if re_status_short().is_match(l) { return Some((l[..2].trim().to_string(), l[3..].trim().to_string())); }
        if l.starts_with('\t') {
            let t = l.trim();
            if let Some((label, path)) = t.split_once(':') { if !label.contains('/') && !label.contains(' ') || label == "new file" { return Some((label.to_string(), path.trim().to_string())); } }
            return Some(("untracked".into(), t.to_string()));
        }
        None
    };
    let entries: Vec<Option<(String, String)>> = lines.iter().map(|l| entry(l)).collect();
    if entries.iter().flatten().count() <= 40 { return None; }
    let mut dirs: BTreeMap<String, (usize, BTreeMap<String, usize>)> = BTreeMap::new();
    for (st, path) in entries.iter().flatten() {
        let p = path.rsplit(" -> ").next().unwrap_or(path).trim_end_matches('/');
        let dir = p.rsplit_once('/').map(|x| x.0).unwrap_or(".").to_string();
        let e = dirs.entry(dir).or_default();
        e.0 += 1; *e.1.entry(st.clone()).or_default() += 1;
    }
    let total = entries.iter().flatten().count();
    let mut out = String::new();
    let mut emitted = false;
    for (l, e) in lines.iter().zip(&entries) {
        if e.is_some() {
            if !emitted {
                emitted = true;
                out.push_str(&format!("[{total} entries in {} directories]\n", dirs.len()));
                for (d, (n, st)) in &dirs { out.push_str(&format!("{d}/ {n} ({})\n", st.iter().map(|(s, k)| format!("{s} {k}")).collect::<Vec<_>>().join(", "))); }
            }
        } else { out.push_str(l); out.push('\n'); }
    }
    Some(out)
}

/// A document over `cap_bytes / 4` is re-printed compactly (lossless, key order kept); arrays of more than 20 elements
/// are cut to the first 10, a `"… N more"` element and the last 3 (lossy). The bool says whether anything was cut.
fn json_pass(raw: &str, limits: &Limits) -> Option<(String, bool)> {
    if raw.len() <= limits.cap_bytes / 4 { return None; }
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let mut cut = false;
    let v = trim_arrays(v, &mut cut);
    Some((serde_json::to_string(&v).ok()?, cut))
}
fn trim_arrays(v: serde_json::Value, cut: &mut bool) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::Array(mut a) => {
            if a.len() > 20 {
                *cut = true;
                let n = a.len();
                let tail = a.split_off(n - 3);
                a.truncate(10);
                a.push(Value::String(format!("… {} more", n - 13)));
                a.extend(tail);
            }
            Value::Array(a.into_iter().map(|x| trim_arrays(x, cut)).collect())
        }
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, x)| (k, trim_arrays(x, cut))).collect()),
        other => other,
    }
}

/// More than 200 entries: a header, the first 50 entries, then the rest as per-directory counts (the biggest 40).
fn fold_listing(raw: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= 200 { return None; }
    let mut dirs: BTreeMap<String, usize> = BTreeMap::new();
    for l in &lines {
        let p = l.trim().trim_start_matches("./").trim_end_matches('/');
        *dirs.entry(p.rsplit_once('/').map(|x| x.0).unwrap_or(".").to_string()).or_default() += 1;
    }
    let mut by_count: Vec<(&String, &usize)> = dirs.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let shown = by_count.iter().take(40).map(|(d, n)| format!("{d}/ {n}")).collect::<Vec<_>>().join(", ");
    let more = by_count.len().saturating_sub(40);
    let mut out = format!("[listing: {} entries in {} directories; the first 50 are shown]\n", lines.len(), dirs.len());
    for l in &lines[..50] { out.push_str(l); out.push('\n'); }
    out.push_str(&format!("[… {} more entries; per directory: {shown}{}]\n", lines.len() - 50, if more > 0 { format!(", and {more} more directories") } else { String::new() }));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_strips_noise_but_keeps_content() {
        let raw = "\x1b[32mok\x1b[0m   \r\nline two\t \n\n\n\n\nend";
        assert_eq!(Shaper::clean(raw), "ok\nline two\n\n\nend");
        assert_eq!(Shaper::clean("a\n"), "a\n");
    }

    #[test]
    fn fold_collapses_runs_of_identical_lines() {
        let raw = "x\nsame\nsame\nsame\nsame\ny\nz\nz\n";
        let f = Shaper::fold(raw, 3);
        assert_eq!(f, "x\nsame\n[… the line above repeated 3 times in a row]\ny\nz\nz\n");
        assert_eq!(Shaper::fold("a\na\n", 3), "a\na\n");
    }

    #[test]
    fn a_repeated_unchanged_read_becomes_a_reference_and_a_changed_one_does_not() {
        let mut s = Shaper::new(Limits::default(), None);
        let first = s.shape(Some("read:/src/main.rs"), "fn main() {}\n", 1);
        assert!(!first.unchanged); assert_eq!(first.text, "fn main() {}\n");
        let again = s.shape(Some("read:/src/main.rs"), "fn main() {}\n", 4);
        assert!(again.unchanged); assert!(again.text.contains("unchanged since turn 1") && again.text.contains("/src/main.rs"));
        let changed = s.shape(Some("read:/src/main.rs"), "fn main() { run() }\n", 5);
        assert!(!changed.unchanged);
        let later = s.shape(Some("read:/src/main.rs"), "fn main() { run() }\n", 6);
        assert!(later.unchanged && later.text.contains("since turn 5"));
        s.reset();
        assert!(!s.shape(Some("read:/src/main.rs"), "fn main() { run() }\n", 7).unchanged);
    }

    #[test]
    fn long_output_is_capped_with_a_stash_that_brings_lines_back() {
        let dir = std::env::temp_dir().join(format!("pgm-stash-{}", std::process::id()));
        let mut s = Shaper::new(Limits { cap_bytes: 500, head_bytes: 200, tail_bytes: 100, fold_at: 3 }, Some(&dir));
        let raw: String = (1..=100).map(|i| format!("line number {i:03} with some padding text\n")).collect();
        let out = s.shape(Some("bash:ls"), &raw, 2);
        let h = out.stashed.clone().expect("stashed");
        assert_eq!(h.lines, 100); assert!(out.text.len() < raw.len()); assert!(out.saved_bytes > 0);
        assert!(out.text.starts_with("line number 001")); assert!(out.text.trim_end().ends_with("line number 100 with some padding text"));
        assert!(out.text.contains(&format!("context_fetch(\"{}\"", h.id)));
        assert_eq!(s.fetch(&h.id, 50, 52).unwrap(), "line number 050 with some padding text\nline number 051 with some padding text\nline number 052 with some padding text");
        assert!(s.fetch("../etc/passwd", 1, 2).is_none());
        assert!(s.fetch("nope", 1, 2).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- structured shapers: realistic samples ----

    fn tmp(name: &str) -> PathBuf { std::env::temp_dir().join(format!("pgm-shape-{name}-{}", std::process::id())) }

    const WARN1: &str = "warning: unused variable: `idx`\n  --> src/parse.rs:41:13\n   |\n41 |         let idx = 3;\n   |             ^^^ help: if this is intentional, prefix it with an underscore: `_idx`\n   |\n   = note: `#[warn(unused_variables)]` on by default\n";
    const WARN2: &str = "warning: function `old_helper` is never used\n  --> src/util.rs:9:4\n   |\n9  | fn old_helper() {}\n   |    ^^^^^^^^^^\n";
    const ERR1: &str = "error[E0308]: mismatched types\n  --> src/main.rs:17:20\n   |\n17 |     let n: u32 = \"five\";\n   |            ---   ^^^^^^ expected `u32`, found `&str`\n   |            |\n   |            expected due to this\n";

    fn cargo_build() -> String {
        let mut s = String::new();
        for i in 0..30 { s.push_str(&format!("   Compiling crate{i} v1.{i}.0\n")); if i == 14 { s.push_str(WARN1); s.push('\n'); } }
        s.push_str("   Compiling app v0.1.0 (/work/app)\n");
        s.push_str(WARN2); s.push('\n');
        s.push_str(ERR1); s.push('\n');
        s.push_str("error: could not compile `app` (bin \"app\") due to 1 previous error; 2 warnings emitted\n");
        s
    }

    fn cargo_test() -> String {
        let mut s = String::from("   Compiling app v0.1.0 (/work/app)\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 3.21s\n     Running unittests src/lib.rs (target/debug/deps/app-1a2b3c)\n\nrunning 122 tests\n");
        for i in 0..122 {
            if i == 40 { s.push_str("test parse::tests::rejects_empty ... FAILED\n"); }
            else if i == 90 { s.push_str("test net::tests::retries_twice ... FAILED\n"); }
            else { s.push_str(&format!("test mod{}::tests::case_{i} ... ok\n", i % 7)); }
        }
        s.push_str("\nfailures:\n\n---- parse::tests::rejects_empty stdout ----\nthread 'parse::tests::rejects_empty' panicked at src/parse.rs:88:9:\nassertion `left == right` failed: empty input must be an error\n  left: Ok(())\n right: Err(Empty)\n\n---- net::tests::retries_twice stdout ----\nthread 'net::tests::retries_twice' panicked at src/net.rs:120:9:\nexpected 2 retries, saw 1\n\nfailures:\n    net::tests::retries_twice\n    parse::tests::rejects_empty\n\ntest result: FAILED. 120 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.41s\n");
        s
    }

    fn jest() -> String {
        let mut s = String::new();
        for i in 0..8 { s.push_str(&format!("PASS src/mod{i}.test.js\n  Mod{i}\n    ✓ does the first thing ({i} ms)\n    ✓ does the second thing (1 ms)\n    ✓ does the third thing (2 ms)\n")); }
        s.push_str("FAIL src/cart.test.js\n  Cart\n    ✓ starts empty (1 ms)\n    ✕ totals with tax (4 ms)\n\n  ● Cart › totals with tax\n\n    expect(received).toBe(expected) // Object.is equality\n\n    Expected: 108\n    Received: 100\n\n      12 |   const c = new Cart();\n      13 |   c.add(100);\n    > 14 |   expect(c.total()).toBe(108);\n\nTest Suites: 1 failed, 8 passed, 9 total\nTests:       1 failed, 25 passed, 26 total\n");
        s
    }

    fn diff_long_middle() -> String {
        let mut s = String::from("diff --git a/src/lib.rs b/src/lib.rs\nindex 83db48f..bf2a3c1 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,60 +1,60 @@\n fn first() {}\n-fn alpha() { 1 }\n+fn alpha() { 2 }\n");
        for i in 0..50 { s.push_str(&format!(" fn untouched_{i}() {{}}\n")); }
        s.push_str("-    let x = 1;\n+    let x = 2;\n fn tail() {}\n");
        s.push_str("diff --git a/src/fmt.rs b/src/fmt.rs\nindex 1111111..2222222 100644\n--- a/src/fmt.rs\n+++ b/src/fmt.rs\n@@ -3,5 +3,5 @@\n-  let a = 1;\n+    let a = 1;\n");
        for i in 0..6 { s.push_str(&format!(" // keep {i}\n")); }
        s
    }

    fn listing(n: usize) -> String { (0..n).map(|i| format!("src/{}/file_{i:03}.rs\n", ["alpha", "beta", "gamma"][i % 3])).collect() }

    fn status(n: usize) -> String {
        let mut s = String::from("On branch work/sponge\nChanges not staged for commit:\n");
        for i in 0..n { s.push_str(&format!("\tmodified:   crates/{}/src/f{i}.rs\n", ["a", "b", "c"][i % 3])); }
        s
    }

    fn big_json_array() -> String {
        let items: Vec<String> = (0..500).map(|i| format!("  {{\n    \"id\": {i},\n    \"name\": \"item number {i}\",\n    \"tags\": [\"x\", \"y\"]\n  }}")).collect();
        format!("[\n{}\n]\n", items.join(",\n"))
    }

    #[test]
    fn detect_recognises_each_kind_and_leaves_file_reads_alone() {
        assert_eq!(detect("bash", Some("bash:cargo build"), &cargo_build()), Kind::Cargo);
        assert_eq!(detect("bash", None, &cargo_test()), Kind::Cargo);
        assert_eq!(detect("bash", Some("bash:npx jest"), &jest()), Kind::TestRunner);
        assert_eq!(detect("bash", None, "===== test session starts =====\ncollected 3 items\n\ntests/test_a.py::test_x PASSED\n===== 1 failed, 2 passed in 0.1s ====="), Kind::TestRunner);
        assert_eq!(detect("bash", None, &diff_long_middle()), Kind::GitDiff);
        assert_eq!(detect("bash", None, &status(5)), Kind::GitStatus);
        assert_eq!(detect("bash", None, &(0..50).map(|i| format!(" M src/f{i}.rs\n")).collect::<String>()), Kind::GitStatus);
        assert_eq!(detect("bash", None, &big_json_array()), Kind::Json);
        assert_eq!(detect("bash", Some("bash:find . -name *.rs"), &listing(300)), Kind::Listing);
        assert_eq!(detect("bash", None, &listing(300)), Kind::Listing);
        assert_eq!(detect("bash", None, "plain words\nmore words\n"), Kind::Plain);
        assert_eq!(detect("bash", None, "{ not json"), Kind::Plain);
        assert_eq!(detect("bash", None, "[x] a checklist item\n"), Kind::Plain);
        // a file read is never restructured, however much it looks like a build log or JSON
        assert_eq!(detect("Read", Some("read:/w/log.txt"), &cargo_build()), Kind::Plain);
        assert_eq!(detect("bash", Some("read:/w/data.json"), &big_json_array()), Kind::Plain);
    }

    #[test]
    fn cargo_build_keeps_every_error_and_warning_and_folds_the_compile_lines() {
        let raw = cargo_build();
        let out = shape_structured(Kind::Cargo, &raw, &Limits::default()).expect("worth folding");
        for block in [WARN1, WARN2, ERR1] { assert!(out.contains(block.trim_end()), "block lost: {block}"); }
        assert!(out.contains("error: could not compile `app`"));
        assert!(out.contains("[compiled 15 crates]") && out.contains("[compiled 16 crates]"), "{out}");
        assert!(!out.contains("Compiling crate"));
        assert!(out.len() < raw.len() / 2 + 400);
    }

    #[test]
    fn cargo_test_keeps_failures_and_folds_the_passes() {
        let raw = cargo_test();
        let out = shape_structured(Kind::Cargo, &raw, &Limits::default()).unwrap();
        for must in ["rejects_empty ... FAILED", "retries_twice ... FAILED", "empty input must be an error", "expected 2 retries, saw 1", "test result: FAILED. 120 passed; 2 failed", "Finished `test` profile", "left: Ok(())"] { assert!(out.contains(must), "lost: {must}\n{out}"); }
        let re = Regex::new(r"\[(\d+) tests ok\]").unwrap();
        let folded: usize = re.captures_iter(&out).map(|c| c[1].parse::<usize>().unwrap()).sum();
        assert_eq!(folded, 120, "{out}");
        assert!(!out.contains("::tests::case_"));
    }

    #[test]
    fn a_jest_run_keeps_the_failure_and_folds_the_passes() {
        let out = shape_structured(Kind::TestRunner, &jest(), &Limits::default()).unwrap();
        for must in ["FAIL src/cart.test.js", "✕ totals with tax", "● Cart › totals with tax", "Expected: 108", "Received: 100", "Tests:       1 failed, 25 passed, 26 total"] { assert!(out.contains(must), "lost: {must}"); }
        assert!(!out.contains("PASS src/mod3.test.js"));
        assert!(Regex::new(r"\[\d+ passed\]").unwrap().is_match(&out));
        // go test: the RUN noise folds with the passes, a failure stays
        let go = "=== RUN   TestA\n--- PASS: TestA (0.00s)\n=== RUN   TestB\n--- PASS: TestB (0.00s)\n=== RUN   TestC\n    c_test.go:12: boom\n--- FAIL: TestC (0.00s)\nFAIL\nFAIL\tpkg/c\t0.004s\n";
        assert_eq!(detect("bash", None, go), Kind::TestRunner);
        let g = shape_structured(Kind::TestRunner, go, &Limits::default());
        assert!(g.is_none() || { let g = g.unwrap(); g.contains("c_test.go:12: boom") && g.contains("--- FAIL: TestC") }, "tiny output may stay as is");
    }

    #[test]
    fn a_diff_keeps_headers_and_changed_lines_and_folds_the_unchanged_middle() {
        let raw = diff_long_middle();
        let out = shape_structured(Kind::GitDiff, &raw, &Limits::default()).unwrap();
        for l in raw.lines().filter(|l| l.starts_with('+') || l.starts_with('-') || l.starts_with("diff --git") || l.starts_with("@@")) { assert!(out.lines().any(|o| o == l), "lost: {l}"); }
        assert!(out.contains("[… 50 unchanged lines]"));
        assert!(out.contains("[… 6 unchanged lines]"));
        assert!(!out.contains("untouched_7"));
        assert!(out.contains(" fn first() {}") && out.contains(" fn tail() {}"), "runs of 3 or fewer stay");
        let ws = out.lines().position(|l| l.starts_with("[whitespace-only hunk")).expect("whitespace-only hunk noted");
        assert!(out.lines().nth(ws - 1).unwrap().starts_with("@@ -3,5"));
    }

    #[test]
    fn a_long_git_status_is_grouped_by_directory_and_a_short_one_is_left_alone() {
        let out = shape_structured(Kind::GitStatus, &status(60), &Limits::default()).unwrap();
        assert!(out.contains("On branch work/sponge"));
        assert!(out.contains("[60 entries in 3 directories]"));
        assert!(out.contains("crates/a/src/ 20 (modified 20)"), "{out}");
        assert!(!out.contains("f17.rs"));
        assert!(shape_structured(Kind::GitStatus, &status(40), &Limits::default()).is_none());
        let short = (0..50).map(|i| format!(" M src/{}/f{i}.rs\n", i % 2)).collect::<String>() + "?? notes.txt\n";
        let s = shape_structured(Kind::GitStatus, &short, &Limits::default()).unwrap();
        assert!(s.contains("src/0/ 25 (M 25)") && s.contains("./ 1 (?? 1)"), "{s}");
    }

    #[test]
    fn a_big_listing_becomes_counts_plus_the_first_fifty() {
        let raw = listing(300);
        let out = shape_structured(Kind::Listing, &raw, &Limits::default()).unwrap();
        assert!(out.contains("[listing: 300 entries in 3 directories"));
        assert!(out.contains("src/alpha/file_000.rs") && out.contains("file_049.rs") && !out.contains("file_050.rs"));
        assert!(out.contains("src/alpha/ 100") && out.contains("250 more entries"));
        assert!(shape_structured(Kind::Listing, &listing(200), &Limits::default()).is_none());
    }

    #[test]
    fn a_json_array_is_trimmed_only_with_the_stash_and_the_stash_has_it_all() {
        let dir = tmp("json");
        let mut s = Shaper::new(Limits::default(), Some(&dir));
        let raw = big_json_array();
        let out = s.shape_tool("bash", Some("bash:curl api"), &raw, 3);
        assert_eq!(out.kind, Kind::Json);
        let h = out.stashed.clone().expect("lossy pass has a handle");
        assert!(out.passes.contains(&"json"));
        assert!(out.text.contains("\"… 487 more\"") && out.text.contains("\"id\":0") && out.text.contains("\"id\":499") && !out.text.contains("\"id\":200,"));
        assert!(out.text.contains(&format!("context_fetch(\"{}\"", h.id)));
        assert!(out.saved_bytes > raw.len() / 2);
        let back = s.fetch(&h.id, 1, 100_000).unwrap();
        assert!(back.contains("\"id\": 250") && back.lines().count() == h.lines);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_json_reprint_without_trimming_is_lossless_and_needs_no_handle() {
        let items: Vec<String> = (0..15).map(|i| format!("  {{\n    \"id\": {i},\n    \"text\": \"{}\"\n  }}", "lorem ipsum ".repeat(50))).collect();
        let raw = format!("[\n{}\n]\n", items.join(",\n"));
        assert!(raw.len() > Limits::default().cap_bytes / 4);
        let mut s = Shaper::new(Limits::default(), None);
        let out = s.shape_tool("bash", None, &raw, 1);
        assert_eq!(out.kind, Kind::Json);
        assert!(out.stashed.is_none() && out.passes == vec!["json"], "{:?}", out.passes);
        assert!(!out.text.contains('\n') || out.text.trim_end().lines().count() == 1);
        let a: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let b: serde_json::Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_lossy_pass_runs_only_when_the_full_text_is_stashed() {
        // no stash directory: the build log keeps every line (only the plain passes ran)
        let mut none = Shaper::new(Limits::default(), None);
        let o = none.shape_tool("bash", Some("bash:cargo build"), &cargo_build(), 2);
        assert_eq!(o.kind, Kind::Cargo);
        assert!(o.stashed.is_none() && !o.passes.contains(&"cargo") && o.text.contains("Compiling crate3 "));
        // with one: folded, a handle, the pass named, and the stash holds the Compiling lines
        let dir = tmp("cargo");
        let mut s = Shaper::new(Limits::default(), Some(&dir));
        let o = s.shape_tool("bash", Some("bash:cargo build"), &cargo_build(), 2);
        let h = o.stashed.clone().expect("handle");
        assert_eq!(o.passes, vec!["cargo"]);
        assert!(o.text.contains("[compiled 15 crates]") && o.text.contains(&h.id));
        assert!(s.fetch(&h.id, 1, 3).unwrap().contains("Compiling crate0 v1.0.0"));
        assert!(o.saved_bytes > 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn passes_name_what_ran_and_the_cap_reuses_the_structured_stash() {
        let dir = tmp("passes");
        let lim = Limits { cap_bytes: 600, head_bytes: 250, tail_bytes: 150, fold_at: 3 };
        let mut s = Shaper::new(lim, Some(&dir));
        let raw = format!("\x1b[32m{}\x1b[0m", cargo_test());
        let o = s.shape_tool("bash", Some("bash:cargo test"), &raw, 5);
        assert_eq!(o.passes, vec!["clean", "cargo", "cap"], "{:?}", o.passes);
        assert!(o.stashed.is_some() && o.text.contains("left out here"));
        assert_eq!(s.next_handle, 2, "one stash for both passes");
        // plain output: no structured pass, nothing to report
        let p = s.shape_tool("bash", None, "hello\n", 6);
        assert_eq!((p.kind, p.passes.len()), (Kind::Plain, 0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn file_reads_and_small_outputs_are_not_restructured() {
        let dir = tmp("reads");
        let mut s = Shaper::new(Limits::default(), Some(&dir));
        let o = s.shape_tool("Read", Some("read:/w/build.log"), &cargo_build(), 1);
        assert_eq!(o.kind, Kind::Plain);
        assert!(o.stashed.is_none() && o.text.contains("Compiling crate3 "));
        assert!(shape_structured(Kind::Cargo, "error[E0308]: mismatched types\n  --> src/a.rs:1:1\n", &Limits::default()).is_none());
        assert!(shape_structured(Kind::Plain, "anything", &Limits::default()).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
