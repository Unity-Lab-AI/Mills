//! The prompt enhancer (Work's sparkle button, 5.35.0): the person's draft goes to a throw-away, one-shot Claude Code call and
//! comes back re-worded for an AI: the same wishes, all of them, clearer and broken into tasks. The person reads it in
//! the composer and decides what to send.
//! - throw-away: `claude -p` in a fresh empty folder (deleted afterwards), no tools, no MCP servers, no session kept,
//!   the enhancer's own system prompt instead of Claude Code's; nothing of the Work session goes with it (no history, no
//!   worktree, no laws), only the draft and the name of the project it is for
//! - not a Work run: it never counts against the session limits (plans/multi-session.md § 6) and never touches a
//!   session's state; one at a time per person
//! - the person's own AI (the session's provider) when it is Claude Code that runs on this hub, else the hub's own
//!   Claude; the session's model unless config `enhanceModel` names one
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::hub::{answer, AiHub, Resp};
use crate::sessions;
use crate::work::Work;
use pgm_core_internal::json::s;
use pgm_identity_internal::Account;
use pgm_net_internal::http::Req;

/// The longest draft it takes (characters).
pub const MAX_DRAFT: usize = 20_000;
const TIMEOUT: Duration = Duration::from_secs(180);

/// What the enhancer is told (its whole system prompt).
pub const SYSTEM: &str = r###"You are a prompt enhancer. You never do the task yourself. You receive a draft prompt that a person wrote for an AI coding agent (Claude Code working in a software project) and you rewrite it so the agent understands it better. You answer with the rewritten prompt and nothing else.

THE ONE RULE ABOVE ALL: KEEP EVERYTHING THE PERSON SAID.
- Every request, wish, idea, question, constraint, preference, example, name, path, number, version, URL, error message, quote and code snippet in the draft must be in your rewrite. Leave nothing out, however small, vague or casual it sounds.
- Do not add new requirements, features, opinions or assumptions of your own. Do not invent file names, APIs, numbers or behaviour the person did not mention. Do not decide things the person left open: where something is open or ambiguous, say so as an open point for the agent to settle (by looking or by asking), with the possible readings, instead of picking one.
- Do not weaken or strengthen anything: "maybe", "if possible" and "ideally" stay soft; "must", "never" and "always" stay hard.
- Keep code, commands, paths, identifiers, error output and quoted text exactly as written (fix nothing inside them). Keep the person's own terms for things; you may add a short clarification next to a term, never replace it.
- Keep the person's point of view: they are asking the agent, so write to the agent ("Add…", "Find out whether…").
- If the draft has questions for the agent, keep them as questions the agent must answer.

