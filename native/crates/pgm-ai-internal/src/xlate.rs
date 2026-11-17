//! Cross-format translation for the API gateway (T-369, G3, plans/api-gateway.md section 3.4): an OpenAI chat request to an
//! Anthropic-speaking provider and an Anthropic Messages request to an OpenAI-speaking one, requests, whole answers and
//! streams; and (G5) a Responses API request to either (see "the Responses API" below). Pure functions and small state
//! machines (no I/O): `gateway.rs` carries the bytes.
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

// ---------- the Responses API (G5: Codex and other Responses clients) ----------
//
// `POST /v1/responses` to a provider that does not speak it natively: the request becomes a chat completions request
// ([`responses_to_chat`]) or, through that, an Anthropic one ([`responses_to_ant`]); the answer comes back as the Responses
// API's objects and SSE events ([`RespStream`], fed by [`ChatToResp`] or [`AntToResp`]).
//
// Carried: `instructions` (the system prompt), `input` as text or items (`message` with role user / system / developer /
// assistant and parts input_text / output_text / input_image; `function_call` {call_id, name, arguments};
// `function_call_output` {call_id, output}), function tools (name, description, parameters, strict), `tool_choice` (auto,
// none, required, a function), `parallel_tool_calls`, `max_output_tokens`, `temperature`, `top_p`, `stream`, `text.format`
// (json_object; json_schema to an OpenAI provider only, as chat's response_format rules). Dropped (harmless): `reasoning`
// items of earlier turns and the `reasoning` field (the provider does not think out loud in this shape), `store` (this hub keeps
// nothing, whatever it says), `include`, `metadata`, `user`, `truncation`, `service_tier`, `prompt_cache_key`,
// `safety_identifier`, `max_tool_calls`, `text.verbosity`, `top_logprobs`. Refused: state this hub does not keep
// (`previous_response_id`, `conversation`, `background`, `item_reference` items), a tool that is not a function (web_search,
// file_search, computer_use, code_interpreter, mcp, image_generation, local_shell, custom ...), `prompt` templates,
// `input_file` / audio parts and images by file_id, any other input item.

/// The message for a request that leans on stored state.
pub const NO_STATE: &str = "this hub keeps no response state; send the whole input";
/// `max_output_tokens` for an Anthropic provider when a Responses caller gave none (Codex never does; Anthropic needs one, and
/// an agent's edits need room).
pub const RESPONSES_DEFAULT_MAX_TOKENS: u64 = 16384;

