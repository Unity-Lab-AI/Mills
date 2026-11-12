//! Cross-format translation for the API gateway (T-369, G3, plans/api-gateway.md section 3.4): an OpenAI chat request to an
//! Anthropic-speaking provider and an Anthropic Messages request to an OpenAI-speaking one, requests, whole answers and
//! streams. Pure functions and small state machines (no I/O): `gateway.rs` carries the bytes.
//!
//! What is carried, dropped and refused (the rule: drop what is harmless, refuse with a 400 naming it what would change the
//! meaning of the request or of the answer):
//!
//! OpenAI -> Anthropic: carried: system / developer messages (text, joined into `system`), user text and images (a
//! `data:` URL becomes a base64 image block, an http(s) URL a url image block), assistant text and `tool_calls` (tool_use
//! blocks), `tool` messages (tool_result blocks, consecutive ones in one user message), tools, `tool_choice` (`parallel_tool_calls:
//! false` becomes `disable_parallel_tool_use`), `max_tokens` / `max_completion_tokens` (4096 when absent: Anthropic needs one),
//! `temperature` (clamped to 1), `top_p`, `stop`, `stream`, `response_format: json_object` (as an instruction in `system`).
//! Dropped (harmless): `user`, `seed`, `store`, `metadata`, `service_tier`, `frequency_penalty`, `presence_penalty`, `logit_bias`,
//! `top_logprobs`, `reasoning_effort`, `prediction`, `parallel_tool_calls: true`, `stream_options` (read by the gateway). Refused:
//! `n` above 1, `logprobs: true`, audio in or out, `file` parts, `response_format: json_schema`, `web_search_options`, a tool
//! that is not a function, a message role Anthropic has no equivalent for.
//!
//! Anthropic -> OpenAI: carried: `system` (string or text blocks), text, images (base64 to a data URL, url to a url), tool_use
//! (`tool_calls`, arguments as JSON text), tool_result (a `tool` message; images in a result go to the user message after it),
//! tools, `tool_choice` (`any` is `required`, `disable_parallel_tool_use` is `parallel_tool_calls: false`), `max_tokens`,
//! `temperature`, `top_p`, `stop_sequences` (as `stop`), `stream`. Dropped (harmless): `cache_control` anywhere, the
//! `thinking` request field and thinking / redacted_thinking blocks of earlier turns (the OpenAI model simply does not think
//! out loud), `top_k`, `metadata`, `service_tier`, `betas`. Refused: `document` blocks, server tools (web search, code
//! execution, computer use, bash, text editor ...) and their result blocks, `mcp_servers`, `container`, a tool_result with
//! blocks other than text and images.
use serde_json::{json, Map, Value};
use std::collections::HashMap;

use crate::toolconv::{args_obj, check_tools, choice_to_anthropic, tools_to_anthropic};

/// Anthropic needs a `max_tokens`: this when an OpenAI caller gave none.
pub const DEFAULT_MAX_TOKENS: u64 = 4096;

fn st(v: &Value) -> String { v.as_str().unwrap_or("").to_string() }
fn push_msg(out: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() { return; }
    if let Some(last) = out.last_mut() {
        if last["role"] == json!(role) { if let Some(c) = last["content"].as_array_mut() { c.extend(blocks); return; } }
    }
    out.push(json!({ "role": role, "content": blocks }));
}

/// Does this chat request need more than text (tools, images, structured output)? Such a request to an Anthropic provider goes
/// through the translator; a plain text one keeps the simple path.
pub fn needs_rich(b: &Value) -> bool {
    crate::toolconv::wants_tools(b) || b["messages"].as_array().map(|m| m.iter().any(|x| x["content"].as_array().map(|c| c.iter().any(|p| p["type"] != json!("text"))).unwrap_or(false))).unwrap_or(false)
}

// ---------- images ----------
const IMAGE_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];
/// An OpenAI `image_url` (a data URL or an address) as an Anthropic image block.
fn image_block(url: &str) -> Result<Value, String> {
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',').ok_or("An image data URL is not data:<type>;base64,<data>.")?;
        let mut it = meta.split(';'); let mt = it.next().unwrap_or("").to_ascii_lowercase();
        if !it.any(|x| x.eq_ignore_ascii_case("base64")) { return Err("An image data URL must be base64.".into()); }
        if !IMAGE_TYPES.contains(&mt.as_str()) { return Err(format!("The image type \"{mt}\" cannot be carried (jpeg, png, gif and webp can).")); }
        return Ok(json!({ "type": "image", "source": { "type": "base64", "media_type": mt, "data": data } }));
    }
    if url.starts_with("https://") || url.starts_with("http://") { return Ok(json!({ "type": "image", "source": { "type": "url", "url": url } })); }
    Err("An image must be a data: URL or an http(s) address.".into())
}
/// An Anthropic image block as an OpenAI `image_url` part.
fn image_part(b: &Value) -> Result<Value, String> {
    let src = &b["source"];
    match src["type"].as_str().unwrap_or("") {
        "base64" => Ok(json!({ "type": "image_url", "image_url": { "url": format!("data:{};base64,{}", st(&src["media_type"]), st(&src["data"])) } })),
        "url" => Ok(json!({ "type": "image_url", "image_url": { "url": st(&src["url"]) } })),
        t => Err(format!("An image with the source \"{t}\" cannot be carried to an OpenAI provider (base64 and url can).")),
    }
}

// ---------- OpenAI request -> Anthropic request ----------
fn oa_text(c: &Value, what: &str) -> Result<String, String> {
    match c {
        Value::Null => Ok(String::new()),
        Value::String(t) => Ok(t.clone()),
        Value::Array(a) => { let mut o = String::new(); for p in a { match p["type"].as_str().unwrap_or("") { "text" => o.push_str(p["text"].as_str().unwrap_or("")), "refusal" => {} t => return Err(format!("A \"{t}\" part in {what} cannot be carried.")) } } Ok(o) }
        _ => Err(format!("The content of {what} must be text.")),
    }
}
fn oa_user_blocks(c: &Value) -> Result<Vec<Value>, String> {
    let text = |t: &str| json!({ "type": "text", "text": t });
    match c {
        Value::Null => Ok(vec![]),
        Value::String(t) => Ok(if t.is_empty() { vec![] } else { vec![text(t)] }),
        Value::Array(a) => {
            let mut out = vec![];
            for p in a {
                match p["type"].as_str().unwrap_or("") {
                    "text" => { let t = p["text"].as_str().unwrap_or(""); if !t.is_empty() { out.push(text(t)); } }
                    "image_url" => { let u = if p["image_url"].is_string() { st(&p["image_url"]) } else { st(&p["image_url"]["url"]) }; out.push(image_block(&u)?); }
                    "input_audio" | "audio" => return Err("Audio input cannot be carried to an Anthropic provider.".into()),
                    "file" => return Err("File input parts cannot be carried to an Anthropic provider.".into()),
                    t => return Err(format!("A \"{t}\" content part cannot be carried to an Anthropic provider.")),
                }
            }
            Ok(out)
        }
        _ => Err("The content of a user message must be text or parts.".into()),
    }
}