WHAT YOU IMPROVE:
1. Clarity: rewrite run-on, chatty or typo-laden text into plain, precise sentences. Fix spelling and grammar outside code and quotes. Resolve "this", "that", "it" to what they refer to when the draft makes it clear.
2. Decomposition: split the draft into its separate tasks. One task per distinct thing to do, in a sensible order (dependencies first; otherwise the person's order). Under each task, list what it must do and any details the person gave for it.
3. Structure: use this layout, leaving out a section only when there is nothing for it:

## Goal
One or two sentences: what the person wants overall, in their terms.

## Tasks
1. **<short task name>**
   - what must happen, with every detail the person gave
   - constraints or preferences for this task
2. ...

## Constraints and preferences
Anything that applies to all tasks (style, compatibility, what not to touch, how to test).

## Open points
Ambiguities, missing information and decisions the person left open, each with the possible readings. The agent settles them by checking the project or by asking the person.

## Done when
What the person will see when the work is complete, taken only from what they said (one line per task where possible).

4. Precision without invention: when the person describes something loosely ("the button near send", "like the mac thing"), keep their description and, if it helps, add the plain meaning in brackets, marked as your reading ("(reading: the macOS genie minimize effect)"). Never present a reading as fact.

FORMAT OF YOUR ANSWER:
- Only the rewritten prompt, in Markdown, ready to send to the agent. No preface ("Here is…"), no explanation of what you changed, no closing remarks, no surrounding code fence.
- Write in the language of the draft.
- Be as long as needed to keep everything and no longer: no filler, no repetition.
- If the draft is already a single, clear, small request, return it lightly cleaned up without forcing the full layout (a short "## Tasks" list is enough).
- If the draft is not a request at all (a greeting, a single word), return it unchanged."###;

fn running() -> &'static Mutex<HashSet<String>> { static R: OnceLock<Mutex<HashSet<String>>> = OnceLock::new(); R.get_or_init(Mutex::default) }

/// POST /api/work/enhance { text, session? } → { text } (the rewrite) or { error }.
pub async fn route(hub: &Arc<dyn AiHub>, a: &Account, r: &Req) -> Resp {
    let w = hub.work();
    let b = r.json();
    let draft = s(&b, "text");
    if draft.trim().is_empty() { return answer(400, json!({ "error": "Nothing to enhance: type your prompt first." })); }
    if draft.chars().count() > MAX_DRAFT { return answer(413, json!({ "error": format!("That prompt is too long to enhance (at most {MAX_DRAFT} characters).") })); }
    let Some(sx) = sessions::resolve(a, &sessions::wanted(r, &b)) else { return answer(404, json!({ "error": "No such session." })) };
    let st = sessions::sstate(w, a, &sx);
    let ai = hub.ai();
    // the person's own AI when Claude Code runs it here; else the hub's own Claude
    let mine = w.effective(ai, &a.username, &s(&st, "provider")).filter(|p| match s(p, "kind").as_str() { "host" | "anthropic" | "claude-cli" => true, "host-claude" => s(p, "remoteKind") != "claude-cli-work", _ => false });
    let Some(prov) = mine.or_else(|| w.provider_get(ai, "host", "")) else { return answer(503, json!({ "error": "No Claude is available to enhance the prompt: pick a Claude model for this session, or ask the host." })) };
    let kind = s(&prov, "kind");
    if (kind == "host" || kind == "claude-cli") && hub.root().join("ai-host.off").exists() { return answer(503, json!({ "error": "The host's AI is switched off right now, so the prompt cannot be enhanced." })); }
    let model = hub.cfg().get("enhanceModel").and_then(Value::as_str).map(String::from).unwrap_or_else(|| { let m = s(&st, "model"); if m.is_empty() { w.default_model_of(a) } else { m } });
    if let Err(m) = ai.scope_check(&a.username, &s(&prov, "id"), "work", &model) { return answer(403, json!({ "error": m })); }
    if !running().lock().unwrap_or_else(|e| e.into_inner()).insert(a.username.clone()) { return answer(409, json!({ "error": "Already enhancing a prompt of yours: wait for it." })); }
    struct Done(String);
    impl Drop for Done { fn drop(&mut self) { running().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0); } }
    let _done = Done(a.username.clone());

    let mut env: Vec<(String, String)> = Work::claude_env(&prov).unwrap_or_default();
    if kind == "host-claude" {
        let host_id = s(&prov, "hostId"); let prov_id = s(&prov, "id").split_once('/').map(|x| x.1.to_string()).unwrap_or_default();
        if !ai.online(&host_id) { return answer(503, json!({ "error": format!("{} is offline right now, so the prompt cannot be enhanced. Pick another AI, or wait for it.", s(&prov, "nickname")) })); }
        let tok = w.llm_token(&a.username, &host_id, &prov_id);
        env.extend([("ANTHROPIC_BASE_URL".into(), format!("http://127.0.0.1:{}/llm/{tok}", hub.cfg().port)), ("ANTHROPIC_AUTH_TOKEN".into(), "viberoller-relay".into()), ("ANTHROPIC_API_KEY".into(), String::new()), ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into())]);
    }
    let project = w.project_in(a, &sx);
    let input = format!("{}The draft prompt to enhance (everything between the lines):\n----------\n{draft}\n----------",
        if project.is_empty() || project == pgm_collab_internal::worktree::NO_PROJECT { String::new() } else { format!("(It is for work in the project \"{project}\".)\n\n") });
    let dir = std::env::temp_dir().join(format!("vr-enhance-{}", pgm_core_internal::crypto::random_hex(8)));
    if std::fs::create_dir_all(&dir).is_err() { return answer(500, json!({ "error": "Could not make a folder to enhance the prompt in." })); }
    struct Gone(std::path::PathBuf);
    impl Drop for Gone { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _gone = Gone(dir.clone());
    let pre: Vec<String> = hub.cfg().get("claudeArgs").and_then(Value::as_array).map(|x| x.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let args = args(&model);
    let mut cmd = tokio::process::Command::new(hub.cfg().get("claudeCommand").and_then(Value::as_str).unwrap_or("claude"));
    cmd.args(&pre).args(&args).current_dir(&dir).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    for (k, v) in &env { cmd.env(k, v); }
    #[cfg(windows)]
    { cmd.creation_flags(0x0800_0000); }
    let mut child = match cmd.spawn() { Ok(c) => c, Err(e) => return answer(500, json!({ "error": format!("Could not start Claude to enhance the prompt: {e}") })) };
    if let Some(mut i) = child.stdin.take() { let p = input.clone(); tokio::spawn(async move { let _ = i.write_all(p.as_bytes()).await; let _ = i.shutdown().await; }); }
    let out = match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return answer(500, json!({ "error": format!("Claude stopped while enhancing the prompt: {e}") })),
        Err(_) => return answer(504, json!({ "error": "Enhancing the prompt took too long; your text is unchanged. Try again." })),
    };
    match result_of(&String::from_utf8_lossy(&out.stdout)) {
        Some((t, false)) if !t.trim().is_empty() => answer(200, json!({ "text": t.trim(), "model": model })),
        Some((t, _)) => answer(502, json!({ "error": format!("Claude could not enhance the prompt{}; your text is unchanged.", if t.trim().is_empty() { String::new() } else { format!(": {}", t.trim().chars().take(300).collect::<String>()) }) })),
        None => answer(502, json!({ "error": format!("Claude gave no answer; your text is unchanged.{}", { let e = String::from_utf8_lossy(&out.stderr); let e = e.trim(); if e.is_empty() { String::new() } else { format!(" ({})", e.chars().take(300).collect::<String>()) } }) })),
    }
}