/// Refuses what needs state this hub does not keep (every provider: nothing is stored, the routing may pick another provider).
pub fn responses_stateless(b: &Value) -> Result<(), String> {
    let set = |k: &str| !b[k].is_null() && b[k] != json!("") && b[k] != json!(false);
    if set("previous_response_id") { return Err(format!("{NO_STATE} (\"previous_response_id\" cannot be used).")); }
    if set("conversation") { return Err(format!("{NO_STATE} (\"conversation\" cannot be used).")); }
    if b["background"] == json!(true) { return Err(format!("{NO_STATE} (\"background\" responses are not kept to be fetched later).")); }
    if b["input"].as_array().map(|a| a.iter().any(|i| i["type"] == json!("item_reference"))).unwrap_or(false) { return Err(format!("{NO_STATE} (\"item_reference\" items cannot be resolved).")); }
    Ok(())
}
/// A Responses tool as a chat one; anything but a function is refused by name.
fn resp_tool(t: &Value) -> Result<Value, String> {
    match t["type"].as_str() {
        Some("function") => {
            let name = st(&t["name"]);
            if name.is_empty() { return Err("Each function tool needs a \"name\".".into()); }
            let params = if t["parameters"].is_object() { t["parameters"].clone() } else { json!({ "type": "object", "properties": {} }) };
            let mut f = json!({ "name": name, "description": st(&t["description"]), "parameters": params });
            if let Some(x) = t["strict"].as_bool() { f["strict"] = json!(x); }
            Ok(json!({ "type": "function", "function": f }))
        }
        Some(x) => Err(format!("The tool \"{x}\"{} cannot be carried: only function tools reach this provider (web_search, file_search, computer_use, code_interpreter, mcp, image_generation and the other hosted tools are OpenAI's own).", if st(&t["name"]).is_empty() { String::new() } else { format!(" ({})", st(&t["name"])) })),
        None => Err("Each tool needs a \"type\" (\"function\").".into()),
    }
}
/// A user (or tool output) content as chat parts: text and images.
fn resp_parts(c: &Value, what: &str) -> Result<Vec<Value>, String> {
    match c {
        Value::Null => Ok(vec![]),
        Value::String(t) => Ok(if t.is_empty() { vec![] } else { vec![json!({ "type": "text", "text": t })] }),
        Value::Array(a) => {
            let mut out = vec![];
            for p in a {
                match p["type"].as_str().unwrap_or("") {
                    "input_text" | "output_text" | "text" => { let t = st(&p["text"]); if !t.is_empty() { out.push(json!({ "type": "text", "text": t })); } }
                    "input_image" => {
                        let url = if p["image_url"].is_string() { st(&p["image_url"]) } else { st(&p["image_url"]["url"]) };
                        if url.is_empty() { return Err("An input_image must carry image_url (a data: URL or an address); one by file_id cannot be carried.".into()); }
                        let mut iu = json!({ "url": url });
                        if let Some(d) = p["detail"].as_str() { iu["detail"] = json!(d); }
                        out.push(json!({ "type": "image_url", "image_url": iu }));
                    }
                    "refusal" => {}
                    "input_file" => return Err(format!("An input_file part in {what} cannot be carried.")),
                    "input_audio" => return Err(format!("Audio in {what} cannot be carried.")),
                    t => return Err(format!("A \"{t}\" part in {what} cannot be carried.")),
                }
            }
            Ok(out)
        }
        _ => Err(format!("The content of {what} must be text or parts.")),
    }
}
fn only_text(parts: &[Value]) -> Option<String> { if parts.iter().all(|p| p["type"] == json!("text")) { Some(parts.iter().map(|p| st(&p["text"])).collect::<Vec<_>>().join("\n")) } else { None } }
/// A Responses request as an OpenAI chat request for `model` (see the notes above for what is carried, dropped and refused).
pub fn responses_to_chat(b: &Value, model: &str) -> Result<Value, String> {
    responses_stateless(b)?;
    if !b["prompt"].is_null() { return Err("\"prompt\" (a stored prompt template) cannot be carried; send instructions and input.".into()); }
    let mut tools = vec![];
    match &b["tools"] { Value::Null => {} Value::Array(a) => { for t in a { tools.push(resp_tool(t)?); } } _ => return Err("\"tools\" must be an array.".into()) }
    if tools.len() > crate::toolconv::MAX_TOOLS { return Err(format!("At most {} tools.", crate::toolconv::MAX_TOOLS)); }
    let mut msgs: Vec<Value> = vec![];
    if let Some(i) = b["instructions"].as_str().filter(|i| !i.is_empty()) { msgs.push(json!({ "role": "system", "content": i })); }
    let mut pending_imgs: Vec<Value> = vec![]; // pictures of tool outputs: a user message after the tool messages
    let flush = |msgs: &mut Vec<Value>, imgs: &mut Vec<Value>| { if !imgs.is_empty() { msgs.push(json!({ "role": "user", "content": std::mem::take(imgs) })); } };
    match &b["input"] {
        Value::Null => {}
        Value::String(t) => msgs.push(json!({ "role": "user", "content": t })),
        Value::Array(items) => for it in items {
            let ty = it["type"].as_str().unwrap_or(if it["role"].is_string() { "message" } else { "" });
            if ty != "function_call_output" { flush(&mut msgs, &mut pending_imgs); }
            match ty {
                "message" => match it["role"].as_str().unwrap_or("") {
                    "system" | "developer" => { let p = resp_parts(&it["content"], "a system message")?; let t = only_text(&p).ok_or("A system message can carry text only.")?; if !t.is_empty() { msgs.push(json!({ "role": "system", "content": t })); } }
                    "user" => { let p = resp_parts(&it["content"], "a user message")?; if !p.is_empty() { msgs.push(json!({ "role": "user", "content": match only_text(&p) { Some(t) => json!(t), None => json!(p) } })); } }
                    "assistant" => { let p = resp_parts(&it["content"], "an assistant message")?; let t = only_text(&p).ok_or("An assistant message can carry text only.")?; if !t.is_empty() { msgs.push(json!({ "role": "assistant", "content": t })); } }
                    r => return Err(format!("The message role \"{r}\" cannot be carried.")),
                },
                "function_call" => {
                    let args = match &it["arguments"] { Value::String(a) => a.clone(), Value::Null => "{}".into(), v => v.to_string() };
                    let call = json!({ "id": st(&it["call_id"]), "type": "function", "function": { "name": st(&it["name"]), "arguments": args } });
                    match msgs.last_mut().filter(|m| m["role"] == json!("assistant")) {
                        Some(m) => { if !m["tool_calls"].is_array() { m["tool_calls"] = json!([]); } if let Some(a) = m["tool_calls"].as_array_mut() { a.push(call); } }
                        None => msgs.push(json!({ "role": "assistant", "content": Value::Null, "tool_calls": [call] })),
                    }
                }
                "function_call_output" => {
                    let p = resp_parts(&it["output"], "a function_call_output")?;
                    let (texts, imgs): (Vec<Value>, Vec<Value>) = p.into_iter().partition(|x| x["type"] == json!("text"));
                    msgs.push(json!({ "role": "tool", "tool_call_id": st(&it["call_id"]), "content": only_text(&texts).unwrap_or_default() }));
                    pending_imgs.extend(imgs);
                }
                "reasoning" => {} // an earlier turn's thinking: the provider has no place for it
                "" => return Err("Each input item needs a \"type\" (message, function_call, function_call_output).".into()),
                t => return Err(format!("A \"{t}\" input item cannot be carried (message, function_call and function_call_output items can).")),
            }
        },
        _ => return Err("\"input\" must be text or a list of items.".into()),
    }
    flush(&mut msgs, &mut pending_imgs);
    if !msgs.iter().any(|m| m["role"] != json!("system")) { return Err("\"input\" needs at least one message.".into()); }
    let mut o = Map::new();
    o.insert("model".into(), json!(model));
    o.insert("messages".into(), json!(msgs));
    if let Some(n) = b["max_output_tokens"].as_u64().filter(|n| *n > 0) { o.insert("max_tokens".into(), json!(n)); }
    if let Some(t) = b["temperature"].as_f64() { o.insert("temperature".into(), json!(t)); }
    if let Some(t) = b["top_p"].as_f64() { o.insert("top_p".into(), json!(t)); }
    if b["stream"] == json!(true) { o.insert("stream".into(), json!(true)); o.insert("stream_options".into(), json!({ "include_usage": true })); }
    let f = &b["text"]["format"];
    match f["type"].as_str() {
        None | Some("text") => {}
        Some("json_object") => { o.insert("response_format".into(), json!({ "type": "json_object" })); }
        Some("json_schema") => {
            let mut js = json!({ "name": if st(&f["name"]).is_empty() { "response".to_string() } else { st(&f["name"]) }, "schema": if f["schema"].is_object() { f["schema"].clone() } else { json!({}) } });
            if let Some(x) = f["strict"].as_bool() { js["strict"] = json!(x); }
            if let Some(d) = f["description"].as_str() { js["description"] = json!(d); }
            o.insert("response_format".into(), json!({ "type": "json_schema", "json_schema": js }));
        }
        Some(t) => return Err(format!("text.format \"{t}\" cannot be carried (text, json_object and json_schema can).")),
    }
    if !tools.is_empty() {
        o.insert("tools".into(), json!(tools));
        match &b["tool_choice"] {
            Value::Null => {}
            Value::String(c) if ["auto", "none", "required"].contains(&c.as_str()) => { o.insert("tool_choice".into(), json!(c)); }
            c if c["type"] == json!("function") && !st(&c["name"]).is_empty() => { o.insert("tool_choice".into(), json!({ "type": "function", "function": { "name": st(&c["name"]) } })); }
            c => return Err(format!("tool_choice {} cannot be carried (auto, none, required and a function can).", c["type"].as_str().map(|t| format!("\"{t}\"")).unwrap_or_else(|| c.to_string()))),
        }
        if let Some(p) = b["parallel_tool_calls"].as_bool() { o.insert("parallel_tool_calls".into(), json!(p)); }
    }
    Ok(Value::Object(o))
}
/// A Responses request as an Anthropic Messages request for `model`: through [`responses_to_chat`] and [`oa_to_ant_request`]
/// (the same carrying and refusals as a chat request to an Anthropic provider), `max_output_tokens` defaulting to
/// [`RESPONSES_DEFAULT_MAX_TOKENS`].
pub fn responses_to_ant(b: &Value, model: &str) -> Result<Value, String> {
    if b["text"]["format"]["type"] == json!("json_schema") { return Err("text.format \"json_schema\" cannot be carried to an Anthropic provider (json_object can).".into()); }
    let mut chat = responses_to_chat(b, model)?;
    if chat["max_tokens"].is_null() { chat["max_tokens"] = json!(RESPONSES_DEFAULT_MAX_TOKENS); }
    oa_to_ant_request(&chat, model)
}