/// An OpenAI chat request as an Anthropic Messages request for `model` (see the module notes for what is dropped and refused).
pub fn oa_to_ant_request(b: &Value, model: &str) -> Result<Value, String> {
    if b["n"].as_u64().map(|n| n > 1).unwrap_or(false) { return Err("\"n\" above 1 cannot be carried to an Anthropic provider (it answers once).".into()); }
    if b["logprobs"] == json!(true) { return Err("\"logprobs\" cannot be carried to an Anthropic provider.".into()); }
    if b["modalities"].as_array().map(|m| m.iter().any(|x| x != "text")).unwrap_or(false) || !b["audio"].is_null() { return Err("Audio output cannot be carried to an Anthropic provider.".into()); }
    if !b["web_search_options"].is_null() { return Err("\"web_search_options\" cannot be carried to an Anthropic provider.".into()); }
    if b["functions"].is_array() { return Err("The legacy \"functions\" field cannot be carried; use \"tools\".".into()); }
    if let Some(m) = check_tools(b) { return Err(m); }
    let mut system: Vec<String> = vec![];
    let mut json_note = false;
    match b["response_format"]["type"].as_str() {
        None | Some("text") => {}
        Some("json_object") => json_note = true,
        Some(t) => return Err(format!("response_format \"{t}\" cannot be carried to an Anthropic provider (json_object can).")),
    }
    let list = b["messages"].as_array().ok_or("\"messages\" is required.")?;
    let mut msgs: Vec<Value> = vec![];
    for m in list {
        match m["role"].as_str().unwrap_or("") {
            "system" | "developer" => { let t = oa_text(&m["content"], "a system message")?; if !t.is_empty() { system.push(t); } }
            "user" => push_msg(&mut msgs, "user", oa_user_blocks(&m["content"])?),
            "assistant" => {
                let mut blocks = vec![];
                let t = oa_text(&m["content"], "an assistant message")?; if !t.is_empty() { blocks.push(json!({ "type": "text", "text": t })); }
                for (i, c) in m["tool_calls"].as_array().cloned().unwrap_or_default().iter().enumerate() {
                    let id = st(&c["id"]);
                    blocks.push(json!({ "type": "tool_use", "id": if id.is_empty() { format!("toolu_h{i}") } else { id }, "name": st(&c["function"]["name"]), "input": args_obj(&c["function"]["arguments"]) }));
                }
                push_msg(&mut msgs, "assistant", blocks);
            }
            "tool" => push_msg(&mut msgs, "user", vec![json!({ "type": "tool_result", "tool_use_id": st(&m["tool_call_id"]), "content": oa_text(&m["content"], "a tool message")? })]),
            r => return Err(format!("The message role \"{r}\" cannot be carried to an Anthropic provider.")),
        }
    }
    if msgs.is_empty() { return Err("\"messages\" needs a user or assistant message.".into()); }
    if json_note { system.push("Respond with a single valid JSON object and nothing else.".into()); }
    let mut o = Map::new();
    o.insert("model".into(), json!(model));
    o.insert("max_tokens".into(), json!(b["max_completion_tokens"].as_u64().or(b["max_tokens"].as_u64()).filter(|n| *n > 0).unwrap_or(DEFAULT_MAX_TOKENS)));
    o.insert("messages".into(), json!(msgs));
    if !system.is_empty() { o.insert("system".into(), json!(system.join("\n\n"))); }
    if b["stream"] == json!(true) { o.insert("stream".into(), json!(true)); }
    if let Some(t) = b["temperature"].as_f64() { o.insert("temperature".into(), json!(t.clamp(0.0, 1.0))); }
    if let Some(t) = b["top_p"].as_f64() { o.insert("top_p".into(), json!(t)); }
    let stop: Vec<String> = match &b["stop"] { Value::String(t) => vec![t.clone()], Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(), _ => vec![] };
    let stop: Vec<String> = stop.into_iter().filter(|x| !x.is_empty()).collect();
    if !stop.is_empty() { o.insert("stop_sequences".into(), json!(stop)); }
    let tools = tools_to_anthropic(b);
    if !tools.is_empty() {
        o.insert("tools".into(), json!(tools));
        let mut choice = choice_to_anthropic(b);
        if b["parallel_tool_calls"] == json!(false) {
            let mut c = choice.take().unwrap_or_else(|| json!({ "type": "auto" }));
            if c["type"] != json!("none") { c["disable_parallel_tool_use"] = json!(true); }
            choice = Some(c);
        }
        if let Some(c) = choice { o.insert("tool_choice".into(), c); }
    }
    Ok(Value::Object(o))
}