/// The `claude` arguments of one throw-away enhancement (the draft goes on stdin).
pub fn args(model: &str) -> Vec<String> {
    let mut v: Vec<String> = ["-p", "--output-format", "json", "--no-session-persistence", "--strict-mcp-config", "--disable-slash-commands", "--tools", "", "--permission-mode", "default", "--system-prompt", SYSTEM].iter().map(|x| x.to_string()).collect();
    if !model.is_empty() { v.push("--model".into()); v.push(model.into()); }
    v
}

/// The answer of `claude -p --output-format json` (one JSON object; a stream's last result line also counts):
/// (its text, whether it is an error).
pub fn result_of(out: &str) -> Option<(String, bool)> {
    out.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok()).find(|v| v["type"] == json!("result") || v.get("result").is_some())
        .map(|v| (v["result"].as_str().unwrap_or("").to_string(), v["is_error"] == json!(true) || v["subtype"].as_str().is_some_and(|x| x != "success")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_enhancement_has_no_tools_no_session_and_its_own_system_prompt() {
        let a = args("claude-sonnet-5-5");
        let at = |k: &str| a.iter().position(|x| x == k).map(|i| a[i + 1].as_str());
        assert_eq!(at("--tools"), Some(""));
        assert_eq!(at("--system-prompt"), Some(SYSTEM));
        assert_eq!(at("--model"), Some("claude-sonnet-5-5"));
        assert!(a.contains(&"--no-session-persistence".to_string()) && a.contains(&"--strict-mcp-config".to_string()));
        assert!(!a.iter().any(|x| x == "--resume" || x == "--mcp-config" || x == "--append-system-prompt"));
        assert!(SYSTEM.contains("KEEP EVERYTHING THE PERSON SAID"));
    }
    #[test]
    fn the_answer_is_read_from_the_result_and_an_error_says_so() {
        assert_eq!(result_of(r###"{"type":"result","subtype":"success","is_error":false,"result":"## Goal\nx"}"###), Some(("## Goal\nx".into(), false)));
        assert_eq!(result_of("{\"type\":\"system\"}\n{\"type\":\"result\",\"subtype\":\"error_max_turns\",\"result\":\"\"}"), Some((String::new(), true)));
        assert_eq!(result_of("not json"), None);
    }
}