/// The fields of a Responses request that its response object echoes (the rest of the object is the answer's own).
pub fn response_base(b: &Value) -> Value {
    let or = |k: &str, d: Value| if b[k].is_null() { d } else { b[k].clone() };
    json!({ "instructions": or("instructions", Value::Null), "max_output_tokens": or("max_output_tokens", Value::Null), "parallel_tool_calls": b["parallel_tool_calls"].as_bool().unwrap_or(true),
        "previous_response_id": Value::Null, "reasoning": { "effort": b["reasoning"]["effort"].clone(), "summary": Value::Null }, "store": false, "temperature": or("temperature", json!(1.0)),
        "text": { "format": if b["text"]["format"].is_object() { b["text"]["format"].clone() } else { json!({ "type": "text" }) } }, "tool_choice": or("tool_choice", json!("auto")), "tools": or("tools", json!([])),
        "top_p": or("top_p", json!(1.0)), "truncation": "disabled", "user": Value::Null, "metadata": if b["metadata"].is_object() { b["metadata"].clone() } else { json!({}) } })
}
/// OpenAI chat usage as the Responses API's (`input_tokens` counts the cached ones too).
pub fn resp_usage_from_oa(n: &Value) -> Value {
    let g = |v: &Value| v.as_u64().unwrap_or(0);
    let (i, o) = (g(&n["prompt_tokens"]), g(&n["completion_tokens"]));
    json!({ "input_tokens": i, "input_tokens_details": { "cached_tokens": g(&n["prompt_tokens_details"]["cached_tokens"]) }, "output_tokens": o, "output_tokens_details": { "reasoning_tokens": g(&n["completion_tokens_details"]["reasoning_tokens"]) }, "total_tokens": i + o })
}
/// Anthropic's usage as the Responses API's (`input_tokens` counts the cache read and written too).
pub fn resp_usage_from_ant(n: &Value) -> Value {
    let g = |k: &str| n[k].as_u64().unwrap_or(0);
    let i = g("input_tokens") + g("cache_read_input_tokens") + g("cache_creation_input_tokens");
    json!({ "input_tokens": i, "input_tokens_details": { "cached_tokens": g("cache_read_input_tokens") }, "output_tokens": g("output_tokens"), "output_tokens_details": { "reasoning_tokens": 0 }, "total_tokens": i + g("output_tokens") })
}

/// A Responses answer as it is made: the output items (an assistant message, function calls) and the SSE events that tell
/// them, each with its `type` and a `sequence_number` from 0 up: `response.created`, `response.in_progress`; per message
/// `response.output_item.added`, `response.content_part.added`, `response.output_text.delta` ..., `response.output_text.done`,
/// `response.content_part.done`, `response.output_item.done`; per call `response.output_item.added`,
/// `response.function_call_arguments.delta` ..., `response.function_call_arguments.done`, `response.output_item.done`; then
/// `response.completed` (or `response.incomplete` with `incomplete_details.reason`, as OpenAI ends an answer cut by
/// max_output_tokens) with the whole response object, or `response.failed` with its error.
pub struct RespStream { id: String, created: u64, model: String, base: Value, seq: u64, started: bool, done: bool, items: Vec<Value>, open: Option<usize>, usage: Value, reason: String, n: usize }
impl RespStream {
    pub fn new(id: &str, created: u64, model: &str, base: Value) -> RespStream { RespStream { id: id.into(), created, model: model.into(), base, seq: 0, started: false, done: false, items: vec![], open: None, usage: Value::Null, reason: String::new(), n: 0 } }
    pub fn set_model(&mut self, m: &str) { if !m.is_empty() { self.model = m.into(); } }
    pub fn set_usage(&mut self, u: Value) { self.usage = u; }
    /// Why the answer stopped short ("" = it did not): max_output_tokens, content_filter.
    pub fn set_reason(&mut self, r: &str) { self.reason = r.into(); }
    fn frame(&mut self, t: &str, v: Value) -> String {
        let mut o = Map::new(); o.insert("type".into(), json!(t)); o.insert("sequence_number".into(), json!(self.seq)); self.seq += 1;
        if let Some(m) = v.as_object() { for (k, x) in m { o.insert(k.clone(), x.clone()); } }
        format!("event: {t}\ndata: {}\n\n", Value::Object(o))
    }
    /// The response object with that status (the output items that are done, the usage once known).
    pub fn response(&self, status: &str) -> Value {
        let mut r = self.base.clone();
        r["id"] = json!(self.id); r["object"] = json!("response"); r["created_at"] = json!(self.created); r["status"] = json!(status); r["model"] = json!(self.model);
        r["error"] = Value::Null; r["incomplete_details"] = Value::Null;
        r["output"] = json!(self.items.iter().filter(|i| i["status"] == json!("completed")).cloned().collect::<Vec<_>>());
        r["usage"] = if status == "in_progress" { Value::Null } else { self.usage.clone() };
        if status == "incomplete" { r["incomplete_details"] = json!({ "reason": self.reason }); }
        r
    }
    /// `response.created` and `response.in_progress` (once): as soon as the upstream answers, so the caller sees the answer begin.
    pub fn start(&mut self, out: &mut Vec<String>) {
        if self.started || self.done { return; } self.started = true;
        let r = self.response("in_progress");
        out.push(self.frame("response.created", json!({ "response": r })));
        out.push(self.frame("response.in_progress", json!({ "response": r })));
    }
    fn item_id(&mut self, pre: &str) -> String { self.n += 1; format!("{pre}_{}_{}", self.id.trim_start_matches("resp_"), self.n) }
    fn close_item(&mut self, out: &mut Vec<String>) {
        let Some(i) = self.open.take() else { return };
        let id = st(&self.items[i]["id"]);
        if self.items[i]["type"] == json!("message") {
            let text = st(&self.items[i]["content"][0]["text"]);
            out.push(self.frame("response.output_text.done", json!({ "item_id": id, "output_index": i, "content_index": 0, "text": text, "logprobs": [] })));
            out.push(self.frame("response.content_part.done", json!({ "item_id": id, "output_index": i, "content_index": 0, "part": { "type": "output_text", "text": text, "annotations": [] } })));
        } else {
            let (args, name) = (st(&self.items[i]["arguments"]), st(&self.items[i]["name"]));
            out.push(self.frame("response.function_call_arguments.done", json!({ "item_id": id, "output_index": i, "name": name, "arguments": args })));
        }
        self.items[i]["status"] = json!("completed");
        let item = self.items[i].clone();
        out.push(self.frame("response.output_item.done", json!({ "output_index": i, "item": item })));
    }
    /// A piece of the assistant's text (a message item is opened for it when none is).
    pub fn text(&mut self, out: &mut Vec<String>, t: &str) {
        if self.done || t.is_empty() { return; }
        self.start(out);
        let i = match self.open.filter(|i| self.items[*i]["type"] == json!("message")) {
            Some(i) => i,
            None => {
                self.close_item(out);
                let id = self.item_id("msg"); let i = self.items.len();
                self.items.push(json!({ "id": id, "type": "message", "status": "in_progress", "role": "assistant", "content": [] }));
                out.push(self.frame("response.output_item.added", json!({ "output_index": i, "item": self.items[i] })));
                self.items[i]["content"] = json!([{ "type": "output_text", "text": "", "annotations": [] }]);
                out.push(self.frame("response.content_part.added", json!({ "item_id": id, "output_index": i, "content_index": 0, "part": { "type": "output_text", "text": "", "annotations": [] } })));
                self.open = Some(i); i
            }
        };
        let id = st(&self.items[i]["id"]);
        let all = st(&self.items[i]["content"][0]["text"]) + t; self.items[i]["content"][0]["text"] = json!(all);
        out.push(self.frame("response.output_text.delta", json!({ "item_id": id, "output_index": i, "content_index": 0, "delta": t, "logprobs": [] })));
    }
    /// A function call begins: its output index.
    pub fn call_start(&mut self, out: &mut Vec<String>, call_id: &str, name: &str) -> usize {
        self.start(out); self.close_item(out);
        let id = self.item_id("fc"); let i = self.items.len();
        let call_id = if call_id.is_empty() { format!("call_{}", id.trim_start_matches("fc_")) } else { call_id.to_string() };
        self.items.push(json!({ "id": id, "type": "function_call", "status": "in_progress", "arguments": "", "call_id": call_id, "name": name }));
        out.push(self.frame("response.output_item.added", json!({ "output_index": i, "item": self.items[i] })));
        self.open = Some(i); i
    }
    /// A piece of a call's arguments (a call already closed takes it in quietly: its done event has gone).
    pub fn call_args(&mut self, out: &mut Vec<String>, i: usize, d: &str) {
        if self.done || d.is_empty() || i >= self.items.len() { return; }
        let all = st(&self.items[i]["arguments"]) + d; self.items[i]["arguments"] = json!(all);
        if self.open == Some(i) { let id = st(&self.items[i]["id"]); out.push(self.frame("response.function_call_arguments.delta", json!({ "item_id": id, "output_index": i, "delta": d }))); }
    }
    /// The end: what is open is closed and `response.completed` (or `response.incomplete`) carries the whole response.
    pub fn complete(&mut self, out: &mut Vec<String>) {
        if self.done { return; }
        self.start(out); self.close_item(out); self.done = true;
        if self.reason.is_empty() { let r = self.response("completed"); out.push(self.frame("response.completed", json!({ "response": r }))); }
        else { let r = self.response("incomplete"); out.push(self.frame("response.incomplete", json!({ "response": r }))); }
    }
    /// The upstream failed: `response.failed` with the error (what was done so far stays in its output).
    pub fn fail(&mut self, out: &mut Vec<String>, msg: &str) {
        if self.done { return; }
        self.start(out); self.close_item(out); self.done = true;
        let mut r = self.response("failed"); r["error"] = json!({ "code": "server_error", "message": msg });
        out.push(self.frame("response.failed", json!({ "response": r })));
    }
    /// The response object as it ended (for a whole answer).
    pub fn final_response(&self) -> Value { if self.reason.is_empty() { self.response("completed") } else { self.response("incomplete") } }
}
/// Why an answer stopped short, in the Responses API's words, from OpenAI's finish_reason or Anthropic's stop_reason.
fn short_reason(r: &str) -> &'static str { match r { "length" | "max_tokens" => "max_output_tokens", "content_filter" | "refusal" => "content_filter", _ => "" } }