// ---------- Anthropic request -> OpenAI request ----------
fn ant_text_of(c: &Value, what: &str) -> Result<(String, Vec<Value>), String> {
    // (text, images) of a tool_result's content or a system value
    match c {
        Value::Null => Ok((String::new(), vec![])),
        Value::String(t) => Ok((t.clone(), vec![])),
        Value::Array(a) => {
            let (mut t, mut imgs) = (String::new(), vec![]);
            for b in a { match b["type"].as_str().unwrap_or("") { "text" => t.push_str(b["text"].as_str().unwrap_or("")), "image" => imgs.push(image_part(b)?), x => return Err(format!("A \"{x}\" block in {what} cannot be carried to an OpenAI provider.")) } }
            Ok((t, imgs))
        }
        _ => Err(format!("The content of {what} must be text or blocks.")),
    }
}
/// An Anthropic Messages request as an OpenAI chat request for `model` (see the module notes for what is dropped and refused).
pub fn ant_to_oa_request(b: &Value, model: &str) -> Result<Value, String> {
    if b["mcp_servers"].as_array().map(|a| !a.is_empty()).unwrap_or(false) { return Err("\"mcp_servers\" cannot be carried to an OpenAI provider.".into()); }
    if !b["container"].is_null() { return Err("\"container\" (code execution) cannot be carried to an OpenAI provider.".into()); }
    let mut tools = vec![];
    for t in b["tools"].as_array().cloned().unwrap_or_default() {
        match t["type"].as_str() { None | Some("custom") => {} Some(x) => return Err(format!("The Anthropic server tool \"{}\" ({x}) cannot be carried to an OpenAI provider.", st(&t["name"]))) }
        let schema = if t["input_schema"].is_object() { t["input_schema"].clone() } else { json!({ "type": "object", "properties": {} }) };
        tools.push(json!({ "type": "function", "function": { "name": st(&t["name"]), "description": st(&t["description"]), "parameters": schema } }));
    }
    let mut out: Vec<Value> = vec![];
    let (sys, sys_imgs) = ant_text_of(&b["system"], "the system prompt")?;
    if !sys_imgs.is_empty() { return Err("The system prompt cannot carry images to an OpenAI provider.".into()); }
    if !sys.is_empty() { out.push(json!({ "role": "system", "content": sys })); }
    let list = b["messages"].as_array().ok_or("messages: at least one user or assistant message is required.")?;
    for m in list {
        let role = m["role"].as_str().unwrap_or("");
        let blocks: Vec<Value> = match &m["content"] { Value::String(t) => vec![json!({ "type": "text", "text": t })], Value::Array(a) => a.clone(), Value::Null => vec![], _ => return Err("A message's content must be text or blocks.".into()) };
        match role {
            "user" => {
                let mut parts: Vec<Value> = vec![]; let mut tool_msgs: Vec<Value> = vec![];
                for blk in &blocks {
                    match blk["type"].as_str().unwrap_or("") {
                        "text" => { let t = blk["text"].as_str().unwrap_or(""); if !t.is_empty() { parts.push(json!({ "type": "text", "text": t })); } }
                        "image" => parts.push(image_part(blk)?),
                        "tool_result" => {
                            let (t, imgs) = ant_text_of(&blk["content"], "a tool_result")?;
                            tool_msgs.push(json!({ "role": "tool", "tool_call_id": st(&blk["tool_use_id"]), "content": t }));
                            parts.extend(imgs); // OpenAI's tool messages are text: the pictures follow as the user's
                        }
                        "thinking" | "redacted_thinking" => {}
                        "document" => return Err("Anthropic document blocks cannot be carried to an OpenAI provider.".into()),
                        x => return Err(format!("A \"{x}\" block cannot be carried to an OpenAI provider.")),
                    }
                }
                out.extend(tool_msgs); // the results go straight after the assistant's calls
                if !parts.is_empty() {
                    let only_text = parts.iter().all(|p| p["type"] == json!("text"));
                    out.push(json!({ "role": "user", "content": if only_text { json!(parts.iter().map(|p| st(&p["text"])).collect::<Vec<_>>().join("\n")) } else { json!(parts) } }));
                }
            }
            "assistant" => {
                let (mut text, mut calls) = (String::new(), vec![]);
                for blk in &blocks {
                    match blk["type"].as_str().unwrap_or("") {
                        "text" => text.push_str(blk["text"].as_str().unwrap_or("")),
                        "tool_use" => calls.push(json!({ "id": st(&blk["id"]), "type": "function", "function": { "name": st(&blk["name"]), "arguments": if blk["input"].is_null() { "{}".to_string() } else { blk["input"].to_string() } } })),
                        "thinking" | "redacted_thinking" => {}
                        x => return Err(format!("A \"{x}\" block cannot be carried to an OpenAI provider.")),
                    }
                }
                if text.is_empty() && calls.is_empty() { continue; }
                let mut msg = json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) } });
                if !calls.is_empty() { msg["tool_calls"] = json!(calls); }
                out.push(msg);
            }
            r => return Err(format!("The message role \"{r}\" is not user or assistant.")),
        }
    }
    if !out.iter().any(|m| m["role"] != json!("system")) { return Err("messages: at least one user or assistant message is required.".into()); }
    let mut o = Map::new();
    o.insert("model".into(), json!(model));
    o.insert("messages".into(), json!(out));
    if let Some(n) = b["max_tokens"].as_u64().filter(|n| *n > 0) { o.insert("max_tokens".into(), json!(n)); }
    if let Some(t) = b["temperature"].as_f64() { o.insert("temperature".into(), json!(t)); }
    if let Some(t) = b["top_p"].as_f64() { o.insert("top_p".into(), json!(t)); }
    if let Some(a) = b["stop_sequences"].as_array() { let s: Vec<&Value> = a.iter().filter(|x| x.as_str().map(|t| !t.is_empty()).unwrap_or(false)).collect(); if !s.is_empty() { o.insert("stop".into(), json!(s)); } }
    if b["stream"] == json!(true) { o.insert("stream".into(), json!(true)); o.insert("stream_options".into(), json!({ "include_usage": true })); }
    if !tools.is_empty() {
        o.insert("tools".into(), json!(tools));
        let tc = &b["tool_choice"];
        match tc["type"].as_str().unwrap_or("") {
            "auto" => { o.insert("tool_choice".into(), json!("auto")); }
            "any" => { o.insert("tool_choice".into(), json!("required")); }
            "none" => { o.insert("tool_choice".into(), json!("none")); }
            "tool" if !st(&tc["name"]).is_empty() => { o.insert("tool_choice".into(), json!({ "type": "function", "function": { "name": st(&tc["name"]) } })); }
            _ => {}
        }
        if tc["disable_parallel_tool_use"] == json!(true) { o.insert("parallel_tool_calls".into(), json!(false)); }
    }
    Ok(Value::Object(o))
}

// ---------- usage ----------
/// Anthropic's usage object as OpenAI's (`prompt_tokens` counts the cache too).
pub fn oa_usage_from_ant(n: &Value) -> Value {
    let g = |k: &str| n[k].as_u64().unwrap_or(0);
    let cr = g("cache_read_input_tokens"); let p = g("input_tokens") + cr + g("cache_creation_input_tokens");
    json!({ "prompt_tokens": p, "completion_tokens": g("output_tokens"), "total_tokens": p + g("output_tokens"), "prompt_tokens_details": { "cached_tokens": cr } })
}
/// OpenAI's usage object as Anthropic's (`input_tokens` is the part not read from the cache).
pub fn ant_usage_from_oa(n: &Value) -> Value {
    let g = |k: &str| n[k].as_u64().unwrap_or(0);
    let cached = n["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
    json!({ "input_tokens": g("prompt_tokens").saturating_sub(cached), "output_tokens": g("completion_tokens"), "cache_read_input_tokens": cached, "cache_creation_input_tokens": 0 })
}
/// Anthropic's `stop_reason` as OpenAI's `finish_reason` (end_turn -> stop, tool_use -> tool_calls, max_tokens -> length).
pub fn finish_from_stop(stop: &str, had_tool: bool) -> &'static str {
    match stop { "tool_use" => "tool_calls", "max_tokens" => "length", "refusal" => "content_filter", "end_turn" | "stop_sequence" | "pause_turn" => "stop", _ => if had_tool { "tool_calls" } else { "stop" } }
}
/// OpenAI's `finish_reason` as Anthropic's `stop_reason`; a stop that came with tool calls is a tool_use.
pub fn stop_from_finish(finish: &str, had_tool: bool) -> &'static str {
    match finish { "tool_calls" | "function_call" => "tool_use", "length" => "max_tokens", "content_filter" => "refusal", _ => if had_tool { "tool_use" } else { "end_turn" } }
}

// ---------- whole answers ----------
/// An Anthropic message as a `chat.completion`.
pub fn ant_to_oa_whole(j: &Value, id: &str, created: u64, fallback_model: &str) -> Value {
    let (text, calls, _) = crate::toolconv::from_anthropic_answer(j);
    let finish = finish_from_stop(j["stop_reason"].as_str().unwrap_or(""), !calls.is_empty());
    let mut msg = json!({ "role": "assistant", "content": if text.is_empty() && !calls.is_empty() { Value::Null } else { json!(text) } });
    if !calls.is_empty() { msg["tool_calls"] = json!(calls); }
    let model = j["model"].as_str().filter(|m| !m.is_empty()).unwrap_or(fallback_model);
    json!({ "id": id, "object": "chat.completion", "created": created, "model": model, "choices": [{ "index": 0, "message": msg, "finish_reason": finish }], "usage": oa_usage_from_ant(&j["usage"]) })
}
/// A `chat.completion` as an Anthropic message.
pub fn oa_to_ant_whole(j: &Value, id: &str, fallback_model: &str) -> Value {
    let msg = &j["choices"][0]["message"];
    let mut content = vec![];
    if let Some(t) = msg["content"].as_str().filter(|t| !t.is_empty()) { content.push(json!({ "type": "text", "text": t })); }
    let mut had = false;
    for (i, c) in msg["tool_calls"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        had = true;
        let cid = st(&c["id"]);
        content.push(json!({ "type": "tool_use", "id": if cid.is_empty() { format!("toolu_{}_{i}", id.trim_start_matches("msg_")) } else { cid }, "name": st(&c["function"]["name"]), "input": args_obj(&c["function"]["arguments"]) }));
    }
    let stop = stop_from_finish(j["choices"][0]["finish_reason"].as_str().unwrap_or(""), had);
    let model = j["model"].as_str().filter(|m| !m.is_empty()).unwrap_or(fallback_model);
    json!({ "id": id, "type": "message", "role": "assistant", "model": model, "content": content, "stop_reason": stop, "stop_sequence": Value::Null, "usage": ant_usage_from_oa(&j["usage"]) })
}
/// A whole `chat.completion` as the stream a caller asked for: one chunk with the message, the finish chunk, (when asked)
/// the usage chunk, `[DONE]`. For an endpoint that answers JSON to a streaming request.
pub fn oa_whole_as_sse(j: &Value, include_usage: bool) -> String {
    let msg = &j["choices"][0]["message"];
    let mut delta = json!({ "role": "assistant", "content": msg["content"].as_str().unwrap_or("") });
    if let Some(c) = msg["tool_calls"].as_array().filter(|c| !c.is_empty()) { delta["tool_calls"] = json!(c.iter().enumerate().map(|(i, x)| { let mut x = x.clone(); x["index"] = json!(i); x }).collect::<Vec<_>>()); }
    let calls = delta["tool_calls"].is_array();
    let fin = j["choices"][0]["finish_reason"].as_str().map(String::from).unwrap_or_else(|| if calls { "tool_calls".into() } else { "stop".into() });
    let base = |choices: Value| json!({ "id": j["id"], "object": "chat.completion.chunk", "created": j["created"], "model": j["model"], "choices": choices });
    let mut s = format!("data: {}\n\n", base(json!([{ "index": 0, "delta": delta, "finish_reason": Value::Null }])));
    s.push_str(&format!("data: {}\n\n", base(json!([{ "index": 0, "delta": {}, "finish_reason": fin }]))));
    if include_usage && j["usage"].is_object() { let mut u = base(json!([])); u["usage"] = j["usage"].clone(); s.push_str(&format!("data: {u}\n\n")); }
    s.push_str("data: [DONE]\n\n");
    s
}

