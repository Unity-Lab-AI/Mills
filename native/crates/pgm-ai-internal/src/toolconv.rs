//! Tool calls through the API (T-317): OpenAI's `tools` / `tool_calls` / `tool` messages in, and the same to and from
//! Anthropic's `tools` / `tool_use` / `tool_result` blocks for the providers that speak Anthropic. Pure functions.
use serde_json::{json, Value};

/// The most tools one request may offer.
pub const MAX_TOOLS: usize = 64;

fn st(v: &Value) -> String { v.as_str().unwrap_or("").to_string() }

/// Does this chat request carry tools (offered, or in the history as calls / results)?
pub fn wants_tools(b: &Value) -> bool {
    b["tools"].as_array().map(|t| !t.is_empty()).unwrap_or(false)
        || b["messages"].as_array().map(|m| m.iter().any(|x| x["role"] == json!("tool") || x["tool_calls"].is_array())).unwrap_or(false)
}

/// The refusal for a malformed tools field, or None.
pub fn check_tools(b: &Value) -> Option<String> {
    if b["tools"].is_null() { return None; }
    let Some(t) = b["tools"].as_array() else { return Some("\"tools\" must be an array.".into()) };
    if t.len() > MAX_TOOLS { return Some(format!("At most {MAX_TOOLS} tools.")); }
    for x in t {
        if x["type"] != json!("function") || st(&x["function"]["name"]).is_empty() { return Some("Each tool is { \"type\": \"function\", \"function\": { \"name\", \"description\", \"parameters\" } }.".into()); }
    }
    None
}

fn text_of(c: &Value) -> String {
    match c { Value::String(t) => t.clone(), Value::Array(a) => a.iter().filter_map(|x| if x["type"] == json!("text") { x["text"].as_str() } else { None }).collect(), _ => String::new() }
}

/// OpenAI messages as the provider gets them: only what OpenAI defines (role, content, tool_calls, tool_call_id, name).
pub fn clean_messages(b: &Value) -> Vec<Value> {
    b["messages"].as_array().map(|a| a.iter().filter(|m| ["system", "user", "assistant", "tool"].contains(&m["role"].as_str().unwrap_or(""))).map(|m| {
        let mut o = serde_json::Map::new();
        for k in ["role", "content", "tool_calls", "tool_call_id", "name"] { if !m[k].is_null() { o.insert(k.into(), m[k].clone()); } }
        if !o.contains_key("content") { o.insert("content".into(), Value::Null); }
        Value::Object(o)
    }).collect()).unwrap_or_default()
}

/// The arguments of a call as an object: OpenAI carries them as a JSON string.
fn args_obj(v: &Value) -> Value {
    match v {
        Value::String(t) => serde_json::from_str::<Value>(t).ok().filter(|x| x.is_object()).unwrap_or_else(|| json!({})),
        o if o.is_object() => o.clone(),
        _ => json!({}),
    }
}

/// The Anthropic request parts for an OpenAI chat request: (system text, messages, tools, tool_choice).
pub fn to_anthropic(b: &Value) -> (String, Vec<Value>, Vec<Value>, Option<Value>) {
    let mut system: Vec<String> = vec![];
    let mut out: Vec<Value> = vec![];
    let mut push = |role: &str, blocks: Vec<Value>| {
        if blocks.is_empty() { return; }
        if let Some(last) = out.last_mut() {
            if last["role"] == json!(role) { if let Some(c) = last["content"].as_array_mut() { c.extend(blocks); return; } }
        }
        out.push(json!({ "role": role, "content": blocks }));
    };
    for m in clean_messages(b) {
        match m["role"].as_str().unwrap_or("") {
            "system" => { let t = text_of(&m["content"]); if !t.is_empty() { system.push(t); } }
            "user" => { let t = text_of(&m["content"]); push("user", vec![json!({ "type": "text", "text": if t.is_empty() { "(no text)".to_string() } else { t } })]); }
            "assistant" => {
                let mut blocks = vec![];
                let t = text_of(&m["content"]); if !t.is_empty() { blocks.push(json!({ "type": "text", "text": t })); }
                for c in m["tool_calls"].as_array().cloned().unwrap_or_default() { blocks.push(json!({ "type": "tool_use", "id": st(&c["id"]), "name": st(&c["function"]["name"]), "input": args_obj(&c["function"]["arguments"]) })); }
                if blocks.is_empty() { blocks.push(json!({ "type": "text", "text": "(no text)" })); }
                push("assistant", blocks);
            }
            "tool" => { let t = text_of(&m["content"]); push("user", vec![json!({ "type": "tool_result", "tool_use_id": st(&m["tool_call_id"]), "content": t })]); }
            _ => {}
        }
    }
    let tools: Vec<Value> = b["tools"].as_array().map(|a| a.iter().map(|t| {
        let f = &t["function"];
        let schema = if f["parameters"].is_object() { f["parameters"].clone() } else { json!({ "type": "object", "properties": {} }) };
        json!({ "name": st(&f["name"]), "description": st(&f["description"]), "input_schema": schema })
    }).collect()).unwrap_or_default();
    let choice = match &b["tool_choice"] {
        Value::String(c) => match c.as_str() { "none" => Some(json!({ "type": "none" })), "required" => Some(json!({ "type": "any" })), "auto" => Some(json!({ "type": "auto" })), _ => None },
        o if o.is_object() && !st(&o["function"]["name"]).is_empty() => Some(json!({ "type": "tool", "name": st(&o["function"]["name"]) })),
        _ => None,
    };
    (system.join("\n\n"), out, tools, if b["tools"].as_array().map(|t| t.is_empty()).unwrap_or(true) { None } else { choice })
}

