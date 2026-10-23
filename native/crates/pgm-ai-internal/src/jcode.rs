//! Jcode PGM as a Work harness (EXTENSIBILITY.md in Jcode-Mods, §3): a `jcode` provider names a `jcodepgm` binary on
//! this machine; a Work run then starts `jcodepgm run --ndjson …` in the person's worktree instead of `claude -p`.
//! Each person gets a private jcode home under their profile (their accounts, sessions, mcp.json and prompt overlay
//! live there), the hub's three MCP servers are written into that home, the hub's system prompt becomes the home's
//! prompt overlay, and jcode's NDJSON events are mapped onto the event shape the page already renders (Claude Code's
//! stream-json: `system/init`, `assistant` with text / tool_use blocks, `user` with tool_result, `result`). That shape
//! is the hub event alias every harness answers in; the page keeps one renderer.
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;

use pgm_core_internal::json::s;

/// The provider kind a `jcode` provider carries (bridge/providers.json).
pub const KIND: &str = "jcode";
/// What `harness` a run or a stored session belongs to.
pub const HARNESS: &str = "jcode";
pub const CLAUDE_HARNESS: &str = "claude";
/// The default binary when a provider names none.
pub const DEFAULT_COMMAND: &str = "jcodepgm";

/// The private jcode home of one person for runs on this hub: `<profile>/jcode-home` unless the provider names one.
pub fn home_for(acct_dir: &Path, prov: &Value) -> PathBuf {
    let h = s(prov, "home");
    if !h.is_empty() && Path::new(&h).is_absolute() { PathBuf::from(h) } else { acct_dir.join("jcode-home") }
}

/// Prepares the home for a run: the hub's MCP servers (the same `mcpServers` JSON Claude Code gets), the system
/// prompt as the home's prompt overlay, a private runtime dir. Returns the env a `jcodepgm` child needs.
pub fn prepare_home(home: &Path, mcp: &Value, system_prompt: &str) -> Vec<(String, String)> {
    let _ = pgm_core_internal::fs::private_dir(home); // theirs alone: accounts and sessions will live here
    let _ = pgm_core_internal::fs::private_dir(&home.join("run"));
    pgm_core_internal::json::write_json_private(&home.join("mcp.json"), mcp); // carries this hub's secret
    let _ = std::fs::write(home.join("prompt-overlay.md"), system_prompt);
    vec![
        ("JCODE_HOME".into(), home.display().to_string()),
        ("JCODE_RUNTIME_DIR".into(), home.join("run").display().to_string()),
        ("JCODE_NO_AUTO_UPDATE".into(), "1".into()),
        ("JCODE_RUN_AUTO_POKE".into(), "0".into()),
        ("JCODE_DISABLE_CLAUDE_MCP".into(), "1".into()),
        ("JCODE_NO_TELEMETRY".into(), "1".into()),
    ]
}