// ---------- streams ----------
/// The `data:` payloads of an SSE stream, bytes in any split (a line may arrive in pieces; CRLF is fine).
#[derive(Default)]
pub struct SseData { buf: Vec<u8> }
impl SseData {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = vec![];
        while let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line); let line = line.trim();
            if let Some(d) = line.strip_prefix("data:") { out.push(d.trim().to_string()); }
        }
        if self.buf.len() > 4_000_000 { self.buf.clear(); }
        out
    }
}
fn merge_numbers(into: &mut Value, u: &Value) {
    let Some(o) = u.as_object() else { return };
    if !into.is_object() { *into = json!({}); }
    for (k, v) in o { if v.is_number() { into[k] = v.clone(); } }
}
fn oa_frame(v: &Value) -> String { format!("data: {v}\n\n") }
fn ant_frame(t: &str, v: Value) -> String { let mut o = Map::new(); o.insert("type".into(), json!(t)); if let Some(m) = v.as_object() { for (k, x) in m { o.insert(k.clone(), x.clone()); } } format!("event: {t}\ndata: {}\n\n", Value::Object(o)) }

/// An Anthropic SSE stream as OpenAI `chat.completion.chunk` events: text deltas as `delta.content`, a tool_use block as a
/// `delta.tool_calls` entry (id and name first, then the arguments as they arrive), the stop reason as `finish_reason`, the
/// usage as a last chunk when the caller asked (`stream_options.include_usage`), then `[DONE]`.
pub struct AntToOa { id: String, req_model: String, created: u64, include_usage: bool, sse: SseData, started: bool, tools: HashMap<u64, usize>, had_tool: bool, stop: String, native: Value, model: String, done: bool }
impl AntToOa {
    pub fn new(id: &str, model: &str, created: u64, include_usage: bool) -> AntToOa { AntToOa { id: id.into(), req_model: model.into(), created, include_usage, sse: SseData::default(), started: false, tools: HashMap::new(), had_tool: false, stop: String::new(), native: Value::Null, model: String::new(), done: false } }
    /// The model the upstream named (else empty) and its usage as Anthropic gave it (for the ledger).
    pub fn model(&self) -> &str { &self.model }
    pub fn native(&self) -> &Value { &self.native }
    fn chunk(&self, delta: Value, finish: Value) -> String {
        oa_frame(&json!({ "id": self.id, "object": "chat.completion.chunk", "created": self.created, "model": if self.model.is_empty() { &self.req_model } else { &self.model }, "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] }))
    }
    fn start(&mut self, out: &mut Vec<String>) { if !self.started { self.started = true; out.push(self.chunk(json!({ "role": "assistant", "content": "" }), Value::Null)); } }
    pub fn push(&mut self, c: &[u8]) -> Vec<String> {
        let mut out = vec![];
        for d in self.sse.push(c) { if self.done { break; } if let Ok(ev) = serde_json::from_str::<Value>(&d) { self.event(&ev, &mut out); } }
        out
    }
    fn event(&mut self, ev: &Value, out: &mut Vec<String>) {
        let idx = ev["index"].as_u64().unwrap_or(0);
        match ev["type"].as_str().unwrap_or("") {
            "message_start" => { if let Some(m) = ev["message"]["model"].as_str().filter(|m| !m.is_empty()) { self.model = m.into(); } merge_numbers(&mut self.native, &ev["message"]["usage"]); self.start(out); }
            "content_block_start" => {
                self.start(out);
                let b = &ev["content_block"];
                if b["type"] == json!("tool_use") {
                    let t = self.tools.len(); self.tools.insert(idx, t); self.had_tool = true;
                    out.push(self.chunk(json!({ "tool_calls": [{ "index": t, "id": st(&b["id"]), "type": "function", "function": { "name": st(&b["name"]), "arguments": "" } }] }), Value::Null));
                }
            }
            "content_block_delta" => {
                self.start(out);
                let d = &ev["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => { if let Some(t) = d["text"].as_str().filter(|t| !t.is_empty()) { out.push(self.chunk(json!({ "content": t }), Value::Null)); } }
                    "input_json_delta" => { if let (Some(t), Some(p)) = (self.tools.get(&idx).copied(), d["partial_json"].as_str().filter(|p| !p.is_empty())) { out.push(self.chunk(json!({ "tool_calls": [{ "index": t, "function": { "arguments": p } }] }), Value::Null)); } }
                    _ => {} // thinking, signatures, citations: OpenAI's chunks have no place for them
                }
            }
            "message_delta" => { merge_numbers(&mut self.native, &ev["usage"]); if let Some(r) = ev["delta"]["stop_reason"].as_str() { self.stop = r.into(); } }
            "message_stop" => self.close(out),
            "error" => { out.push(oa_frame(&json!({ "error": { "message": st(&ev["error"]["message"]), "type": "api_error", "code": Value::Null } }))); out.push("data: [DONE]\n\n".into()); self.done = true; }
            _ => {}
        }
    }
    fn close(&mut self, out: &mut Vec<String>) {
        if self.done { return; }
        self.start(out);
        out.push(self.chunk(json!({}), json!(finish_from_stop(&self.stop, self.had_tool))));
        if self.include_usage && self.native.is_object() {
            out.push(oa_frame(&json!({ "id": self.id, "object": "chat.completion.chunk", "created": self.created, "model": if self.model.is_empty() { &self.req_model } else { &self.model }, "choices": [], "usage": oa_usage_from_ant(&self.native) })));
        }
        out.push("data: [DONE]\n\n".into()); self.done = true;
    }
    /// The upstream ended: whatever is open is finished (a stream cut short ends as a normal stop).
    pub fn finish(&mut self) -> Vec<String> { let mut out = vec![]; self.close(&mut out); out }
    pub fn error(&mut self, msg: &str) -> Vec<String> {
        if self.done { return vec![]; }
        self.done = true;
        vec![oa_frame(&json!({ "error": { "message": msg, "type": "api_error", "code": Value::Null } })), "data: [DONE]\n\n".into()]
    }
}

/// An OpenAI chat stream as Anthropic SSE events: `message_start`, text and tool_use content blocks (a call's arguments as
/// `input_json_delta`), `message_delta` with the stop reason and the usage, `message_stop`.
pub struct OaToAnt { id: String, req_model: String, sse: SseData, started: bool, open: Option<usize>, next: usize, tools: HashMap<u64, usize>, had_tool: bool, finish: String, native: Value, model: String, done: bool }
impl OaToAnt {
    pub fn new(id: &str, model: &str) -> OaToAnt { OaToAnt { id: id.into(), req_model: model.into(), sse: SseData::default(), started: false, open: None, next: 0, tools: HashMap::new(), had_tool: false, finish: String::new(), native: Value::Null, model: String::new(), done: false } }
    pub fn model(&self) -> &str { &self.model }
    /// The upstream's usage as OpenAI gave it (for the ledger).
    pub fn native(&self) -> &Value { &self.native }
    fn model_name(&self) -> &str { if self.model.is_empty() { &self.req_model } else { &self.model } }
    fn start(&mut self, out: &mut Vec<String>) {
        if self.started { return; } self.started = true;
        out.push(ant_frame("message_start", json!({ "message": { "id": self.id, "type": "message", "role": "assistant", "content": [], "model": self.model_name(), "stop_reason": Value::Null, "stop_sequence": Value::Null, "usage": { "input_tokens": 0, "output_tokens": 0 } } })));
    }
    fn close_block(&mut self, out: &mut Vec<String>) { if let Some(i) = self.open.take() { out.push(ant_frame("content_block_stop", json!({ "index": i }))); } }
    pub fn push(&mut self, c: &[u8]) -> Vec<String> {
        let mut out = vec![];
        for d in self.sse.push(c) {
            if self.done { break; }
            if d == "[DONE]" { self.close(&mut out); continue; }
            if let Ok(j) = serde_json::from_str::<Value>(&d) { self.chunk(&j, &mut out); }
        }
        out
    }
    fn chunk(&mut self, j: &Value, out: &mut Vec<String>) {
        if j["error"].is_object() { let m = j["error"]["message"].as_str().unwrap_or("the upstream failed").to_string(); out.extend(self.error(&m)); return; }
        if let Some(m) = j["model"].as_str().filter(|m| !m.is_empty() && !self.started) { self.model = m.into(); }
        if j["usage"].is_object() { self.native = j["usage"].clone(); }
        let ch = &j["choices"][0];
        if !ch.is_object() { return; }
        self.start(out);
        let d = &ch["delta"];
        if let Some(t) = d["content"].as_str().filter(|t| !t.is_empty()) {
            let idx = match self.open.filter(|i| !self.tools.values().any(|v| v == i)) {
                Some(i) => i,
                None => { self.close_block(out); let i = self.next; self.next += 1; self.open = Some(i); out.push(ant_frame("content_block_start", json!({ "index": i, "content_block": { "type": "text", "text": "" } }))); i }
            };
            out.push(ant_frame("content_block_delta", json!({ "index": idx, "delta": { "type": "text_delta", "text": t } })));
        }
        for (pos, tc) in d["tool_calls"].as_array().cloned().unwrap_or_default().iter().enumerate() {
            let oi = tc["index"].as_u64().unwrap_or(pos as u64);
            let block = match self.tools.get(&oi).copied() {
                Some(b) => b,
                None => {
                    self.close_block(out);
                    let b = self.next; self.next += 1; self.tools.insert(oi, b); self.open = Some(b); self.had_tool = true;
                    let id = st(&tc["id"]);
                    let id = if id.is_empty() { format!("toolu_{}_{b}", self.id.trim_start_matches("msg_")) } else { id };
                    out.push(ant_frame("content_block_start", json!({ "index": b, "content_block": { "type": "tool_use", "id": id, "name": st(&tc["function"]["name"]), "input": {} } })));
                    b
                }
            };
            if let Some(a) = tc["function"]["arguments"].as_str().filter(|a| !a.is_empty()) { out.push(ant_frame("content_block_delta", json!({ "index": block, "delta": { "type": "input_json_delta", "partial_json": a } }))); }
        }
        if let Some(f) = ch["finish_reason"].as_str().filter(|f| !f.is_empty()) { self.finish = f.into(); }
    }
    fn close(&mut self, out: &mut Vec<String>) {
        if self.done { return; }
        self.start(out); self.close_block(out);
        out.push(ant_frame("message_delta", json!({ "delta": { "stop_reason": stop_from_finish(&self.finish, self.had_tool), "stop_sequence": Value::Null }, "usage": ant_usage_from_oa(&self.native) })));
        out.push(ant_frame("message_stop", json!({})));
        self.done = true;
    }
    pub fn finish(&mut self) -> Vec<String> { let mut out = vec![]; self.close(&mut out); out }
    pub fn error(&mut self, msg: &str) -> Vec<String> {
        if self.done { return vec![]; }
        self.done = true;
        vec![ant_frame("error", json!({ "error": { "type": "api_error", "message": msg } }))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> Value { json!({ "type": "function", "function": { "name": name, "description": "d", "parameters": { "type": "object", "properties": { "q": { "type": "string" } } } } }) }

    #[test]
    fn an_openai_request_becomes_an_anthropic_one() {
        let b = json!({ "model": "alias", "max_completion_tokens": 77, "temperature": 1.7, "top_p": 0.9, "stop": "END", "stream": true, "user": "u", "seed": 3, "store": true, "parallel_tool_calls": false,
            "response_format": { "type": "json_object" },
            "tools": [tool("get")], "tool_choice": "required",
            "messages": [
                { "role": "system", "content": "be brief" }, { "role": "developer", "content": [{ "type": "text", "text": "dev" }] },
                { "role": "user", "content": [{ "type": "text", "text": "look" }, { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }, { "type": "image_url", "image_url": "https://x.test/a.jpg" }] },
                { "role": "assistant", "content": "ok", "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "get", "arguments": "{\"q\":\"a\"}" } }, { "id": "c2", "type": "function", "function": { "name": "get", "arguments": "{" } }] },
                { "role": "tool", "tool_call_id": "c1", "content": "one" }, { "role": "tool", "tool_call_id": "c2", "content": [{ "type": "text", "text": "two" }] },
                { "role": "user", "content": "thanks" }] });
        let o = oa_to_ant_request(&b, "claude-real").unwrap();
        assert_eq!(o["model"], "claude-real"); assert_eq!(o["max_tokens"], 77); assert_eq!(o["temperature"], 1.0); assert_eq!(o["top_p"], 0.9);
        assert_eq!(o["stop_sequences"], json!(["END"])); assert_eq!(o["stream"], true);
        assert_eq!(o["system"], "be brief\n\ndev\n\nRespond with a single valid JSON object and nothing else.");
        let m = o["messages"].as_array().unwrap();
        assert_eq!(m.len(), 3, "{m:?}");
        assert_eq!(m[0]["content"][1], json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } }));
        assert_eq!(m[0]["content"][2], json!({ "type": "image", "source": { "type": "url", "url": "https://x.test/a.jpg" } }));
        assert_eq!(m[1]["content"][1], json!({ "type": "tool_use", "id": "c1", "name": "get", "input": { "q": "a" } }));
        assert_eq!(m[1]["content"][2]["input"], json!({}));
        assert_eq!(m[2]["content"][0], json!({ "type": "tool_result", "tool_use_id": "c1", "content": "one" }));
        assert_eq!(m[2]["content"][1]["content"], "two"); assert_eq!(m[2]["content"][2], json!({ "type": "text", "text": "thanks" }));
        assert_eq!(o["tools"][0]["input_schema"]["properties"]["q"]["type"], "string");
        assert_eq!(o["tool_choice"], json!({ "type": "any", "disable_parallel_tool_use": true }));
        for k in ["user", "seed", "store", "parallel_tool_calls", "response_format", "stream_options"] { assert!(o.get(k).is_none(), "{k}"); }
        // defaults: a max_tokens, no stream, no tools
        let d = oa_to_ant_request(&json!({ "messages": [{ "role": "user", "content": "hi" }] }), "m").unwrap();
        assert_eq!(d["max_tokens"], DEFAULT_MAX_TOKENS); assert!(d.get("stream").is_none() && d.get("tools").is_none() && d.get("system").is_none());
    }

