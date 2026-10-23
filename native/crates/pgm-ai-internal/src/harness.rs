//! The coding harnesses a Work run can use, behind one trait (Jcode-Mods WORKPLAN 9a, ARCHITECTURE.md §3): Claude Code
//! (`claude -p --output-format stream-json`, the message on stdin) and Jcode PGM (`jcodepgm run --ndjson`, the message
//! as the last argument). A harness says how its process is started ([`Harness::command`]), how to resume one of its
//! sessions ([`Harness::session_resume_args`]) and turns each stdout line into the hub's one event shape
//! ([`Harness::map_line`] -> [`HubEvent`]). The shape is Claude Code's stream-json (the alias the page renders), so
//! Claude's lines pass through unchanged and Jcode's are mapped onto it. Work (work.rs) holds no `if harness == …`.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::jcode;
use pgm_core_internal::json::s;

/// Which harness a run or a stored session belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarnessKind { Claude, Jcode }

impl HarnessKind {
    /// The name stored as `sessionHarness` and tagged on events: "claude" | "jcode".
    pub fn as_str(&self) -> &'static str { match self { HarnessKind::Claude => jcode::CLAUDE_HARNESS, HarnessKind::Jcode => jcode::HARNESS } }
    pub fn parse(name: &str) -> Option<HarnessKind> { match name { jcode::CLAUDE_HARNESS => Some(HarnessKind::Claude), jcode::HARNESS => Some(HarnessKind::Jcode), _ => None } }
}

/// One event of the hub's schema, the shape the page consumes (Claude Code's stream-json names are the alias). Each
/// variant carries the whole JSON event, so nothing a harness sent is lost on the way.
#[derive(Clone, Debug, PartialEq)]
pub enum HubEvent {
    /// `system` / `init`: the session started (session id, model).
    Init(Value),
    /// any other `system` event (a session announcement, hooks, ...).
    System(Value),
    /// `assistant`: text and / or tool_use blocks (usage rides on `message.usage`).
    Assistant(Value),
    /// `user`: tool_result blocks.
    User(Value),
    /// `result`: the run's final answer (`is_error`, `usage`, `duration_ms`).
    Result(Value),
    /// any other event, kept as it came (Claude's own extras; Jcode's `{ type: "jcode", event }`).
    Unknown(Value),
    /// a stdout line that is not JSON: the page shows it as `{ type: "raw", text }`.
    Raw(String),
}

impl HubEvent {
    /// Classifies an event by its `type` (and `subtype` for `system`).
    pub fn from_value(v: Value) -> HubEvent {
        match v["type"].as_str().unwrap_or("") {
            "system" => if v["subtype"] == json!("init") { HubEvent::Init(v) } else { HubEvent::System(v) },
            "assistant" => HubEvent::Assistant(v),
            "user" => HubEvent::User(v),
            "result" => HubEvent::Result(v),
            _ => HubEvent::Unknown(v),
        }
    }
    /// The JSON the page receives.
    pub fn into_value(self) -> Value {
        match self {
            HubEvent::Init(v) | HubEvent::System(v) | HubEvent::Assistant(v) | HubEvent::User(v) | HubEvent::Result(v) | HubEvent::Unknown(v) => v,
            HubEvent::Raw(t) => json!({ "type": "raw", "text": t }),
        }
    }
}

/// Everything a harness needs to start one run (borrowed from Work; no I/O to build it).
pub struct RunContext<'a> {
    /// the provider entry that was picked (bridge/providers.json shape)
    pub prov: &'a Value,
    /// the message to send (attached-file text, context and freshness note already folded in)
    pub input: &'a str,
    /// the harness session to resume, when this run continues one
    pub resume: Option<&'a str>,
    /// the model Work resolved (state model, else the default); Claude's `--model`
    pub model: &'a str,
    /// the person's chosen model as stored in work.json (Jcode picks its own from the provider's list)
    pub state_model: &'a str,
    /// the person's profile directory (the private jcode home lives under it)
    pub acct_dir: &'a Path,
    /// the run's working directory (the person's worktree)
    pub cwd: &'a Path,
    /// the hub's system prompt (laws included)
    pub system_prompt: &'a str,
    /// the hub's MCP servers as JSON (Jcode writes them as its mcp.json; Null for Claude)
    pub mcp_json: &'a Value,
    /// the MCP config file Claude Code is pointed at
    pub mcp_config: &'a Path,
    pub permission_mode: &'a str,
    pub allowed_tools: &'a [String],
    /// folders the message's attached files live in (Claude `--add-dir`)
    pub add_dirs: &'a [String],
    /// `claudeCommand` / `claudeArgs` from the hub config (the regression runner's fake Claude; else `claude`)
    pub claude_command: &'a str,
    pub claude_args: &'a [String],
}