/// An OpenAI chat stream as Responses events.
pub struct ChatToResp { sse: SseData, em: RespStream, calls: HashMap<u64, usize>, finish: String, native: Value, model: String }
impl ChatToResp {
    pub fn new(em: RespStream) -> ChatToResp { ChatToResp { sse: SseData::default(), em, calls: HashMap::new(), finish: String::new(), native: Value::Null, model: String::new() } }
    pub fn model(&self) -> &str { &self.model }
    /// The upstream's usage as OpenAI gave it (for the ledger).
    pub fn native(&self) -> &Value { &self.native }
    pub fn push(&mut self, c: &[u8]) -> Vec<String> {
        let mut out = vec![];
        for d in self.sse.push(c) {
            if self.em.done { break; }
            if d == "[DONE]" { self.close(&mut out); continue; }
            if let Ok(j) = serde_json::from_str::<Value>(&d) { self.chunk(&j, &mut out); }
        }
        out
    }
    fn chunk(&mut self, j: &Value, out: &mut Vec<String>) {
        if j["error"].is_object() { let m = j["error"]["message"].as_str().unwrap_or("the upstream failed").to_string(); self.em.fail(out, &m); return; }
        if let Some(m) = j["model"].as_str().filter(|m| !m.is_empty() && self.model.is_empty()) { self.model = m.into(); self.em.set_model(m); }
        if j["usage"].is_object() { self.native = j["usage"].clone(); }
        self.em.start(out);
        let ch = &j["choices"][0];
        if !ch.is_object() { return; }
        let d = &ch["delta"];
        if let Some(t) = d["content"].as_str() { self.em.text(out, t); }
        for (pos, tc) in d["tool_calls"].as_array().cloned().unwrap_or_default().iter().enumerate() {
            let oi = tc["index"].as_u64().unwrap_or(pos as u64);
            let i = match self.calls.get(&oi).copied() { Some(i) => i, None => { let i = self.em.call_start(out, &st(&tc["id"]), &st(&tc["function"]["name"])); self.calls.insert(oi, i); i } };
            if let Some(a) = tc["function"]["arguments"].as_str() { self.em.call_args(out, i, a); }
        }
        if let Some(f) = ch["finish_reason"].as_str().filter(|f| !f.is_empty()) { self.finish = f.into(); }
    }
    fn close(&mut self, out: &mut Vec<String>) {
        if self.em.done { return; }
        self.em.set_usage(resp_usage_from_oa(&self.native)); self.em.set_reason(short_reason(&self.finish));
        self.em.complete(out);
    }
    /// The upstream ended (a stream cut short without [DONE] still ends as an answer).
    pub fn finish(&mut self) -> Vec<String> { let mut out = vec![]; self.close(&mut out); out }
    pub fn error(&mut self, msg: &str) -> Vec<String> { let mut out = vec![]; self.em.fail(&mut out, msg); out }
}
/// A whole `chat.completion` as a response object.
pub fn chat_to_response_whole(j: &Value, mut em: RespStream) -> Value {
    let mut sink = vec![];
    if let Some(m) = j["model"].as_str() { em.set_model(m); }
    let msg = &j["choices"][0]["message"];
    if let Some(t) = msg["content"].as_str() { em.text(&mut sink, t); }
    for c in msg["tool_calls"].as_array().cloned().unwrap_or_default() {
        let i = em.call_start(&mut sink, &st(&c["id"]), &st(&c["function"]["name"]));
        let a = match &c["function"]["arguments"] { Value::String(a) => a.clone(), Value::Null => String::new(), v => v.to_string() };
        em.call_args(&mut sink, i, &a);
    }
    em.set_usage(resp_usage_from_oa(&j["usage"])); em.set_reason(short_reason(j["choices"][0]["finish_reason"].as_str().unwrap_or("")));
    em.complete(&mut sink);
    em.final_response()
}