    #[test]
    fn an_openai_request_that_cannot_be_carried_is_refused_by_name() {
        let u = json!([{ "role": "user", "content": "x" }]);
        let cases: Vec<(Value, &str)> = vec![
            (json!({ "messages": u, "n": 2 }), "\"n\""), (json!({ "messages": u, "logprobs": true }), "logprobs"),
            (json!({ "messages": u, "modalities": ["text", "audio"] }), "Audio output"), (json!({ "messages": u, "response_format": { "type": "json_schema", "json_schema": {} } }), "json_schema"),
            (json!({ "messages": [{ "role": "user", "content": [{ "type": "input_audio", "input_audio": {} }] }] }), "Audio input"),
            (json!({ "messages": [{ "role": "user", "content": [{ "type": "file", "file": {} }] }] }), "File input"),
            (json!({ "messages": [{ "role": "user", "content": [{ "type": "image_url", "image_url": { "url": "data:image/bmp;base64,AA" } }] }] }), "image/bmp"),
            (json!({ "messages": [{ "role": "user", "content": [{ "type": "image_url", "image_url": { "url": "ftp://x" } }] }] }), "data: URL or an http"),
            (json!({ "messages": [{ "role": "robot", "content": "x" }] }), "robot"), (json!({ "messages": [] }), "needs a user"),
            (json!({ "messages": u, "tools": [{ "type": "custom" }] }), "Each tool"),
        ];
        for (b, want) in cases { let e = oa_to_ant_request(&b, "m").unwrap_err(); assert!(e.contains(want), "{b} -> {e}"); }
    }