/// How to start the process of one run.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    /// extra environment of the harness itself (Jcode: its private home); Work adds provider / relay / Forgejo env
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// what is written to the process's stdin ("" = nothing)
    pub stdin: String,
}

pub trait Harness: Send {
    fn kind(&self) -> HarnessKind;
    /// The process to start. Jcode's prepares its private home on disk first (mcp.json, prompt overlay).
    fn command(&self, ctx: &RunContext) -> CommandSpec;
    /// Turns one stdout line into hub events (empty = nothing to show). A line that is not JSON is `HubEvent::Raw`.
    fn map_line(&mut self, line: &str) -> Vec<HubEvent>;
    /// The arguments that resume a session of this harness.
    fn session_resume_args(&self, session: &str) -> Vec<String>;
    /// A note for the page when the run starts, if the harness has one.
    fn banner(&self) -> Option<String> { None }
    /// True once the harness reported the end of the run (done / error).
    fn ended(&self) -> bool { false }
}

/// The harness for a kind, ready for one run.
pub fn harness_for(kind: HarnessKind, prov: &Value, state_model: &str) -> Box<dyn Harness> {
    match kind { HarnessKind::Claude => Box::new(ClaudeHarness), HarnessKind::Jcode => Box::new(JcodeHarness::new(prov, state_model)) }
}

/// Claude Code: stream-json events already are the hub shape, passed through as they are.
pub struct ClaudeHarness;

impl Harness for ClaudeHarness {
    fn kind(&self) -> HarnessKind { HarnessKind::Claude }
    fn command(&self, ctx: &RunContext) -> CommandSpec {
        let mut args: Vec<String> = ctx.claude_args.to_vec();
        args.extend(["-p", "--output-format", "stream-json", "--verbose", "--permission-mode"].iter().map(|x| x.to_string()));
        args.push(ctx.permission_mode.into());
        args.extend(["--mcp-config".into(), ctx.mcp_config.display().to_string(), "--strict-mcp-config".into(), "--append-system-prompt".into(), ctx.system_prompt.into()]);
        if !ctx.allowed_tools.is_empty() { args.push("--allowedTools".into()); args.push(ctx.allowed_tools.join(",")); }
        if !ctx.model.is_empty() { args.push("--model".into()); args.push(ctx.model.into()); }
        if let Some(r) = ctx.resume { args.extend(self.session_resume_args(r)); }
        for d in ctx.add_dirs { args.push("--add-dir".into()); args.push(d.clone()); }
        CommandSpec { program: ctx.claude_command.into(), args, env: vec![], cwd: ctx.cwd.to_path_buf(), stdin: ctx.input.into() }
    }
    fn map_line(&mut self, line: &str) -> Vec<HubEvent> {
        let l = line.trim(); if l.is_empty() { return vec![]; }
        match serde_json::from_str::<Value>(l) { Ok(v) => vec![HubEvent::from_value(v)], Err(_) => vec![HubEvent::Raw(l.to_string())] }
    }
    fn session_resume_args(&self, session: &str) -> Vec<String> { vec!["--resume".into(), session.into()] }
}

/// Jcode PGM: `jcodepgm run --ndjson`, its events mapped onto the hub shape (jcode.rs `Mapper`).
pub struct JcodeHarness { mapper: jcode::Mapper, model: String, label: String }

impl JcodeHarness {
    pub fn new(prov: &Value, state_model: &str) -> JcodeHarness {
        // the model jcode is asked for: the person's pick when the provider offers it (any when it lists none), else its first
        let ids: Vec<String> = prov["models"].as_array().map(|m| m.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let m0 = state_model.to_string();
        let model = if ids.contains(&m0) || (ids.is_empty() && !m0.is_empty()) { m0 } else { ids.first().cloned().unwrap_or_default() };
        JcodeHarness { mapper: jcode::Mapper::new(state_model), model, label: s(prov, "label") }
    }
}

impl Harness for JcodeHarness {
    fn kind(&self) -> HarnessKind { HarnessKind::Jcode }
    fn command(&self, ctx: &RunContext) -> CommandSpec {
        let home = jcode::home_for(ctx.acct_dir, ctx.prov);
        let env = jcode::prepare_home(&home, ctx.mcp_json, ctx.system_prompt);
        let (program, args) = jcode::command(ctx.prov, &self.model, ctx.resume, ctx.input);
        CommandSpec { program, args, env, cwd: ctx.cwd.to_path_buf(), stdin: String::new() }
    }
    fn map_line(&mut self, line: &str) -> Vec<HubEvent> {
        let l = line.trim(); if l.is_empty() { return vec![]; }
        match serde_json::from_str::<Value>(l) { Ok(v) => self.mapper.map(v).into_iter().map(HubEvent::from_value).collect(), Err(_) => vec![HubEvent::Raw(l.to_string())] }
    }
    fn session_resume_args(&self, session: &str) -> Vec<String> { vec!["--resume".into(), session.into()] }
    fn banner(&self) -> Option<String> { Some(format!("Using Jcode PGM ({}){}.", self.label, if self.model.is_empty() { String::new() } else { format!(", model {}", self.model) })) }
    fn ended(&self) -> bool { self.mapper.ended() }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn all(h: &mut dyn Harness, lines: &[&str]) -> Vec<HubEvent> { lines.iter().flat_map(|l| h.map_line(l)).collect() }

