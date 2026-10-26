//! **pgm-workflow-command-internal** = PGM_WorkflowCommand_Internal (PsyGraphMediaWorkflowCommandInternal).
//!
//! `/hyperwork <name> [options] <prompt>` (spec 10.2, T-207): one parser for every product (the page's Work box, Hyper
//! Prod Man, jcode), so the three read a line the same way. Serde only: no engine, no runtime, so a TUI or a page build
//! takes it alone. Mirrored by copy into Jcode PGM's `pgm/libs/` (the same file); the engine re-exports it as `command`.
//!
//! ```text
//! /hyperwork release-notes --project VibeRoller --on hub --version 3 the prompt text …
//! ```
//! Options may stand anywhere after the name (`--project X`, `--on local|hub|gateway|hpm`, `--version N|tag`, and
//! `--project=X` style); everything else is the prompt. A name that matches no workflow runs nothing: the answer names
//! the nearest ones instead (never runs "the closest" by guess).
#![forbid(unsafe_code)]
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub name: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// Not a `/hyperwork` line at all.
    No,
    /// `/hyperwork` with nothing (or only part of a name) after it: the type-ahead's text.
    Typing(String),
    Command(Command),
    /// A line that is a `/hyperwork` command but is wrong (`--on` with nothing after it, an unknown runner …).
    Bad(String),
}

const RUNNERS: &[&str] = &["local", "hub", "gateway", "hpm", "any"];

/// Reads a line. `/hw` is the short form.
pub fn parse(line: &str) -> Parsed {
    let t = line.trim_start();
    let rest = match t.strip_prefix("/hyperwork").or_else(|| t.strip_prefix("/hw")) {
        Some(r) if r.is_empty() || r.starts_with(char::is_whitespace) => r,
        _ => return Parsed::No,
    };
    let rest = rest.trim_start();
    let (name, mut tail) = match rest.find(char::is_whitespace) { Some(i) => (&rest[..i], &rest[i..]), None => (rest, "") };
    if name.is_empty() || tail.is_empty() && !line.ends_with(char::is_whitespace) { return Parsed::Typing(name.to_string()); }
    if name.starts_with("--") { return Parsed::Bad("the workflow's name comes first: /hyperwork <name> [--project P] [--on runner] [--version V] <prompt>".into()); }
    let mut cmd = Command { name: name.to_string(), ..Default::default() };
    let mut words: Vec<&str> = vec![];
    loop {
        tail = tail.trim_start();
        if tail.is_empty() { break; }
        let (w, after) = match tail.find(char::is_whitespace) { Some(i) => (&tail[..i], &tail[i..]), None => (tail, "") };
        let opt = ["project", "on", "version"].iter().find(|o| w == format!("--{o}") || w.starts_with(&format!("--{o}=")));
        match opt {
            Some(o) => {
                let (value, after) = match w.split_once('=') {
                    Some((_, v)) => (v.to_string(), after),
                    None => {
                        let a = after.trim_start();
                        let (v, rest) = match a.find(char::is_whitespace) { Some(i) => (&a[..i], &a[i..]), None => (a, "") };
                        (v.to_string(), rest)
                    }
                };
                if value.is_empty() || value.starts_with("--") { return Parsed::Bad(format!("--{o} needs a value")); }
                match *o {
                    "project" => cmd.project = Some(value),
                    "on" => {
                        if !RUNNERS.contains(&value.as_str()) { return Parsed::Bad(format!("--on is one of {}, not `{value}`", RUNNERS.join(", "))); }
                        cmd.on = Some(value)
                    }
                    _ => cmd.version = Some(value),
                }
                tail = after;
            }
            None => { words.push(w); tail = after; }
        }
    }
    cmd.prompt = words.join(" ");
    Parsed::Command(cmd)
}

/// How near a name is to what was typed (lower is nearer), for "did you mean": a prefix, then a word start, then a
/// substring, then letters in order; None: not near at all.
pub fn nearness(name: &str, typed: &str) -> Option<u32> {
    let (n, q) = (name.to_lowercase(), typed.to_lowercase());
    if q.is_empty() { return Some(5); }
    if n == q { return Some(0); }
    if n.starts_with(&q) { return Some(1); }
    if n.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&q)) { return Some(2); }
    if n.contains(&q) { return Some(3); }
    let mut it = n.chars();
    if q.chars().all(|c| it.any(|x| x == c)) { return Some(4); }
    None
}

/// The names nearest to what was typed, nearest first (at most `max`).
pub fn nearest<'a>(names: impl IntoIterator<Item = &'a str>, typed: &str, max: usize) -> Vec<String> {
    let mut v: Vec<(u32, &str)> = names.into_iter().filter_map(|n| nearness(n, typed).map(|r| (r, n))).collect();
    v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(b.1)));
    v.into_iter().take(max).map(|(_, n)| n.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(line: &str) -> Command { match parse(line) { Parsed::Command(c) => c, other => panic!("{line}: {other:?}") } }

    #[test]
    fn r460_the_command_line() {
        assert_eq!(parse("hello"), Parsed::No);
        assert_eq!(parse("/hyperworks x"), Parsed::No);
        assert_eq!(parse("/hyperwork"), Parsed::Typing(String::new()));
        assert_eq!(parse("/hyperwork te"), Parsed::Typing("te".into()), "still typing the name");
        let c = cmd("/hyperwork release-notes write the notes for v5");
        assert_eq!((c.name.as_str(), c.prompt.as_str(), c.project.as_deref()), ("release-notes", "write the notes for v5", None));
        let c = cmd("/hyperwork team-review --project VibeRoller --on hub --version 3 look at the diff");
        assert_eq!((c.name.as_str(), c.project.as_deref(), c.on.as_deref(), c.version.as_deref(), c.prompt.as_str()), ("team-review", Some("VibeRoller"), Some("hub"), Some("3"), "look at the diff"));
        let c = cmd("/hw notes fix it --project=Jcode-PGM now");
        assert_eq!((c.name.as_str(), c.project.as_deref(), c.prompt.as_str()), ("notes", Some("Jcode-PGM"), "fix it now"));
        assert_eq!(cmd("/hyperwork test ").prompt, "", "a name and a space: a command with an empty prompt");
        assert!(matches!(parse("/hyperwork test --on moon x"), Parsed::Bad(m) if m.contains("--on")));
        assert!(matches!(parse("/hyperwork test --project"), Parsed::Bad(m) if m.contains("--project")));
        assert!(matches!(parse("/hyperwork --project x test"), Parsed::Bad(_)));
    }

    #[test]
    fn r460_a_name_that_matches_nothing_gets_the_nearest() {
        let names = ["test", "team-review", "release-notes", "deploy"];
        assert_eq!(nearest(names, "te", 2), ["test", "team-review"], "prefix first");
        assert_eq!(nearest(names, "te", 9).last().map(String::as_str), Some("release-notes"), "letters in order come last");
        assert_eq!(nearest(names, "review", 3), ["team-review"], "word starts");
        assert_eq!(nearest(names, "rlsnts", 3), ["release-notes"], "letters in order");
        assert!(nearest(names, "zzz", 3).is_empty());
    }
}