    #[test]
    fn an_anthropic_request_becomes_an_openai_one() {
        let b = json!({ "model": "alias", "max_tokens": 99, "temperature": 0.4, "top_p": 0.8, "stop_sequences": ["A", "B"], "stream": true, "top_k": 4, "metadata": { "user_id": "u" },
            "thinking": { "type": "enabled", "budget_tokens": 1024 },
            "system": [{ "type": "text", "text": "one", "cache_control": { "type": "ephemeral" } }, { "type": "text", "text": "two" }],
            "tools": [{ "name": "get", "description": "d", "input_schema": { "type": "object" }, "cache_control": { "type": "ephemeral" } }], "tool_choice": { "type": "tool", "name": "get", "disable_parallel_tool_use": true },
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "look", "cache_control": { "type": "ephemeral" } }, { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } }] },
                { "role": "assistant", "content": [{ "type": "thinking", "thinking": "hm", "signature": "s" }, { "type": "text", "text": "calling" }, { "type": "tool_use", "id": "tu1", "name": "get", "input": { "q": "a" } }] },
                { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "tu1", "content": [{ "type": "text", "text": "res" }, { "type": "image", "source": { "type": "url", "url": "https://x.test/i.png" } }] }, { "type": "text", "text": "and?" }] }] });
        let o = ant_to_oa_request(&b, "gpt-real").unwrap();
        assert_eq!(o["model"], "gpt-real"); assert_eq!(o["max_tokens"], 99); assert_eq!(o["stop"], json!(["A", "B"])); assert_eq!(o["stream_options"], json!({ "include_usage": true }));
        assert_eq!(o["tool_choice"], json!({ "type": "function", "function": { "name": "get" } })); assert_eq!(o["parallel_tool_calls"], false);
        assert_eq!(o["tools"][0], json!({ "type": "function", "function": { "name": "get", "description": "d", "parameters": { "type": "object" } } }));
        let m = o["messages"].as_array().unwrap();
        assert_eq!(m[0], json!({ "role": "system", "content": "onetwo" }));
        assert_eq!(m[1]["content"][1]["image_url"]["url"], "data:image/png;base64,AAAA");
        assert_eq!(m[2], json!({ "role": "assistant", "content": "calling", "tool_calls": [{ "id": "tu1", "type": "function", "function": { "name": "get", "arguments": "{\"q\":\"a\"}" } }] }));
        assert_eq!(m[3], json!({ "role": "tool", "tool_call_id": "tu1", "content": "res" }));
        assert_eq!(m[4]["content"][0]["image_url"]["url"], "https://x.test/i.png"); assert_eq!(m[4]["content"][1]["text"], "and?");
        for k in ["top_k", "metadata", "thinking", "cache_control"] { assert!(!o.to_string().contains(k), "{k}"); }
        let d = ant_to_oa_request(&json!({ "max_tokens": 5, "messages": [{ "role": "user", "content": "hi" }, { "role": "assistant", "content": [{ "type": "tool_use", "id": "t", "name": "n", "input": null }] }] }), "m").unwrap();
        assert_eq!(d["messages"][0], json!({ "role": "user", "content": "hi" })); assert_eq!(d["messages"][1]["content"], Value::Null); assert_eq!(d["messages"][1]["tool_calls"][0]["function"]["arguments"], "{}");
        assert!(d.get("stream").is_none());
    }

    #[test]
    fn an_anthropic_request_that_cannot_be_carried_is_refused_by_name() {
        let u = json!([{ "role": "user", "content": "x" }]);
        let cases: Vec<(Value, &str)> = vec![
            (json!({ "messages": [{ "role": "user", "content": [{ "type": "document", "source": {} }] }] }), "document"),
            (json!({ "messages": u, "tools": [{ "type": "web_search_20250305", "name": "web_search" }] }), "web_search"),
            (json!({ "messages": u, "mcp_servers": [{ "name": "m" }] }), "mcp_servers"), (json!({ "messages": u, "container": "c" }), "container"),
            (json!({ "messages": [{ "role": "assistant", "content": [{ "type": "server_tool_use", "id": "s" }] }, { "role": "user", "content": "x" }] }), "server_tool_use"),
            (json!({ "messages": [{ "role": "user", "content": [{ "type": "image", "source": { "type": "file", "file_id": "f" } }] }] }), "file"),
            (json!({ "messages": [{ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t", "content": [{ "type": "document" }] }] }] }), "document"),
            (json!({ "messages": [] }), "at least one"),
        ];
        for (b, want) in cases { let e = ant_to_oa_request(&b, "m").unwrap_err(); assert!(e.contains(want), "{b} -> {e}"); }
    }

    #[test]
    fn finish_reasons_and_usage_map_both_ways() {
        for (s, tool, f) in [("end_turn", false, "stop"), ("tool_use", true, "tool_calls"), ("max_tokens", false, "length"), ("stop_sequence", false, "stop"), ("refusal", false, "content_filter"), ("", true, "tool_calls"), ("", false, "stop")] { assert_eq!(finish_from_stop(s, tool), f, "{s}"); }
        for (f, tool, s) in [("stop", false, "end_turn"), ("tool_calls", true, "tool_use"), ("length", false, "max_tokens"), ("stop", true, "tool_use"), ("content_filter", false, "refusal"), ("", false, "end_turn")] { assert_eq!(stop_from_finish(f, tool), s, "{f}"); }
        let a = oa_usage_from_ant(&json!({ "input_tokens": 10, "cache_read_input_tokens": 5, "cache_creation_input_tokens": 2, "output_tokens": 7 }));
        assert_eq!(a, json!({ "prompt_tokens": 17, "completion_tokens": 7, "total_tokens": 24, "prompt_tokens_details": { "cached_tokens": 5 } }));
        assert_eq!(ant_usage_from_oa(&json!({ "prompt_tokens": 17, "completion_tokens": 7, "prompt_tokens_details": { "cached_tokens": 5 } })), json!({ "input_tokens": 12, "output_tokens": 7, "cache_read_input_tokens": 5, "cache_creation_input_tokens": 0 }));
        assert_eq!(ant_usage_from_oa(&Value::Null)["input_tokens"], 0);
    }

    #[test]
    fn whole_answers_translate_both_ways() {
        let ant = json!({ "model": "claude-x", "stop_reason": "tool_use", "content": [{ "type": "text", "text": "t" }, { "type": "tool_use", "id": "tu", "name": "get", "input": { "q": 1 } }], "usage": { "input_tokens": 3, "output_tokens": 4 } });
        let o = ant_to_oa_whole(&ant, "chatcmpl-1", 5, "fallback");
        assert_eq!(o["choices"][0]["finish_reason"], "tool_calls"); assert_eq!(o["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"], "{\"q\":1}"); assert_eq!(o["model"], "claude-x"); assert_eq!(o["usage"]["total_tokens"], 7);
        let oa = json!({ "model": "gpt-x", "choices": [{ "message": { "role": "assistant", "content": null, "tool_calls": [{ "id": "c", "type": "function", "function": { "name": "get", "arguments": "{\"q\":2}" } }] }, "finish_reason": "tool_calls" }], "usage": { "prompt_tokens": 9, "completion_tokens": 2 } });
        let a = oa_to_ant_whole(&oa, "msg_1", "fb");
        assert_eq!(a["stop_reason"], "tool_use"); assert_eq!(a["content"], json!([{ "type": "tool_use", "id": "c", "name": "get", "input": { "q": 2 } }])); assert_eq!(a["usage"]["input_tokens"], 9);
        let plain = oa_to_ant_whole(&json!({ "choices": [{ "message": { "content": "hi" }, "finish_reason": "length" }] }), "msg_2", "fb");
        assert_eq!((plain["stop_reason"].as_str(), plain["model"].as_str(), plain["content"][0]["text"].as_str()), (Some("max_tokens"), Some("fb"), Some("hi")));
    }

    fn ev(t: &str, o: Value) -> String { let mut v = o; v["type"] = json!(t); format!("event: {t}\ndata: {v}\n\n") }
    fn ant_stream() -> String {
        [ev("message_start", json!({ "message": { "id": "m", "model": "claude-x", "usage": { "input_tokens": 11, "cache_read_input_tokens": 5, "output_tokens": 1 } } })), ev("ping", json!({})),
         ev("content_block_start", json!({ "index": 0, "content_block": { "type": "text", "text": "" } })), ev("content_block_delta", json!({ "index": 0, "delta": { "type": "text_delta", "text": "Hel" } })), ev("content_block_delta", json!({ "index": 0, "delta": { "type": "text_delta", "text": "lo é" } })), ev("content_block_stop", json!({ "index": 0 })),
         ev("content_block_start", json!({ "index": 1, "content_block": { "type": "thinking", "thinking": "" } })), ev("content_block_delta", json!({ "index": 1, "delta": { "type": "thinking_delta", "thinking": "x" } })), ev("content_block_stop", json!({ "index": 1 })),
         ev("content_block_start", json!({ "index": 2, "content_block": { "type": "tool_use", "id": "tu1", "name": "get", "input": {} } })), ev("content_block_delta", json!({ "index": 2, "delta": { "type": "input_json_delta", "partial_json": "" } })),
         ev("content_block_delta", json!({ "index": 2, "delta": { "type": "input_json_delta", "partial_json": "{\"city\":" } })), ev("content_block_delta", json!({ "index": 2, "delta": { "type": "input_json_delta", "partial_json": "\"Zürich\"}" } })), ev("content_block_stop", json!({ "index": 2 })),
         ev("content_block_start", json!({ "index": 3, "content_block": { "type": "tool_use", "id": "tu2", "name": "get", "input": {} } })), ev("content_block_delta", json!({ "index": 3, "delta": { "type": "input_json_delta", "partial_json": "{}" } })), ev("content_block_stop", json!({ "index": 3 })),
         ev("message_delta", json!({ "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 17 } })), ev("message_stop", json!({}))].concat()
    }
    fn data_of(frames: &[String]) -> Vec<Value> { frames.iter().filter_map(|f| f.strip_prefix("data: ")).filter(|d| !d.starts_with("[DONE]")).map(|d| serde_json::from_str(d.trim()).unwrap()).collect() }

    #[test]
    fn an_anthropic_stream_becomes_openai_chunks_whatever_the_split() {
        let sse = ant_stream();
        let mut reference: Option<Vec<String>> = None;
        for step in [1usize, 3, 7, 64, 100_000] {
            let mut c = AntToOa::new("chatcmpl-1", "req-model", 9, true); let mut frames = vec![];
            for p in sse.as_bytes().chunks(step) { frames.extend(c.push(p)); }
            frames.extend(c.finish());
            if let Some(r) = &reference { assert_eq!(&frames, r, "step {step}"); } else { reference = Some(frames.clone()); }
            let d = data_of(&frames);
            assert_eq!(d[0]["choices"][0]["delta"], json!({ "role": "assistant", "content": "" }));
            let text: String = d.iter().filter_map(|x| x["choices"][0]["delta"]["content"].as_str()).collect();
            assert_eq!(text, "Hello é");
            let tcs: Vec<Value> = d.iter().flat_map(|x| x["choices"][0]["delta"]["tool_calls"].as_array().cloned().unwrap_or_default()).collect();
            assert_eq!(tcs[0], json!({ "index": 0, "id": "tu1", "type": "function", "function": { "name": "get", "arguments": "" } }));
            let args0: String = tcs.iter().filter(|t| t["index"] == 0).filter_map(|t| t["function"]["arguments"].as_str()).collect();
            assert_eq!(serde_json::from_str::<Value>(&args0).unwrap(), json!({ "city": "Zürich" }));
            assert!(tcs.iter().any(|t| t["index"] == 1 && t["id"] == "tu2"), "the second call is index 1");
            let fin: Vec<&Value> = d.iter().filter(|x| !x["choices"][0]["finish_reason"].is_null()).collect();
            assert_eq!(fin.len(), 1); assert_eq!(fin[0]["choices"][0]["finish_reason"], "tool_calls");
            let last = d.last().unwrap(); assert_eq!(last["choices"], json!([])); assert_eq!(last["usage"], json!({ "prompt_tokens": 16, "completion_tokens": 17, "total_tokens": 33, "prompt_tokens_details": { "cached_tokens": 5 } }));
            assert_eq!(frames.last().unwrap(), "data: [DONE]\n\n"); assert_eq!(d[0]["model"], "claude-x");
            assert_eq!((c.model(), c.native()["output_tokens"].as_u64()), ("claude-x", Some(17)));
        }
        // no usage chunk when the caller did not ask for one
        let mut c = AntToOa::new("i", "m", 1, false); let mut frames = c.push(sse.as_bytes()); frames.extend(c.finish());
        assert!(data_of(&frames).iter().all(|x| x.get("usage").is_none()));
        // an error event ends the stream with an error and [DONE]; a stream cut short still ends properly
        let mut c = AntToOa::new("i", "m", 1, false); let f = c.push(ev("error", json!({ "error": { "type": "overloaded_error", "message": "busy" } })).as_bytes());
        assert!(f[0].contains("busy") && f.last().unwrap() == "data: [DONE]\n\n" && c.finish().is_empty());
        let mut c = AntToOa::new("i", "m", 1, false); c.push(ev("message_start", json!({ "message": { "model": "q" } })).as_bytes()); let f = c.finish();
        assert!(f[0].contains("\"finish_reason\":\"stop\"") && f.last().unwrap() == "data: [DONE]\n\n");
    }

    fn oa_chunk(delta: Value, finish: Value) -> String { format!("data: {}\n\n", json!({ "id": "c", "model": "gpt-x", "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })) }
    fn oa_stream() -> String {
        [oa_chunk(json!({ "role": "assistant", "content": "" }), Value::Null), oa_chunk(json!({ "content": "Hi " }), Value::Null), oa_chunk(json!({ "content": "there é" }), Value::Null),
         oa_chunk(json!({ "tool_calls": [{ "index": 0, "id": "call_1", "type": "function", "function": { "name": "get", "arguments": "" } }] }), Value::Null),
         oa_chunk(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "{\"city\":" } }] }), Value::Null), oa_chunk(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "\"Zürich\"}" } }] }), Value::Null),
         oa_chunk(json!({ "tool_calls": [{ "index": 1, "id": "call_2", "type": "function", "function": { "name": "get", "arguments": "{}" } }] }), Value::Null),
         oa_chunk(json!({}), json!("tool_calls")), format!("data: {}\n\n", json!({ "id": "c", "model": "gpt-x", "choices": [], "usage": { "prompt_tokens": 20, "completion_tokens": 6, "prompt_tokens_details": { "cached_tokens": 8 } } })), "data: [DONE]\n\n".to_string()].concat()
    }
    #[test]
    fn an_openai_stream_becomes_anthropic_events_whatever_the_split() {
        let sse = oa_stream(); let mut reference: Option<Vec<String>> = None;
        for step in [1usize, 5, 11, 200, 100_000] {
            let mut c = OaToAnt::new("msg_abc", "req"); let mut frames = vec![];
            for p in sse.as_bytes().chunks(step) { frames.extend(c.push(p)); }
            frames.extend(c.finish());
            if let Some(r) = &reference { assert_eq!(&frames, r, "step {step}"); } else { reference = Some(frames.clone()); }
            let kinds: Vec<String> = frames.iter().map(|f| f.lines().next().unwrap().trim_start_matches("event: ").to_string()).collect();
            assert_eq!(kinds.first().map(String::as_str), Some("message_start")); assert_eq!(kinds[kinds.len() - 2..], ["message_delta", "message_stop"]);
            assert_eq!(kinds.iter().filter(|k| *k == "content_block_start").count(), 3, "text, call 1, call 2: {kinds:?}");
            assert_eq!(kinds.iter().filter(|k| *k == "content_block_start").count(), kinds.iter().filter(|k| *k == "content_block_stop").count());
            let d: Vec<Value> = frames.iter().map(|f| serde_json::from_str(f.lines().nth(1).unwrap().trim_start_matches("data: ")).unwrap()).collect();
            assert_eq!(d[0]["message"]["model"], "gpt-x");
            let text: String = d.iter().filter(|x| x["delta"]["type"] == "text_delta").map(|x| x["delta"]["text"].as_str().unwrap()).collect(); assert_eq!(text, "Hi there é");
            let starts: Vec<&Value> = d.iter().filter(|x| x["type"] == "content_block_start").collect();
            assert_eq!(starts[1]["content_block"], json!({ "type": "tool_use", "id": "call_1", "name": "get", "input": {} })); assert_eq!((starts[1]["index"].as_u64(), starts[2]["index"].as_u64()), (Some(1), Some(2)));
            let args: String = d.iter().filter(|x| x["delta"]["type"] == "input_json_delta" && x["index"] == 1).map(|x| x["delta"]["partial_json"].as_str().unwrap()).collect();
            assert_eq!(serde_json::from_str::<Value>(&args).unwrap(), json!({ "city": "Zürich" }));
            let md = &d[d.len() - 2]; assert_eq!(md["delta"]["stop_reason"], "tool_use"); assert_eq!(md["usage"], json!({ "input_tokens": 12, "output_tokens": 6, "cache_read_input_tokens": 8, "cache_creation_input_tokens": 0 }));
            assert_eq!(c.native()["prompt_tokens"], 20);
        }
        // a stream with no [DONE] and no finish_reason still closes; an error chunk becomes an error event
        let mut c = OaToAnt::new("msg_x", "req"); let mut f = c.push(oa_chunk(json!({ "content": "a" }), Value::Null).as_bytes()); f.extend(c.finish());
        assert!(f.last().unwrap().starts_with("event: message_stop") && f.iter().any(|x| x.contains("\"stop_reason\":\"end_turn\"")));
        let mut c = OaToAnt::new("msg_x", "req"); let f = c.push(b"data: {\"error\":{\"message\":\"boom\"}}\n\n"); assert!(f[0].starts_with("event: error") && f[0].contains("boom") && c.finish().is_empty());
    }

    #[test]
    fn a_json_answer_to_a_streaming_request_is_streamed() {
        let j = json!({ "id": "x", "model": "m", "created": 1, "choices": [{ "message": { "role": "assistant", "content": null, "tool_calls": [{ "id": "c", "type": "function", "function": { "name": "n", "arguments": "{}" } }] }, "finish_reason": "tool_calls" }], "usage": { "prompt_tokens": 1, "completion_tokens": 2 } });
        let s = oa_whole_as_sse(&j, true); let d: Vec<Value> = s.lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]").map(|d| serde_json::from_str(d).unwrap()).collect();
        assert_eq!(d[0]["choices"][0]["delta"]["tool_calls"][0]["index"], 0); assert_eq!(d[1]["choices"][0]["finish_reason"], "tool_calls"); assert_eq!(d[2]["usage"]["completion_tokens"], 2); assert!(s.ends_with("data: [DONE]\n\n"));
        assert!(!oa_whole_as_sse(&j, false).contains("usage"));
        // and that stream goes on to Anthropic events
        let mut c = OaToAnt::new("msg_1", "r"); let mut f = c.push(s.as_bytes()); f.extend(c.finish());
        assert!(f.iter().any(|x| x.contains("\"type\":\"tool_use\"")) && f.iter().any(|x| x.contains("\"stop_reason\":\"tool_use\"")));
    }
}