    #[test]
    fn claude_lines_pass_through_classified_and_unchanged() {
        let mut h = ClaudeHarness;
        let init = r#"{"type":"system","subtype":"init","session_id":"c1","model":"claude-haiku-5-5"}"#;
        let text = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hi"}],"usage":{"input_tokens":3,"output_tokens":1,"cache_read_input_tokens":40,"cache_creation_input_tokens":7}}}"#;
        let tool = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#;
        let res = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a","is_error":false}]}}"#;
        let fin = r#"{"type":"result","subtype":"success","is_error":false,"result":"Hi","usage":{"input_tokens":3,"output_tokens":1,"cache_read_input_tokens":40,"cache_creation_input_tokens":7}}"#;
        let err = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"boom"}"#;
        let unknown = r#"{"type":"rate_limit_event","info":{"x":1}}"#;
        let out = all(&mut h, &[init, text, tool, res, fin, err, unknown]);
        assert!(matches!(out[0], HubEvent::Init(_)) && matches!(out[1], HubEvent::Assistant(_)) && matches!(out[2], HubEvent::Assistant(_)));
        assert!(matches!(out[3], HubEvent::User(_)) && matches!(out[4], HubEvent::Result(_)) && matches!(out[5], HubEvent::Result(_)) && matches!(out[6], HubEvent::Unknown(_)));
        // nothing is changed on the way
        for (i, src) in [init, text, tool, res, fin, err, unknown].iter().enumerate() { assert_eq!(out[i].clone().into_value(), serde_json::from_str::<Value>(src).unwrap()); }
        let v = out[1].clone().into_value();
        assert_eq!(v["message"]["usage"]["cache_read_input_tokens"], 40); assert_eq!(v["message"]["content"][0]["text"], "Hi");
        assert_eq!(out[5].clone().into_value()["is_error"], true);
        assert!(!h.ended());
    }

    #[test]
    fn claude_blank_lines_are_nothing_and_non_json_is_raw() {
        let mut h = ClaudeHarness;
        assert!(h.map_line("   ").is_empty());
        let out = h.map_line("  warning: not json ");
        assert_eq!(out, vec![HubEvent::Raw("warning: not json".into())]);
        assert_eq!(out[0].clone().into_value(), json!({ "type": "raw", "text": "warning: not json" }));
    }

    #[test]
    fn jcode_lines_become_the_hub_shape() {
        let mut h = JcodeHarness::new(&json!({ "kind": "jcode", "label": "J" }), "");
        let out = all(&mut h, &[
            r#"{"type":"start","session_id":"s1","provider":"viberoller","model":"claude-haiku-5-5"}"#,
            r#"{"type":"text_delta","text":"Let me look."}"#,
            r#"{"type":"tool_start","id":"t1","name":"bash"}"#,
            r#"{"type":"tool_input","delta":"{\"command\":\"ls\"}"}"#,
            r#"{"type":"tool_exec","id":"t1","name":"bash"}"#,
            r#"{"type":"tool_done","id":"t1","name":"bash","output":"a\nb\n","error":null}"#,
            r#"{"type":"connection_type","connection":"direct"}"#,
            r#"{"type":"tokens","input":10,"output":2,"cache_read_input":40,"cache_creation_input":7}"#,
            r#"{"type":"done","session_id":"s1","text":"Two files."}"#,
        ]);
        let kinds: Vec<&str> = out.iter().map(|e| match e { HubEvent::Init(_) => "init", HubEvent::System(_) => "system", HubEvent::Assistant(_) => "assistant", HubEvent::User(_) => "user", HubEvent::Result(_) => "result", HubEvent::Unknown(_) => "unknown", HubEvent::Raw(_) => "raw" }).collect();
        assert_eq!(kinds, ["init", "assistant", "assistant", "user", "unknown", "assistant", "result"]);
        let v: Vec<Value> = out.into_iter().map(HubEvent::into_value).collect();
        assert_eq!(v[0]["harness"], "jcode"); assert_eq!(v[0]["session_id"], "s1");
        assert_eq!(v[1]["message"]["content"][0]["text"], "Let me look.");
        assert_eq!(v[2]["message"]["content"][0]["type"], "tool_use"); assert_eq!(v[2]["message"]["content"][0]["input"]["command"], "ls");
        assert_eq!(v[3]["message"]["content"][0]["tool_use_id"], "t1"); assert_eq!(v[3]["message"]["content"][0]["is_error"], false);
        assert_eq!(v[4]["type"], "jcode"); assert_eq!(v[4]["event"]["type"], "connection_type");
        assert_eq!(v[5]["message"]["content"][0]["text"], "Two files.");
        assert_eq!(v[6]["is_error"], false); assert_eq!(v[6]["result"], "Two files.");
        assert_eq!(v[6]["usage"]["cache_read_input_tokens"], 40); assert_eq!(v[6]["usage"]["cache_creation_input_tokens"], 7);
        assert!(h.ended());
    }