/// An Anthropic answer as (text, OpenAI tool_calls, finish_reason).
pub fn from_anthropic_answer(j: &Value) -> (String, Vec<Value>, String) {
    let mut text = String::new();
    let mut calls = vec![];
    for blk in j["content"].as_array().cloned().unwrap_or_default() {
        match blk["type"].as_str().unwrap_or("") {
            "text" => text.push_str(blk["text"].as_str().unwrap_or("")),
            "tool_use" => calls.push(json!({ "id": st(&blk["id"]), "type": "function", "function": { "name": st(&blk["name"]), "arguments": blk["input"].to_string() } })),
            _ => {}
        }
    }
    let finish = match j["stop_reason"].as_str().unwrap_or("") { "tool_use" => "tool_calls", "max_tokens" => "length", _ => if calls.is_empty() { "stop" } else { "tool_calls" } };
    (text, calls, finish.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_tools_and_refuses_malformed() {
        assert!(!wants_tools(&json!({ "messages": [{ "role": "user", "content": "x" }] })));
        assert!(!wants_tools(&json!({ "tools": [], "messages": [] })));
        assert!(wants_tools(&json!({ "tools": [{ "type": "function", "function": { "name": "a" } }] })));
        assert!(wants_tools(&json!({ "messages": [{ "role": "tool", "content": "r", "tool_call_id": "c1" }] })));
        assert!(check_tools(&json!({})).is_none());
        assert!(check_tools(&json!({ "tools": "x" })).is_some());
        assert!(check_tools(&json!({ "tools": [{ "type": "function" }] })).is_some());
        assert!(check_tools(&json!({ "tools": [{ "type": "function", "function": { "name": "a" } }] })).is_none());
    }

    #[test]
    fn openai_history_becomes_anthropic_blocks() {
        let b = json!({
            "tools": [{ "type": "function", "function": { "name": "re_tools", "description": "d", "parameters": { "type": "object", "properties": { "filter": { "type": "string" } } } } }],
            "tool_choice": "required",
            "messages": [
                { "role": "system", "content": "sys" },
                { "role": "user", "content": "list them" },
                { "role": "assistant", "content": null, "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "re_tools", "arguments": "{\"filter\":\"x\"}" } }, { "id": "c2", "type": "function", "function": { "name": "re_skill", "arguments": "not json" } }] },
                { "role": "tool", "tool_call_id": "c1", "content": "one" },
                { "role": "tool", "tool_call_id": "c2", "content": "two" }
            ]
        });
        let (sys, msgs, tools, choice) = to_anthropic(&b);
        assert_eq!(sys, "sys");
        assert_eq!(msgs.len(), 3, "the two results share one user message: {msgs:?}");
        assert_eq!(msgs[1]["content"][0], json!({ "type": "tool_use", "id": "c1", "name": "re_tools", "input": { "filter": "x" } }));
        assert_eq!(msgs[1]["content"][1]["input"], json!({}), "unparsable arguments become an empty object");
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0], json!({ "type": "tool_result", "tool_use_id": "c1", "content": "one" }));
        assert_eq!(msgs[2]["content"][1]["tool_use_id"], "c2");
        assert_eq!(tools[0]["name"], "re_tools");
        assert_eq!(tools[0]["input_schema"]["properties"]["filter"]["type"], "string");
        assert_eq!(choice, Some(json!({ "type": "any" })));
        let named = to_anthropic(&json!({ "tools": [{ "type": "function", "function": { "name": "a" } }], "tool_choice": { "type": "function", "function": { "name": "a" } }, "messages": [] }));
        assert_eq!(named.3, Some(json!({ "type": "tool", "name": "a" })));
    }

    #[test]
    fn anthropic_answer_becomes_tool_calls() {
        let (t, calls, fin) = from_anthropic_answer(&json!({ "stop_reason": "tool_use", "content": [{ "type": "text", "text": "ok " }, { "type": "tool_use", "id": "tu1", "name": "re_tools", "input": { "filter": "a" } }] }));
        assert_eq!((t.as_str(), fin.as_str()), ("ok ", "tool_calls"));
        assert_eq!(calls[0]["function"]["name"], "re_tools");
        assert_eq!(serde_json::from_str::<Value>(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap(), json!({ "filter": "a" }));
        assert_eq!(calls[0]["id"], "tu1");
        let (t2, c2, f2) = from_anthropic_answer(&json!({ "stop_reason": "end_turn", "content": [{ "type": "text", "text": "done" }] }));
        assert_eq!((t2.as_str(), c2.len(), f2.as_str()), ("done", 0, "stop"));
    }
}