/// An Anthropic stream as Responses events (thinking blocks are left out: the Responses caller asked no reasoning of it).
pub struct AntToResp { sse: SseData, em: RespStream, blocks: HashMap<u64, usize>, stop: String, native: Value, model: String }
impl AntToResp {
    pub fn new(em: RespStream) -> AntToResp { AntToResp { sse: SseData::default(), em, blocks: HashMap::new(), stop: String::new(), native: Value::Null, model: String::new() } }
    pub fn model(&self) -> &str { &self.model }
    /// The upstream's usage as Anthropic gave it (for the ledger).
    pub fn native(&self) -> &Value { &self.native }
    pub fn push(&mut self, c: &[u8]) -> Vec<String> {
        let mut out = vec![];
        for d in self.sse.push(c) { if self.em.done { break; } if let Ok(ev) = serde_json::from_str::<Value>(&d) { self.event(&ev, &mut out); } }
        out
    }
    fn event(&mut self, ev: &Value, out: &mut Vec<String>) {
        let idx = ev["index"].as_u64().unwrap_or(0);
        match ev["type"].as_str().unwrap_or("") {
            "message_start" => { if let Some(m) = ev["message"]["model"].as_str().filter(|m| !m.is_empty()) { self.model = m.into(); self.em.set_model(m); } merge_numbers(&mut self.native, &ev["message"]["usage"]); self.em.start(out); }
            "content_block_start" => { let b = &ev["content_block"]; if b["type"] == json!("tool_use") { let i = self.em.call_start(out, &st(&b["id"]), &st(&b["name"])); self.blocks.insert(idx, i); } }
            "content_block_delta" => {
                let d = &ev["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => { if let Some(t) = d["text"].as_str() { self.em.text(out, t); } }
                    "input_json_delta" => { if let (Some(i), Some(p)) = (self.blocks.get(&idx).copied(), d["partial_json"].as_str()) { self.em.call_args(out, i, p); } }
                    _ => {}
                }
            }
            "message_delta" => { merge_numbers(&mut self.native, &ev["usage"]); if let Some(r) = ev["delta"]["stop_reason"].as_str() { self.stop = r.into(); } }
            "message_stop" => self.close(out),
            "error" => { let m = st(&ev["error"]["message"]); self.em.fail(out, if m.is_empty() { "the upstream failed" } else { &m }); }
            _ => {}
        }
    }
    fn close(&mut self, out: &mut Vec<String>) {
        if self.em.done { return; }
        self.em.set_usage(resp_usage_from_ant(&self.native)); self.em.set_reason(short_reason(&self.stop));
        self.em.complete(out);
    }
    pub fn finish(&mut self) -> Vec<String> { let mut out = vec![]; self.close(&mut out); out }
    pub fn error(&mut self, msg: &str) -> Vec<String> { let mut out = vec![]; self.em.fail(&mut out, msg); out }
}
/// A whole Anthropic message as a response object.
pub fn ant_to_response_whole(j: &Value, mut em: RespStream) -> Value {
    let mut sink = vec![];
    if let Some(m) = j["model"].as_str() { em.set_model(m); }
    for blk in j["content"].as_array().cloned().unwrap_or_default() {
        match blk["type"].as_str().unwrap_or("") {
            "text" => em.text(&mut sink, &st(&blk["text"])),
            "tool_use" => { let i = em.call_start(&mut sink, &st(&blk["id"]), &st(&blk["name"])); em.call_args(&mut sink, i, &if blk["input"].is_null() { "{}".to_string() } else { blk["input"].to_string() }); }
            _ => {}
        }
    }
    em.set_usage(resp_usage_from_ant(&j["usage"])); em.set_reason(short_reason(j["stop_reason"].as_str().unwrap_or("")));
    em.complete(&mut sink);
    em.final_response()
}
/// The usage of a native Responses answer (Responses' own shape: `input_tokens` with the cached ones inside) as chat's, the
/// shape the ledger reads it in.
pub fn chat_usage_from_resp(u: &Value) -> Value {
    json!({ "prompt_tokens": u["input_tokens"].as_u64().unwrap_or(0), "completion_tokens": u["output_tokens"].as_u64().unwrap_or(0), "prompt_tokens_details": { "cached_tokens": u["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0) } })
}
/// The model, usage and last sequence number of a native Responses stream, read as it goes by, and the whole response it
/// ended with (`response.completed` / `.incomplete` / `.failed`, its output filled from the `output_item.done` events when
/// the provider left it empty).
#[derive(Default)]
pub struct RespReader { sse: SseData, pub model: String, pub native: Value, pub last_seq: Option<u64>, pub done: Option<Value>, items: Vec<Value> }
impl RespReader {
    fn take(&mut self, ev: &Value) {
        if let Some(n) = ev["sequence_number"].as_u64() { self.last_seq = Some(n); }
        let r = &ev["response"];
        if let Some(m) = r["model"].as_str().filter(|m| !m.is_empty()) { self.model = m.into(); }
        if r["usage"].is_object() { self.native = chat_usage_from_resp(&r["usage"]); }
        match ev["type"].as_str().unwrap_or("") {
            "response.output_item.done" => self.items.push(ev["item"].clone()),
            "response.completed" | "response.incomplete" | "response.failed" => {
                let mut r = r.clone();
                if !r["output"].as_array().map(|a| !a.is_empty()).unwrap_or(false) && r.is_object() { r["output"] = json!(self.items); }
                self.done = Some(r);
            }
            _ => {}
        }
    }
    pub fn push(&mut self, c: &[u8]) { for d in self.sse.push(c) { if let Ok(j) = serde_json::from_str::<Value>(&d) { self.take(&j); } } }
    /// A whole response object.
    pub fn from_json(body: &[u8]) -> RespReader { let mut r = RespReader::default(); let j: Value = serde_json::from_slice(body).unwrap_or(Value::Null); r.take(&json!({ "response": j })); r }
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

    // ---------- the Responses API ----------
    fn codex_request() -> Value {
        json!({ "model": "alias", "instructions": "You are Codex R586", "stream": true, "store": true, "include": ["reasoning.encrypted_content"], "reasoning": { "effort": "high" }, "prompt_cache_key": "k", "parallel_tool_calls": false, "max_output_tokens": 500, "temperature": 0.3, "top_p": 0.8, "metadata": { "a": "b" },
            "tools": [{ "type": "function", "name": "shell", "description": "Runs a command", "parameters": { "type": "object", "properties": { "command": { "type": "array", "items": { "type": "string" } } }, "required": ["command"] }, "strict": false }],
            "tool_choice": "auto",
            "input": [
                { "type": "message", "role": "developer", "content": [{ "type": "input_text", "text": "dev note" }] },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "list files" }, { "type": "input_image", "image_url": "data:image/png;base64,AAAA", "detail": "low" }] },
                { "type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "xyz" },
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Running ls" }] },
                { "type": "function_call", "id": "fc_1", "call_id": "call_a", "name": "shell", "arguments": "{\"command\":[\"ls\"]}" },
                { "type": "function_call", "call_id": "call_b", "name": "shell", "arguments": "{\"command\":[\"pwd\"]}" },
                { "type": "function_call_output", "call_id": "call_a", "output": "a.txt\nb.txt" },
                { "type": "function_call_output", "call_id": "call_b", "output": [{ "type": "input_text", "text": "/work" }, { "type": "input_image", "image_url": "https://x.test/s.png" }] },
                { "role": "user", "content": "thanks" }] })
    }
    #[test]
    fn a_responses_request_becomes_a_chat_one() {
        let o = responses_to_chat(&codex_request(), "gpt-real").unwrap();
        assert_eq!(o["model"], "gpt-real"); assert_eq!(o["max_tokens"], 500); assert_eq!(o["temperature"], 0.3); assert_eq!(o["top_p"], 0.8);
        assert_eq!(o["stream"], true); assert_eq!(o["stream_options"], json!({ "include_usage": true }));
        assert_eq!(o["tools"], json!([{ "type": "function", "function": { "name": "shell", "description": "Runs a command", "parameters": codex_request()["tools"][0]["parameters"], "strict": false } }]));
        assert_eq!(o["tool_choice"], "auto"); assert_eq!(o["parallel_tool_calls"], false);
        let m = o["messages"].as_array().unwrap();
        assert_eq!(m[0], json!({ "role": "system", "content": "You are Codex R586" })); assert_eq!(m[1], json!({ "role": "system", "content": "dev note" }));
        assert_eq!(m[2], json!({ "role": "user", "content": [{ "type": "text", "text": "list files" }, { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA", "detail": "low" } }] }));
        assert_eq!(m[3], json!({ "role": "assistant", "content": "Running ls", "tool_calls": [{ "id": "call_a", "type": "function", "function": { "name": "shell", "arguments": "{\"command\":[\"ls\"]}" } }, { "id": "call_b", "type": "function", "function": { "name": "shell", "arguments": "{\"command\":[\"pwd\"]}" } }] }));
        assert_eq!(m[4], json!({ "role": "tool", "tool_call_id": "call_a", "content": "a.txt\nb.txt" })); assert_eq!(m[5], json!({ "role": "tool", "tool_call_id": "call_b", "content": "/work" }));
        assert_eq!(m[6], json!({ "role": "user", "content": [{ "type": "image_url", "image_url": { "url": "https://x.test/s.png" } }] }), "a tool output's picture follows the tool messages");
        assert_eq!(m[7], json!({ "role": "user", "content": "thanks" })); assert_eq!(m.len(), 8);
        for k in ["store", "include", "reasoning", "prompt_cache_key", "metadata", "instructions", "input", "max_output_tokens"] { assert!(o.get(k).is_none(), "{k}"); }
        // plain text input, structured output
        let d = responses_to_chat(&json!({ "input": "hi", "text": { "format": { "type": "json_schema", "name": "w", "schema": { "type": "object" }, "strict": true } } }), "m").unwrap();
        assert_eq!(d["messages"], json!([{ "role": "user", "content": "hi" }])); assert!(d.get("stream").is_none() && d.get("max_tokens").is_none() && d.get("tools").is_none());
        assert_eq!(d["response_format"], json!({ "type": "json_schema", "json_schema": { "name": "w", "schema": { "type": "object" }, "strict": true } }));
        assert_eq!(responses_to_chat(&json!({ "input": "x", "text": { "format": { "type": "json_object" } } }), "m").unwrap()["response_format"], json!({ "type": "json_object" }));
        let named = responses_to_chat(&json!({ "input": "x", "tools": [{ "type": "function", "name": "f" }], "tool_choice": { "type": "function", "name": "f" } }), "m").unwrap();
        assert_eq!(named["tool_choice"], json!({ "type": "function", "function": { "name": "f" } })); assert_eq!(named["tools"][0]["function"]["parameters"], json!({ "type": "object", "properties": {} }));
    }
    #[test]
    fn a_responses_request_becomes_an_anthropic_one() {
        let o = responses_to_ant(&codex_request(), "claude-real").unwrap();
        assert_eq!(o["model"], "claude-real"); assert_eq!(o["max_tokens"], 500); assert_eq!(o["system"], "You are Codex R586\n\ndev note"); assert_eq!(o["stream"], true);
        assert_eq!(o["tools"][0], json!({ "name": "shell", "description": "Runs a command", "input_schema": codex_request()["tools"][0]["parameters"] }));
        assert_eq!(o["tool_choice"], json!({ "type": "auto", "disable_parallel_tool_use": true }));
        let m = o["messages"].as_array().unwrap();
        assert_eq!(m[0]["content"][1], json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } }));
        assert_eq!(m[1], json!({ "role": "assistant", "content": [{ "type": "text", "text": "Running ls" }, { "type": "tool_use", "id": "call_a", "name": "shell", "input": { "command": ["ls"] } }, { "type": "tool_use", "id": "call_b", "name": "shell", "input": { "command": ["pwd"] } }] }));
        assert_eq!(m[2]["content"][0], json!({ "type": "tool_result", "tool_use_id": "call_a", "content": "a.txt\nb.txt" })); assert_eq!(m[2]["content"][1]["tool_use_id"], "call_b");
        assert_eq!(m[2]["content"][2], json!({ "type": "image", "source": { "type": "url", "url": "https://x.test/s.png" } })); assert_eq!(m[2]["content"][3], json!({ "type": "text", "text": "thanks" }));
        assert_eq!(m.len(), 3);
        assert_eq!(responses_to_ant(&json!({ "input": "hi" }), "m").unwrap()["max_tokens"], RESPONSES_DEFAULT_MAX_TOKENS);
    }
    #[test]
    fn a_responses_request_that_cannot_be_carried_is_refused_by_name() {
        let cases: Vec<(Value, &str)> = vec![
            (json!({ "input": "x", "previous_response_id": "resp_1" }), "keeps no response state"), (json!({ "input": "x", "conversation": "c" }), "conversation"), (json!({ "input": "x", "background": true }), "background"),
            (json!({ "input": [{ "type": "item_reference", "id": "i" }] }), "item_reference"),
            (json!({ "input": "x", "tools": [{ "type": "web_search_preview" }] }), "web_search_preview"), (json!({ "input": "x", "tools": [{ "type": "file_search", "vector_store_ids": [] }] }), "file_search"),
            (json!({ "input": "x", "tools": [{ "type": "computer_use_preview" }] }), "computer_use"), (json!({ "input": "x", "tools": [{ "type": "code_interpreter" }] }), "code_interpreter"),
            (json!({ "input": "x", "tools": [{ "type": "mcp", "server_label": "s" }] }), "\"mcp\""), (json!({ "input": "x", "tools": [{ "type": "image_generation" }] }), "image_generation"),
            (json!({ "input": "x", "tools": [{ "type": "custom", "name": "apply_patch" }] }), "apply_patch"), (json!({ "input": "x", "tools": [{ "type": "function" }] }), "needs a \"name\""),
            (json!({ "input": [{ "type": "local_shell_call", "call_id": "c" }] }), "local_shell_call"), (json!({ "input": [{ "role": "user", "content": [{ "type": "input_file", "file_id": "f" }] }] }), "input_file"),
            (json!({ "input": [{ "role": "user", "content": [{ "type": "input_image", "file_id": "f" }] }] }), "file_id"), (json!({ "input": "x", "prompt": { "id": "p" } }), "prompt"),
            (json!({ "input": "x", "text": { "format": { "type": "grammar" } } }), "grammar"), (json!({ "input": [] }), "at least one message"), (json!({ "instructions": "only" }), "at least one message"),
            (json!({ "input": "x", "tools": [{ "type": "function", "name": "f" }], "tool_choice": { "type": "allowed_tools" } }), "allowed_tools"),
        ];
        for (b, want) in cases { let e = responses_to_chat(&b, "m").unwrap_err(); assert!(e.contains(want), "{b} -> {e}"); }
        assert!(responses_to_ant(&json!({ "input": "x", "text": { "format": { "type": "json_schema", "schema": {} } } }), "m").unwrap_err().contains("json_schema"));
        assert!(responses_stateless(&json!({ "previous_response_id": null, "input": "x" })).is_ok());
    }

    fn resp_frames(frames: &[String]) -> Vec<(String, Value)> {
        frames.iter().map(|f| { let mut l = f.lines(); let ev = l.next().unwrap().trim_start_matches("event: ").to_string(); (ev, serde_json::from_str(l.next().unwrap().trim_start_matches("data: ")).unwrap()) }).collect()
    }
    /// The shape every Responses stream must have: event names = types, sequence numbers 0, 1, 2 ..., created and in_progress
    /// first, one terminal event last, items opened before they are told and closed once.
    fn assert_resp_shape(fr: &[(String, Value)], last: &str) {
        for (i, (ev, d)) in fr.iter().enumerate() { assert_eq!(d["type"], json!(ev), "{ev}"); assert_eq!(d["sequence_number"], json!(i), "{ev} #{i}"); }
        assert_eq!(fr[0].0, "response.created"); assert_eq!(fr[1].0, "response.in_progress"); assert_eq!(fr[0].1["response"]["status"], "in_progress"); assert_eq!(fr[0].1["response"]["output"], json!([]));
        assert_eq!(fr.last().unwrap().0, last); assert_eq!(fr.iter().filter(|f| f.0 == last).count(), 1);
        assert_eq!(fr.iter().filter(|f| f.0 == "response.output_item.added").count(), fr.iter().filter(|f| f.0 == "response.output_item.done").count());
    }
    fn tool_stream_check(fr: &[(String, Value)], call_id: &str) {
        let kinds: Vec<&str> = fr.iter().map(|f| f.0.as_str()).collect();
        let want = ["response.created", "response.in_progress", "response.output_item.added", "response.content_part.added", "response.output_text.delta", "response.output_text.delta", "response.output_text.done", "response.content_part.done", "response.output_item.done",
            "response.output_item.added", "response.function_call_arguments.delta", "response.function_call_arguments.delta", "response.function_call_arguments.done", "response.output_item.done"];
        assert_eq!(kinds[..want.len()], want, "{kinds:?}");
        let text: String = fr.iter().filter(|f| f.0 == "response.output_text.delta").map(|f| f.1["delta"].as_str().unwrap()).collect();
        let msg_done = &fr[8].1["item"]; assert_eq!(msg_done["content"][0]["text"], json!(text)); assert_eq!(msg_done["status"], "completed"); assert_eq!(msg_done["role"], "assistant"); assert_eq!(fr[8].1["output_index"], 0);
        assert_eq!(fr[9].1["item"]["type"], "function_call"); assert_eq!(fr[9].1["item"]["call_id"], call_id); assert_eq!(fr[9].1["item"]["arguments"], ""); assert_eq!(fr[9].1["output_index"], 1);
        let args: String = fr.iter().filter(|f| f.0 == "response.function_call_arguments.delta" && f.1["output_index"] == 1).map(|f| f.1["delta"].as_str().unwrap()).collect();
        assert_eq!(serde_json::from_str::<Value>(&args).unwrap(), json!({ "city": "Zürich" }));
        assert_eq!(fr[12].1["arguments"], json!(args)); assert_eq!(fr[12].1["item_id"], fr[9].1["item"]["id"]);
        let done = &fr.last().unwrap().1["response"];
        assert_eq!(done["status"], "completed"); assert_eq!(done["object"], "response"); assert!(done["id"].as_str().unwrap().starts_with("resp_"));
        assert_eq!(done["output"].as_array().unwrap().iter().filter(|i| i["type"] == "function_call").count(), 2);
        assert_eq!(done["output"][1]["arguments"], json!(args)); assert_eq!(done["output"][1]["status"], "completed");
    }
    #[test]
    fn an_anthropic_stream_becomes_responses_events_whatever_the_split() {
        let sse = ant_stream(); let mut reference: Option<Vec<String>> = None;
        for step in [1usize, 3, 7, 64, 100_000] {
            let mut c = AntToResp::new(RespStream::new("resp_t", 5, "req-model", response_base(&json!({ "tools": [], "instructions": "i" }))));
            let mut frames = vec![]; for p in sse.as_bytes().chunks(step) { frames.extend(c.push(p)); } frames.extend(c.finish());
            if let Some(r) = &reference { assert_eq!(&frames, r, "step {step}"); } else { reference = Some(frames.clone()); }
            let fr = resp_frames(&frames);
            assert_resp_shape(&fr, "response.completed"); tool_stream_check(&fr, "tu1");
            let done = &fr.last().unwrap().1["response"];
            assert_eq!(done["model"], "claude-x"); assert_eq!(done["instructions"], "i"); assert_eq!(done["created_at"], 5);
            assert_eq!(done["usage"], json!({ "input_tokens": 16, "input_tokens_details": { "cached_tokens": 5 }, "output_tokens": 17, "output_tokens_details": { "reasoning_tokens": 0 }, "total_tokens": 33 }));
            assert_eq!(done["output"][0]["content"][0]["text"], "Hello é"); assert!(!frames.concat().contains("thinking"));
            assert_eq!((c.model(), c.native()["output_tokens"].as_u64()), ("claude-x", Some(17)));
        }
        // max_tokens ends as response.incomplete; an error event as response.failed; a stream cut short still ends
        let cut = [ev("message_start", json!({ "message": { "model": "q", "usage": { "input_tokens": 2 } } })), ev("content_block_start", json!({ "index": 0, "content_block": { "type": "text", "text": "" } })), ev("content_block_delta", json!({ "index": 0, "delta": { "type": "text_delta", "text": "Hel" } })),
            ev("message_delta", json!({ "delta": { "stop_reason": "max_tokens" }, "usage": { "output_tokens": 3 } })), ev("message_stop", json!({}))].concat();
        let mut c = AntToResp::new(RespStream::new("resp_i", 1, "m", response_base(&json!({})))); let mut f = c.push(cut.as_bytes()); f.extend(c.finish());
        let fr = resp_frames(&f); assert_resp_shape(&fr, "response.incomplete");
        let r = &fr.last().unwrap().1["response"]; assert_eq!(r["status"], "incomplete"); assert_eq!(r["incomplete_details"], json!({ "reason": "max_output_tokens" })); assert_eq!(r["output"][0]["content"][0]["text"], "Hel");
        let mut c = AntToResp::new(RespStream::new("resp_e", 1, "m", response_base(&json!({})))); let mut f = c.push(ev("error", json!({ "error": { "type": "overloaded_error", "message": "busy" } })).as_bytes()); f.extend(c.finish());
        let fr = resp_frames(&f); assert_resp_shape(&fr, "response.failed"); assert_eq!(fr.last().unwrap().1["response"]["error"], json!({ "code": "server_error", "message": "busy" }));
        let mut c = AntToResp::new(RespStream::new("resp_c", 1, "m", response_base(&json!({})))); let mut f = c.push(cut.as_bytes().split_at(cut.len() / 2).0); f.extend(c.error("the upstream stream broke"));
        let fr = resp_frames(&f); assert_resp_shape(&fr, "response.failed"); assert!(c.finish().is_empty());
    }
    #[test]
    fn an_openai_chat_stream_becomes_responses_events_whatever_the_split() {
        let sse = oa_stream(); let mut reference: Option<Vec<String>> = None;
        for step in [1usize, 5, 11, 200, 100_000] {
            let mut c = ChatToResp::new(RespStream::new("resp_o", 2, "req", response_base(&json!({}))));
            let mut frames = vec![]; for p in sse.as_bytes().chunks(step) { frames.extend(c.push(p)); } frames.extend(c.finish());
            if let Some(r) = &reference { assert_eq!(&frames, r, "step {step}"); } else { reference = Some(frames.clone()); }
            let fr = resp_frames(&frames);
            assert_resp_shape(&fr, "response.completed"); tool_stream_check(&fr, "call_1");
            let done = &fr.last().unwrap().1["response"];
            assert_eq!(done["model"], "gpt-x"); assert_eq!(done["output"][0]["content"][0]["text"], "Hi there é");
            assert_eq!(done["usage"], json!({ "input_tokens": 20, "input_tokens_details": { "cached_tokens": 8 }, "output_tokens": 6, "output_tokens_details": { "reasoning_tokens": 0 }, "total_tokens": 26 }));
            assert_eq!(c.native()["prompt_tokens"], 20);
        }
        let mut c = ChatToResp::new(RespStream::new("resp_l", 1, "m", response_base(&json!({}))));
        let mut f = c.push(format!("{}{}", oa_chunk(json!({ "content": "a" }), Value::Null), oa_chunk(json!({}), json!("length"))).as_bytes()); f.extend(c.finish());
        assert_resp_shape(&resp_frames(&f), "response.incomplete");
        let mut c = ChatToResp::new(RespStream::new("resp_x", 1, "m", response_base(&json!({})))); let f = c.push(b"data: {\"error\":{\"message\":\"boom\"}}\n\n");
        let fr = resp_frames(&f); assert_resp_shape(&fr, "response.failed"); assert!(c.finish().is_empty());
    }
    #[test]
    fn whole_answers_become_response_objects() {
        let base = response_base(&json!({ "tools": [{ "type": "function", "name": "get" }], "tool_choice": "auto", "max_output_tokens": 9 }));
        let ant = json!({ "model": "claude-x", "stop_reason": "tool_use", "content": [{ "type": "text", "text": "t" }, { "type": "thinking", "thinking": "x" }, { "type": "tool_use", "id": "tu", "name": "get", "input": { "q": 1 } }], "usage": { "input_tokens": 3, "cache_read_input_tokens": 2, "output_tokens": 4 } });
        let r = ant_to_response_whole(&ant, RespStream::new("resp_a", 7, "fb", base.clone()));
        assert_eq!((r["object"].as_str(), r["status"].as_str(), r["model"].as_str(), r["created_at"].as_u64()), (Some("response"), Some("completed"), Some("claude-x"), Some(7)));
        assert_eq!(r["output"][0]["type"], "message"); assert_eq!(r["output"][0]["content"], json!([{ "type": "output_text", "text": "t", "annotations": [] }]));
        assert_eq!(r["output"][1]["type"], "function_call"); assert_eq!(r["output"][1]["call_id"], "tu"); assert_eq!(r["output"][1]["arguments"], "{\"q\":1}"); assert_eq!(r["output"].as_array().unwrap().len(), 2);
        assert_eq!(r["usage"]["input_tokens"], 5); assert_eq!(r["usage"]["input_tokens_details"]["cached_tokens"], 2); assert_eq!(r["usage"]["total_tokens"], 9);
        assert_eq!(r["max_output_tokens"], 9); assert_eq!(r["tools"][0]["name"], "get"); assert_eq!(r["store"], false); assert_eq!(r["incomplete_details"], Value::Null);
        let oa = json!({ "model": "gpt-x", "choices": [{ "message": { "role": "assistant", "content": "partial" }, "finish_reason": "length" }], "usage": { "prompt_tokens": 9, "completion_tokens": 2 } });
        let r = chat_to_response_whole(&oa, RespStream::new("resp_b", 1, "fb", base));
        assert_eq!(r["status"], "incomplete"); assert_eq!(r["incomplete_details"]["reason"], "max_output_tokens"); assert_eq!(r["output"][0]["content"][0]["text"], "partial"); assert_eq!(r["usage"]["output_tokens"], 2);
    }
    #[test]
    fn a_native_responses_stream_is_read_for_usage_and_its_end() {
        let e = |t: &str, n: u64, o: Value| { let mut v = o; v["type"] = json!(t); v["sequence_number"] = json!(n); format!("event: {t}\ndata: {v}\n\n") };
        let s = [e("response.created", 0, json!({ "response": { "model": "gpt-n", "output": [] } })), e("response.output_item.done", 1, json!({ "item": { "type": "message", "id": "m" } })),
            e("response.completed", 2, json!({ "response": { "model": "gpt-n", "output": [], "usage": { "input_tokens": 100, "input_tokens_details": { "cached_tokens": 40 }, "output_tokens": 7 } } }))].concat();
        for step in [1usize, 9, 10_000] {
            let mut r = RespReader::default(); for c in s.as_bytes().chunks(step) { r.push(c); }
            assert_eq!(r.model, "gpt-n"); assert_eq!(r.last_seq, Some(2)); assert_eq!(r.done.as_ref().unwrap()["output"][0]["id"], "m");
            let u = crate::openai::normalise(&r.native).unwrap(); assert_eq!((u.input, u.cache_read, u.output), (60, 40, 7));
        }
        let w = RespReader::from_json(br#"{"object":"response","model":"gpt-w","usage":{"input_tokens":5,"output_tokens":2}}"#); assert_eq!(w.model, "gpt-w"); assert_eq!(w.native["prompt_tokens"], 5);
    }
}