    #[test]
    fn jcode_error_is_a_failed_result_and_non_json_is_raw() {
        let mut h = JcodeHarness::new(&json!({ "kind": "jcode" }), "");
        let out = all(&mut h, &[r#"{"type":"start","session_id":"s3"}"#, "not json at all", r#"{"type":"error","session_id":"s3","message":"boom"}"#]);
        assert_eq!(out[1], HubEvent::Raw("not json at all".into()));
        let r = out[2].clone().into_value();
        assert_eq!(r["type"], "result"); assert_eq!(r["is_error"], true); assert_eq!(r["result"], "boom"); assert_eq!(r["harness"], "jcode");
        assert!(h.map_line("").is_empty());
        assert!(h.ended());
    }

    #[test]
    fn kinds_round_trip_and_resume_args_name_the_session() {
        for k in [HarnessKind::Claude, HarnessKind::Jcode] { assert_eq!(HarnessKind::parse(k.as_str()), Some(k)); }
        assert_eq!(HarnessKind::parse("other"), None);
        assert_eq!(ClaudeHarness.session_resume_args("x"), ["--resume", "x"]);
        assert_eq!(JcodeHarness::new(&json!({}), "").session_resume_args("x"), ["--resume", "x"]);
        assert_eq!(harness_for(HarnessKind::Jcode, &json!({}), "").kind(), HarnessKind::Jcode);
        assert_eq!(harness_for(HarnessKind::Claude, &json!({}), "").kind(), HarnessKind::Claude);
    }

    #[test]
    fn the_commands_match_what_each_harness_always_ran() {
        let (mcp, tools, dirs, cargs) = (json!({ "mcpServers": {} }), vec!["Read".to_string(), "Bash(x:*)".to_string()], vec!["/att".to_string()], vec!["--fake".to_string()]);
        let prov = json!({ "kind": "jcode", "command": "/opt/jcodepgm", "provider": "viberoller", "models": ["m1", "m2"], "label": "J" });
        let tmp = std::env::temp_dir().join(format!("pgm-harness-{}", pgm_core_internal::crypto::random_hex(4)));
        let mk = |prov: &'_ Value, resume: Option<&'static str>| RunContext { prov: Box::leak(Box::new(prov.clone())), input: "hello", resume, model: "claude-x", state_model: "m2", acct_dir: Box::leak(Box::new(tmp.clone())), cwd: Path::new("/wt"),
            system_prompt: "SP", mcp_json: Box::leak(Box::new(mcp.clone())), mcp_config: Path::new("/p/.mcp.json"), permission_mode: "default", allowed_tools: Box::leak(Box::new(tools.clone())), add_dirs: Box::leak(Box::new(dirs.clone())), claude_command: "claude", claude_args: Box::leak(Box::new(cargs.clone())) };
        let c = ClaudeHarness.command(&mk(&prov, Some("sess")));
        assert_eq!(c.program, "claude"); assert_eq!(c.stdin, "hello"); assert!(c.env.is_empty()); assert_eq!(c.cwd, PathBuf::from("/wt"));
        assert_eq!(c.args, ["--fake", "-p", "--output-format", "stream-json", "--verbose", "--permission-mode", "default", "--mcp-config", "/p/.mcp.json", "--strict-mcp-config", "--append-system-prompt", "SP",
            "--allowedTools", "Read,Bash(x:*)", "--model", "claude-x", "--resume", "sess", "--add-dir", "/att"]);
        let jh = JcodeHarness::new(&prov, "m2");
        let j = jh.command(&mk(&prov, Some("sess")));
        assert_eq!(j.program, "/opt/jcodepgm"); assert_eq!(j.stdin, "");
        assert_eq!(j.args, ["run", "--ndjson", "--no-update", "-p", "viberoller", "-m", "m2", "--resume", "sess", "hello"]);
        assert!(j.env.iter().any(|(k, v)| k == "JCODE_HOME" && v == &tmp.join("jcode-home").display().to_string()));
        assert_eq!(jh.banner().unwrap(), "Using Jcode PGM (J), model m2.");
        let _ = std::fs::remove_dir_all(tmp);
    }
}