/// The command line of one run: `jcodepgm run --ndjson [--no-update] [-p <provider>] [-m <model>] [--resume <id>] <message>`.
pub fn command(prov: &Value, model: &str, resume: Option<&str>, message: &str) -> (String, Vec<String>) {
    let cmd = { let c = s(prov, "command"); if c.is_empty() { DEFAULT_COMMAND.to_string() } else { c } };
    let mut args: Vec<String> = prov["args"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    args.extend(["run".to_string(), "--ndjson".into(), "--no-update".into()]);
    let p = s(prov, "provider");
    if !p.is_empty() { args.push("-p".into()); args.push(p); }
    if !model.is_empty() { args.push("-m".into()); args.push(model.into()); }
    if let Some(r) = resume.filter(|r| !r.is_empty()) { args.push("--resume".into()); args.push(r.into()); }
    args.push(message.into());
    (cmd, args)
}

/// Maps jcode's `run --ndjson` events onto the page's event shape. One per run.
pub struct Mapper { text: String, tool_name: Map<String, Value>, tool_input: Map<String, Value>, current_tool: String, usage: Value, model: String, session: String, started: Instant, ended: bool }

impl Mapper {
    pub fn new(model: &str) -> Mapper { Mapper { text: String::new(), tool_name: Map::new(), tool_input: Map::new(), current_tool: String::new(), usage: Value::Null, model: model.into(), session: String::new(), started: Instant::now(), ended: false } }
    pub fn ended(&self) -> bool { self.ended }
    fn sid(&self, ev: &Value) -> String { let x = s(ev, "session_id"); if x.is_empty() { self.session.clone() } else { x } }
    fn usage_block(&self) -> Value {
        if self.usage.is_null() { return Value::Null; }
        let g = |k: &str| self.usage[k].as_u64().unwrap_or(0);
        json!({ "input_tokens": g("input"), "output_tokens": g("output"), "cache_read_input_tokens": g("cache_read_input"), "cache_creation_input_tokens": g("cache_creation_input") })
    }
    /// The buffered text as one assistant message (nothing when there is none).
    fn flush_text(&mut self, out: &mut Vec<Value>) {
        if self.text.is_empty() { return; }
        let text = std::mem::take(&mut self.text);
        let mut msg = json!({ "role": "assistant", "model": self.model, "content": [{ "type": "text", "text": text }] });
        let u = self.usage_block(); if !u.is_null() { msg["usage"] = u; }
        out.push(json!({ "type": "assistant", "harness": HARNESS, "session_id": self.session, "message": msg }));
    }
    pub fn map(&mut self, ev: Value) -> Vec<Value> {
        let mut out = vec![];
        let t = s(&ev, "type");
        match t.as_str() {
            "start" => {
                self.session = s(&ev, "session_id"); let m = s(&ev, "model"); if !m.is_empty() { self.model = m; }
                out.push(json!({ "type": "system", "subtype": "init", "harness": HARNESS, "session_id": self.session, "model": self.model, "provider": ev["provider"] }));
            }
            "session" => { let sid = s(&ev, "session_id"); if !sid.is_empty() { self.session = sid.clone(); out.push(json!({ "type": "system", "subtype": "session", "harness": HARNESS, "session_id": sid })); } }
            "text_delta" => self.text.push_str(&s(&ev, "text")),
            "text_replace" => self.text = s(&ev, "text"),
            "text_suffix_replace" => { let (old, new) = (s(&ev, "old_text"), s(&ev, "new_text")); if !old.is_empty() && self.text.ends_with(&old) { let n = self.text.len() - old.len(); self.text.truncate(n); } self.text.push_str(&new); }
            "tool_start" => { let id = s(&ev, "id"); self.current_tool = id.clone(); self.tool_name.insert(id.clone(), ev["name"].clone()); self.tool_input.insert(id, json!("")); }
            "tool_input" => { let id = if s(&ev, "id").is_empty() { self.current_tool.clone() } else { s(&ev, "id") }; let cur = self.tool_input.get(&id).and_then(Value::as_str).unwrap_or("").to_string() + &s(&ev, "delta"); self.tool_input.insert(id, json!(cur)); }
            "tool_exec" => {
                self.flush_text(&mut out);
                let id = s(&ev, "id"); let name = if ev["name"].is_null() { self.tool_name.get(&id).cloned().unwrap_or(json!("tool")) } else { ev["name"].clone() };
                let raw = self.tool_input.get(&id).and_then(Value::as_str).unwrap_or("").to_string();
                let input = serde_json::from_str::<Value>(&raw).ok().filter(Value::is_object).unwrap_or_else(|| if raw.is_empty() { json!({}) } else { json!({ "input": raw }) });
                out.push(json!({ "type": "assistant", "harness": HARNESS, "session_id": self.session, "message": { "role": "assistant", "model": self.model, "content": [{ "type": "tool_use", "id": id, "name": name, "input": input }] } }));
            }
            "tool_done" => {
                let id = s(&ev, "id"); let err = !ev["error"].is_null() && ev["error"] != json!("");
                let content = if err { ev["error"].clone() } else if ev["output"].is_null() { json!("") } else { ev["output"].clone() };
                out.push(json!({ "type": "user", "harness": HARNESS, "session_id": self.session, "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": id, "content": content, "is_error": err }] } }));
            }
            "tokens" => { self.usage = ev.clone(); }
            "done" => {
                self.ended = true;
                if self.text.is_empty() { let t = s(&ev, "text"); if !t.is_empty() { self.text = t; } }
                let result = self.text.clone();
                self.flush_text(&mut out);
                out.push(json!({ "type": "result", "subtype": "success", "is_error": false, "harness": HARNESS, "session_id": self.sid(&ev), "duration_ms": self.started.elapsed().as_millis() as u64, "result": result, "usage": self.usage_block(), "provider": ev["provider"], "model": ev["model"] }));
            }
            "error" => {
                self.ended = true;
                self.flush_text(&mut out);
                out.push(json!({ "type": "result", "subtype": "error", "is_error": true, "harness": HARNESS, "session_id": self.sid(&ev), "duration_ms": self.started.elapsed().as_millis() as u64, "result": s(&ev, "message") }));
            }
            // progress and bookkeeping the page does not render: kept, named, for debugging
            _ => out.push(json!({ "type": "jcode", "event": ev })),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn evs(m: &mut Mapper, lines: &[&str]) -> Vec<Value> { lines.iter().flat_map(|l| m.map(serde_json::from_str(l).unwrap())).collect() }

    #[test]
    fn a_plain_answer_becomes_init_assistant_result() {
        let mut m = Mapper::new("");
        let out = evs(&mut m, &[r#"{"type":"start","session_id":"s1","provider":"viberoller","model":"claude-haiku-5-5"}"#, r#"{"type":"text_delta","text":"Hel"}"#, r#"{"type":"text_delta","text":"lo"}"#, r#"{"type":"tokens","input":10,"output":2}"#, r#"{"type":"done","session_id":"s1","text":"Hello","usage":{}}"#]);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["type"], "system"); assert_eq!(out[0]["session_id"], "s1"); assert_eq!(out[0]["model"], "claude-haiku-5-5");
        assert_eq!(out[1]["type"], "assistant"); assert_eq!(out[1]["message"]["content"][0]["text"], "Hello"); assert_eq!(out[1]["message"]["usage"]["input_tokens"], 10);
        assert_eq!(out[2]["type"], "result"); assert_eq!(out[2]["is_error"], false); assert_eq!(out[2]["result"], "Hello");
        assert!(m.ended());
    }

    #[test]
    fn tools_become_tool_use_and_tool_result_blocks() {
        let mut m = Mapper::new("x");
        let out = evs(&mut m, &[r#"{"type":"start","session_id":"s2"}"#, r#"{"type":"text_delta","text":"Let me look."}"#, r#"{"type":"tool_start","id":"t1","name":"bash"}"#, r#"{"type":"tool_input","delta":"{\"command\":\"ls\"}"}"#, r#"{"type":"tool_exec","id":"t1","name":"bash"}"#, r#"{"type":"tool_done","id":"t1","name":"bash","output":"a\nb\n","error":null}"#, r#"{"type":"done","session_id":"s2","text":"Two files."}"#]);
        let types: Vec<&str> = out.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(types, ["system", "assistant", "assistant", "user", "assistant", "result"]);
        assert_eq!(out[1]["message"]["content"][0]["text"], "Let me look.");
        assert_eq!(out[2]["message"]["content"][0]["type"], "tool_use"); assert_eq!(out[2]["message"]["content"][0]["input"]["command"], "ls");
        assert_eq!(out[3]["message"]["content"][0]["tool_use_id"], "t1"); assert_eq!(out[3]["message"]["content"][0]["is_error"], false);
        assert_eq!(out[4]["message"]["content"][0]["text"], "Two files.");
    }

    #[test]
    fn an_error_is_a_failed_result_and_unknown_events_are_kept() {
        let mut m = Mapper::new("");
        let out = evs(&mut m, &[r#"{"type":"start","session_id":"s3"}"#, r#"{"type":"connection_type","connection":"direct"}"#, r#"{"type":"error","session_id":"s3","message":"boom"}"#]);
        assert_eq!(out[1]["type"], "jcode"); assert_eq!(out[1]["event"]["type"], "connection_type");
        assert_eq!(out[2]["type"], "result"); assert_eq!(out[2]["is_error"], true); assert_eq!(out[2]["result"], "boom");
    }

    #[test]
    fn the_command_line_carries_provider_model_resume_and_message() {
        let p = json!({ "kind": "jcode", "command": "/opt/jcodepgm", "args": ["--socket", "/tmp/x.sock"], "provider": "viberoller" });
        let (c, a) = command(&p, "claude-haiku-5-5", Some("sess-9"), "hello there");
        assert_eq!(c, "/opt/jcodepgm");
        assert_eq!(a, ["--socket", "/tmp/x.sock", "run", "--ndjson", "--no-update", "-p", "viberoller", "-m", "claude-haiku-5-5", "--resume", "sess-9", "hello there"]);
        let (c2, a2) = command(&json!({ "kind": "jcode" }), "", None, "hi");
        assert_eq!(c2, DEFAULT_COMMAND); assert_eq!(a2, ["run", "--ndjson", "--no-update", "hi"]);
    }

    #[test]
    fn the_home_gets_mcp_servers_overlay_and_private_env() {
        let dir = std::env::temp_dir().join(format!("pgm-jcode-home-{}", pgm_core_internal::crypto::random_hex(4)));
        let env = prepare_home(&dir, &json!({ "mcpServers": { "pc": { "command": "node" } } }), "LAWS");
        assert!(dir.join("mcp.json").exists() && dir.join("prompt-overlay.md").exists() && dir.join("run").is_dir());
        assert_eq!(std::fs::read_to_string(dir.join("prompt-overlay.md")).unwrap(), "LAWS");
        let get = |k: &str| env.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).unwrap_or_default();
        assert_eq!(get("JCODE_HOME"), dir.display().to_string()); assert_eq!(get("JCODE_NO_AUTO_UPDATE"), "1"); assert_eq!(get("JCODE_DISABLE_CLAUDE_MCP"), "1");
        assert_eq!(home_for(Path::new("/p"), &json!({})), PathBuf::from("/p/jcode-home"));
        assert_eq!(home_for(Path::new("/p"), &json!({ "home": "/srv/j" })), PathBuf::from("/srv/j"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
