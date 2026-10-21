//! Unity's runs (from bridge/assistant.js): one answer with the local brain (Ollama, native tool calls, up to
//! MAX_ROUNDS rounds) or a Claude brain (`claude -p` with her tools through bridge/assistant-tools-mcp.js), the tools
//! that live in her store (tasks, calendar, reminders, memory, knowledge), and her answers for the team chat.
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use super::auth::Account;
use super::unity::{s, truthy, Ci, Unity, HISTORY_KEEP, MAX_RESULT, MAX_ROUNDS};
use super::unity_time as ut;
use crate::front::Hub;
use vr_core::now_ms;

fn now() -> i64 { now_ms() as i64 }
fn next_id(st: &mut Value, p: &str) -> String { let n = st["next"][p].as_i64().unwrap_or(0) + 1; st["next"][p] = json!(n); format!("{p}{n:03}") }
fn arr(st: &Value, k: &str) -> Vec<Value> { st[k].as_array().cloned().unwrap_or_default() }
fn show(t: &Value, tz: &str) -> Value {
    let mut v = t.clone(); let o = v.as_object_mut().unwrap(); o.remove("created");
    match t["due"].as_i64() { Some(d) => { o.insert("due".into(), json!(ut::fmt(d, tz))); if t["status"] == json!("pending") && d < now() { o.insert("overdue".into(), json!(true)); } } None => { o.remove("due"); } }
    v
}
fn find_idx(list: &[Value], id: &Value) -> Option<usize> { let id = match id { Value::String(x) => x.to_lowercase(), o => o.to_string().to_lowercase() }; list.iter().position(|t| s(t, "id").to_lowercase() == id) }
fn num_or(v: &Value, d: i64) -> i64 { v.as_f64().or_else(|| v.as_str().and_then(|x| x.trim().parse().ok())).filter(|x| *x != 0.0 && x.is_finite()).map(|x| x as i64).unwrap_or(d) }

/// The tools that live in her store.
pub fn store_tool(st: &mut Value, name: &str, a: &Value, tz: &str) -> Value {
    let no = |e: String| json!({ "ok": false, "error": e });
    match name {
        "add_task" => {
            let mut t = json!({ "id": next_id(st, "T"), "title": s(a, "title").chars().take(300).collect::<String>(), "status": "pending", "priority": num_or(&a["priority"], 5), "created": now(), "notes": [] });
            if truthy(&a["due"]) { if let Some(d) = ut::parse_when(&s(a, "due"), tz) { t["due"] = json!(d); } }
            st["tasks"].as_array_mut().unwrap().push(t.clone()); json!({ "ok": true, "task": show(&t, tz) })
        }
        "list_tasks" => {
            let f = if s(a, "filter").is_empty() { "pending".to_string() } else { s(a, "filter") };
            let mut l: Vec<Value> = arr(st, "tasks").into_iter().filter(|t| f == "all" || if f == "overdue" { t["status"] == json!("pending") && t["due"].as_i64().map(|d| d < now()).unwrap_or(false) } else { t["status"] == json!(f) }).collect();
            l.sort_by_key(|t| t["priority"].as_i64().unwrap_or(5));
            json!({ "ok": true, "count": l.len(), "tasks": l.iter().map(|t| show(t, tz)).collect::<Vec<_>>() })
        }
        "complete_task" | "delete_task" | "set_task_priority" | "set_task_due" | "add_task_note" => {
            let list = arr(st, "tasks"); let Some(i) = find_idx(&list, &a["task_id"]) else { return no(format!("No task {}", s(a, "task_id"))) };
            let t = &mut st["tasks"][i];
            match name {
                "complete_task" => { t["status"] = json!("done"); t["done"] = json!(now()); json!({ "ok": true, "task": t["title"] }) }
                "delete_task" => { let title = t["title"].clone(); st["tasks"].as_array_mut().unwrap().remove(i); json!({ "ok": true, "deleted": title }) }
                "set_task_priority" => { t["priority"] = json!(num_or(&a["priority"], 5).clamp(1, 10)); json!({ "ok": true }) }
                "set_task_due" => match ut::parse_when(&s(a, "due"), tz) { Some(d) => { t["due"] = json!(d); json!({ "ok": true, "due": ut::fmt(d, tz) }) } None => no("Could not read that date.".into()) },
                _ => { if !t["notes"].is_array() { t["notes"] = json!([]); } t["notes"].as_array_mut().unwrap().push(json!(s(a, "note").chars().take(1000).collect::<String>())); json!({ "ok": true }) }
            }
        }
        "search_tasks" => { let q = s(a, "query").to_lowercase(); json!({ "ok": true, "tasks": arr(st, "tasks").iter().filter(|t| format!("{} {}", s(t, "title"), t["notes"].as_array().map(|n| n.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ")).unwrap_or_default()).to_lowercase().contains(&q)).map(|t| show(t, tz)).collect::<Vec<_>>() }) }
        "add_event" => {
            let Some(at) = ut::parse_when(&s(a, "when"), tz) else { return no("Could not read that date.".into()) };
            let mut e = json!({ "id": next_id(st, "E"), "title": s(a, "title").chars().take(300).collect::<String>(), "at": at, "minutes": num_or(&a["duration_minutes"], 60) });
            if truthy(&a["location"]) { e["location"] = json!(s(a, "location").chars().take(200).collect::<String>()); }
            st["events"].as_array_mut().unwrap().push(e.clone()); let mut shown = e; shown["at"] = json!(ut::fmt(at, tz)); json!({ "ok": true, "event": shown })
        }
        "today_events" | "upcoming_events" => {
            let k = ut::day_key(now(), tz); let end = now() + num_or(&a["days"], 7) * 86_400_000;
            let mut l: Vec<Value> = arr(st, "events").into_iter().filter(|e| { let at = e["at"].as_i64().unwrap_or(0); if name == "today_events" { ut::day_key(at, tz) == k } else { at >= now() - 3_600_000 && at <= end } }).collect();
            l.sort_by_key(|e| e["at"].as_i64().unwrap_or(0));
            json!({ "ok": true, "events": l.into_iter().map(|mut e| { let at = e["at"].as_i64().unwrap_or(0); e["at"] = json!(ut::fmt(at, tz)); e }).collect::<Vec<_>>() })
        }
        "delete_event" | "delete_reminder" | "forget_knowledge" => {
            let (key, idk, what, field) = match name { "delete_event" => ("events", "event_id", "event", "title"), "delete_reminder" => ("reminders", "reminder_id", "reminder", "text"), _ => ("knowledge", "entry_id", "entry", "") };
            let list = arr(st, key); let Some(i) = find_idx(&list, &a[idk]) else { return no(format!("No {what} {}", s(a, idk))) };
            let gone = st[key].as_array_mut().unwrap().remove(i);
            if field.is_empty() { json!({ "ok": true }) } else { json!({ "ok": true, "deleted": gone[field] }) }
        }
        "set_reminder" => {
            let Some(at) = ut::parse_when(&s(a, "when"), tz) else { return no("Could not read that time.".into()) };
            let r = json!({ "id": next_id(st, "R"), "text": s(a, "text").chars().take(300).collect::<String>(), "at": at, "fired": false });
            st["reminders"].as_array_mut().unwrap().push(r.clone()); let mut shown = r; shown["at"] = json!(ut::fmt(at, tz)); json!({ "ok": true, "reminder": shown })
        }
        "list_reminders" => { let mut l: Vec<Value> = arr(st, "reminders").into_iter().filter(|r| r["fired"] != json!(true)).collect(); l.sort_by_key(|r| r["at"].as_i64().unwrap_or(0)); json!({ "ok": true, "reminders": l.into_iter().map(|mut r| { let at = r["at"].as_i64().unwrap_or(0); r["at"] = json!(ut::fmt(at, tz)); r }).collect::<Vec<_>>() }) }
        "remember" => { st["memory"][s(a, "key").chars().take(80).collect::<String>()] = json!(s(a, "value").chars().take(500).collect::<String>()); json!({ "ok": true }) }
        "recall_memory" => if truthy(&a["key"]) { json!({ "ok": true, "key": a["key"], "value": st["memory"].get(s(a, "key")).cloned().unwrap_or(Value::Null) }) } else { json!({ "ok": true, "memory": st["memory"] }) },
        "forget_memory" => { let had = st["memory"].as_object_mut().map(|m| m.remove(&s(a, "key")).is_some()).unwrap_or(false); json!({ "ok": had }) }
        "learn" => { let k = json!({ "id": next_id(st, "K"), "fact": s(a, "fact").chars().take(2000).collect::<String>(), "tags": s(a, "tags").split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect::<Vec<_>>(), "at": now() }); let id = k["id"].clone(); st["knowledge"].as_array_mut().unwrap().push(k); json!({ "ok": true, "id": id }) }
        "recall_knowledge" => {
            let q = s(a, "query").to_lowercase(); let words: Vec<String> = regex::Regex::new(r"[a-z0-9]+").unwrap().find_iter(&q).map(|m| m.as_str().to_string()).collect();
            let mut scored: Vec<(usize, Value)> = arr(st, "knowledge").into_iter().map(|k| { let hay = format!("{} {}", s(&k, "fact"), k["tags"].as_array().map(|t| t.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ")).unwrap_or_default()).to_lowercase(); (words.iter().filter(|w| hay.contains(w.as_str())).count(), k) }).filter(|x| x.0 > 0).collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            json!({ "ok": true, "results": scored.into_iter().take(10).map(|(_, k)| json!({ "id": k["id"], "fact": k["fact"], "tags": k["tags"] })).collect::<Vec<_>>() })
        }
        _ => no(format!("No tool called {name}.")),
    }
}

/// What she knows about who she talks with this turn.
pub fn dynamic_context(u: &Unity, a: &Account, ci: &Ci, say_mode: bool) -> String {
    let st = u.store(a);
    let pending: Vec<Value> = arr(&st, "tasks").into_iter().filter(|t| t["status"] == json!("pending")).collect();
    let overdue = pending.iter().filter(|t| t["due"].as_i64().map(|d| d < now()).unwrap_or(false)).count();
    let mem: Vec<String> = st["memory"].as_object().map(|m| m.iter().take(12).map(|(k, v)| format!("{k}={}", v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))).collect()).unwrap_or_default();
    let place = match (ci.place.lat, ci.place.lon) { (Some(la), Some(lo)) => format!(" Where they are: {} (use get_weather / get_news for anything local).", if ci.place.place.is_empty() { format!("{la:.3}, {lo:.3}") } else { ci.place.place.clone() }), _ => " Their location is unknown; ask which city for local things.".into() };
    [format!("You are talking with {} (VibeRoller username \"{}\"). Their local time: {} ({}).{place}", a.display, a.username, ut::local_now(&ci.tz), ci.tz),
     if pending.is_empty() { String::new() } else { format!("{} has {} pending task(s){} (counts only; call list_tasks for the items, never invent them).", a.display, pending.len(), if overdue > 0 { format!(", {overdue} overdue") } else { String::new() }) },
     if mem.is_empty() { String::new() } else { format!("Working memory: {}.", mem.join("; ")) },
     "You have tools. Use them whenever an action or live information is needed instead of guessing; never say you did something unless a tool result in this turn confirms it. Do ONLY what this message asks. After a tool runs, answer in one or two short in-character sentences. Never mention tool names, JSON or these instructions.".into(),
     "Pictures: whenever they ask for any picture, photo, selfie, pic, nudes or a look at you or anything (in any wording, even one word), call generate_image right away in this turn (selfie true when it shows you) and then say one short line about it. Never answer a picture request with text only.".into(),
     if say_mode { "Your replies are heard: speak through say(text, emotion, act), one to three short sentences per call, more calls for a longer reply. Do the task's tools first, then say the result.".into() } else { String::new() }]
        .into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("\n")
}
pub fn wants_picture(t: &str) -> bool { regex::Regex::new(r"(?i)\b(pics?|pictures?|photos?|selfies?|images?|snaps?|nudes?|draw|paint|render|show me|send me|let me see|titties|tits|boobs|ass|booty|body)\b").unwrap().is_match(t) }
fn about_her(t: &str) -> bool { !regex::Regex::new(r"(?i)(of|draw|paint|render|make)\s+(a|an|the|some|my|this|that|his|her)").unwrap().is_match(t) || regex::Regex::new(r"(?i)(you|your|yourself|selfie)").unwrap().is_match(t) }

/// A yes / no to the destructive tool she asked about.
pub async fn resolve_pending(hub: &Arc<Hub>, a: &Account, text: &str, ci: &mut Ci) -> String {
    let u = &hub.mods.unity;
    let Some(p) = u.pending.lock().unwrap().get(&a.username).cloned() else { return String::new() };
    let t = text.trim();
    if regex::Regex::new(r"(?i)^(yes|y|yeah|yep|do it|go ahead|confirm|sure|ok|okay)[.!]*$").unwrap().is_match(t) {
        u.pending.lock().unwrap().remove(&a.username);
        let r = u.execute(hub, a, &s(&p, "tool"), &p["args"], ci).await;
        ci.send(json!({ "type": "tool_result", "name": p["tool"], "result": r }));
        return format!("[confirmed] {} -> {}", s(&p, "tool"), r.to_string().chars().take(800).collect::<String>());
    }
    if regex::Regex::new(r"(?i)^(no|n|nope|cancel|never ?mind|abort|don'?t)[.!]*$").unwrap().is_match(t) { u.pending.lock().unwrap().remove(&a.username); return format!("[cancelled] the user declined {}", s(&p, "tool")); }
    String::new()
}
/// One tool call from either brain; a destructive one waits for the person's yes / no.
pub async fn call_tool(hub: &Arc<Hub>, a: &Account, name: &str, args: &Value, ci: &mut Ci) -> Value {
    let u = &hub.mods.unity;
    ci.send(json!({ "type": "tool", "name": name, "args": args }));
    let confirm = Unity::tools().iter().any(|t| t["name"] == json!(name) && t["confirm"] == json!(true));
    let result = if confirm {
        u.pending.lock().unwrap().insert(a.username.clone(), json!({ "tool": name, "args": args }));
        ci.send(json!({ "type": "confirm", "tool": name, "args": args }));
        json!({ "ok": false, "pending": true, "message": format!("{name} needs {}'s confirmation; ask them to say yes or no.", a.display) })
    } else { u.execute(hub, a, name, args, ci).await };
    if name == "generate_image" && result["ok"] == json!(true) { ci.send(json!({ "type": "image", "ref": result["image"] })); }
    if name == "say" && truthy(&args["text"]) { ci.send(json!({ "type": "say", "text": s(args, "text"), "emotion": if truthy(&args["emotion"]) { args["emotion"].clone() } else { json!("neutral") }, "act": if truthy(&args["act"]) { args["act"].clone() } else { json!("statement") } })); }
    ci.send(json!({ "type": "tool_result", "name": name, "result": result }));
    result
}
#[derive(Default)]
pub struct Opts { pub extra_system: String, pub no_history: bool, pub photo: String, pub no_pictures: bool }
/// One answer with the local brain.
pub async fn run_local(hub: &Arc<Hub>, a: &Account, text: &str, ci: &mut Ci, cancel: &super::openai::Cancel, opts: Opts) -> Result<String, String> {
    let u = &hub.mods.unity;
    let st = u.ensure_ollama(hub).await;
    let model = if st.model { u.local_model.clone() } else { u.fallback.clone() };
    if !st.up || model.is_empty() {
        return Err(if st.up { format!("The local brain ({}) is not loaded on the AI host yet.", u.local_model) } else { let r = if u.via_node { hub.mods.ai.reason() } else { String::new() }; if r.is_empty() { "Unity's brain (Ollama) is not running on the AI host.".into() } else { r } });
    }
    let native = u.native_tools(hub, &model).await;
    let say_mode = model.starts_with("unity-brain");
    let mut system = format!("{}\n\n{}", u.persona(), dynamic_context(u, a, ci, say_mode));
    let tools: Vec<Value> = u.tools_for(a, ci.out.is_some()).into_iter().filter(|t| say_mode || t["name"] != json!("say")).collect();
    if !native { system += &format!("\n\nTOOL USE: when you need a tool, reply with ONLY one line of JSON: {{\"tool\": \"<name>\", \"arguments\": {{...}}}}. You get the result next. Tools:\n{}", tools.iter().map(|t| format!("- {}({}): {}", s(t, "name"), t["params"].as_object().map(|p| p.keys().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default(), s(t, "description"))).collect::<Vec<_>>().join("\n")); }
    let note = resolve_pending(hub, a, text, ci).await;
    if !opts.extra_system.is_empty() { system += &format!("\n\n{}", opts.extra_system); }
    let mut user = json!({ "role": "user", "content": if note.is_empty() { text.to_string() } else { format!("{text}\n\n(system note: {note})") } });
    if !opts.photo.is_empty() { if let Some(b) = u.read_picture(&a.username, &opts.photo) { user["images"] = json!([b]); user["content"] = json!(format!("{}\n\n(They sent you a photo, id {}: you can see it. To change it use edit_image / inpaint_image with image \"camera\".)", s(&user, "content"), opts.photo)); } }
    let mut messages = vec![json!({ "role": "system", "content": system })];
    if !opts.no_history { let h = arr(&u.store(a), "history"); messages.extend(h.iter().skip(h.len().saturating_sub(HISTORY_KEEP)).cloned()); }
    messages.push(user);
    let schemas: Vec<Value> = tools.iter().map(|t| json!({ "type": "function", "function": { "name": t["name"], "description": t["description"], "parameters": { "type": "object", "properties": t["params"], "required": t["required"] } } })).collect();
    let (mut said, mut seen, mut fin) = (vec![], std::collections::HashSet::new(), String::new());
    let num_ctx = u.a["numCtx"].as_u64().unwrap_or(16384);
    for round in 0..MAX_ROUNDS {
        if cancel.is() { return Err("cancelled".into()); }
        let mut body = json!({ "model": model, "messages": messages, "stream": true, "think": false, "keep_alive": "30m", "options": { "num_ctx": num_ctx } });
        if native { body["tools"] = json!(schemas); }
        let (status, mut src) = u.engine(hub, &format!("{}/api/chat", u.ollama), "POST", body.to_string().into_bytes(), 600_000).await?;
        if !(200..300).contains(&status) { return Err(format!("Local brain error: {}", String::from_utf8_lossy(&src.all().await).chars().take(200).collect::<String>())); }
        let (mut content, mut calls, mut buf): (String, Vec<(String, Value)>, String) = (String::new(), vec![], String::new());
        loop {
            let chunk = tokio::select! { c = src.next() => c, _ = cancel.woke() => return Err("cancelled".into()) };
            let Some(c) = chunk else { break };
            buf.push_str(&String::from_utf8_lossy(&c));
            while let Some(i) = buf.find('\n') {
                let line = buf[..i].trim().to_string(); buf.drain(..=i);
                if line.is_empty() { continue; }
                let ev: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
                if !ev["error"].is_null() { return Err(s(&ev, "error")); }
                let m = &ev["message"];
                if let Some(t) = m["content"].as_str().filter(|t| !t.is_empty()) { content.push_str(t); if calls.is_empty() { ci.send(json!({ "type": "token", "text": t })); } }
                for c in m["tool_calls"].as_array().cloned().unwrap_or_default() { calls.push((s(&c["function"], "name"), if c["function"]["arguments"].is_null() { json!({}) } else { c["function"]["arguments"].clone() })); }
            }
        }
        content = regex::Regex::new(r"(?s)<think>.*?</think>").unwrap().replace_all(&content, "").trim().to_string();
        if !native { if let Some(c) = regex::Regex::new(r#"(?s)\{\s*"tool"\s*:\s*"([^"]+)"\s*,\s*"arguments"\s*:\s*(\{.*?\})\s*\}"#).unwrap().captures(&content) { if let Ok(args) = serde_json::from_str::<Value>(&c[2]) { calls = vec![(c[1].to_string(), args)]; content = String::new(); } } }
        if calls.is_empty() { fin = content; break; }
        messages.push(json!({ "role": "assistant", "content": content, "tool_calls": calls.iter().map(|(n, a)| json!({ "function": { "name": n, "arguments": a } })).collect::<Vec<_>>() }));
        let mut deferred = false;
        for (n, args) in calls {
            let sig = format!("{n}{args}");
            let result = if seen.contains(&sig) { json!({ "ok": false, "error": format!("{n} already ran with these arguments this turn; use its result.") }) } else { call_tool(hub, a, &n, &args, ci).await };
            seen.insert(sig);
            if n == "say" && truthy(&args["text"]) { said.push(s(&args, "text")); }
            if result["pending"] == json!(true) { deferred = true; }
            let mut payload = result.to_string(); if payload.chars().count() > MAX_RESULT { payload = payload.chars().take(MAX_RESULT).collect::<String>() + "...\"}"; }
            messages.push(json!({ "role": "tool", "content": payload, "tool_name": n }));
        }
        if deferred && round == MAX_ROUNDS - 1 { break; }
    }
    // backstop: they clearly asked for a picture and none was made this turn: made from their words
    if !opts.no_pictures && wants_picture(text) && ci.images.is_empty() {
        let prompt = regex::Regex::new(r"^\s*@\w+\s*").unwrap().replace(text, "").to_string();
        let r = u.make_image(hub, ci, &json!({ "prompt": prompt, "selfie": about_her(text), "shape": "portrait" })).await;
        ci.send(json!({ "type": "tool", "name": "generate_image", "args": { "prompt": text, "auto": true } }));
        ci.send(json!({ "type": "tool_result", "name": "generate_image", "result": r }));
        if r["ok"] == json!(true) { ci.send(json!({ "type": "image", "ref": r["image"] })); }
    }
    let spoken = said.join(" ").trim().to_string();
    let shown = if fin.is_empty() { spoken.clone() } else if said.iter().all(|l| fin.contains(l.as_str())) { fin.clone() } else { format!("{spoken} {fin}").trim().to_string() };
    if said.is_empty() && say_mode && !fin.is_empty() { ci.send(json!({ "type": "say", "text": fin, "emotion": "neutral", "act": if fin.ends_with('?') { "yes_no_question" } else { "statement" } })); }
    Ok(shown)
}
/// One answer with a Claude brain: `claude -p` with her tools (MCP relay → /internal/assistant-tool).
pub async fn run_claude(hub: &Arc<Hub>, a: &Account, text: &str, ci: &mut Ci, model: &str) -> Result<String, String> {
    let u = &hub.mods.unity;
    let note = resolve_pending(hub, a, text, ci).await;
    if let Some(o) = &ci.out { u.claude_out.lock().unwrap().insert(a.username.clone(), o.clone()); }
    let mcp = a.dir.join(".mcp-assistant.json");
    let node = std::env::var("VR_NODE").unwrap_or_else(|_| "node".into());
    super::unity::write_json(&mcp, &json!({ "mcpServers": { "assistant": { "command": node, "args": [std::path::absolute(hub.root.join("assistant-tools-mcp.js")).unwrap_or_default().display().to_string()], "env": { "MILLS_BRIDGE": format!("http://127.0.0.1:{}", hub.cfg.port), "MILLS_INTERNAL": vr_core::crypto::hmac_hex(u.secret.as_bytes(), a.username.as_bytes()), "MILLS_ACCOUNT": a.username } } } }));
    vr_core::settings::private(&mcp);
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose", "--permission-mode", "default", "--mcp-config"].iter().map(|x| x.to_string()).collect();
    args.extend([mcp.display().to_string(), "--strict-mcp-config".into(), "--allowedTools".into(), "mcp__assistant".into(), "--append-system-prompt".into(), format!("{}\n\n{}", u.persona(), dynamic_context(u, a, ci, false)), "--model".into(), model.into()]);
    let sess = s(&u.state(a), "claudeSession"); if !sess.is_empty() { args.push("--resume".into()); args.push(sess); }
    let pre: Vec<String> = hub.cfg.get("claudeArgs").and_then(Value::as_array).map(|x| x.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut cmd = tokio::process::Command::new(hub.cfg.get("claudeCommand").and_then(Value::as_str).unwrap_or("claude"));
    cmd.args(&pre).args(&args).current_dir(&a.dir).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    { cmd.creation_flags(0x0800_0000); }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    if let Some(r) = u.runs.lock().unwrap().get_mut(&a.username) { r.1 = child.id(); }
    if let Some(mut stdin) = child.stdin.take() { let inp = if note.is_empty() { text.to_string() } else { format!("{text}\n\n(system note: {note})") }; tokio::spawn(async move { let _ = stdin.write_all(inp.as_bytes()).await; let _ = stdin.shutdown().await; }); }
    let mut fin = String::new();
    if let Some(out) = child.stdout.take() {
        let mut lines = tokio::io::BufReader::new(out).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let Ok(ev) = serde_json::from_str::<Value>(l.trim()) else { continue };
            if let Some(sid) = ev["session_id"].as_str() { let sid = sid.to_string(); u.set_state(a, |m| { m.insert("claudeSession".into(), json!(sid)); }); }
            if ev["type"] == json!("assistant") { for c in ev["message"]["content"].as_array().cloned().unwrap_or_default() { if c["type"] == json!("text") { if let Some(t) = c["text"].as_str().filter(|t| !t.is_empty()) { ci.send(json!({ "type": "token", "text": format!("{t}\n") })); } } } }
            if ev["type"] == json!("result") { if let Some(r) = ev["result"].as_str() { fin = r.to_string(); } }
        }
    }
    let _ = child.wait().await;
    u.claude_out.lock().unwrap().remove(&a.username);
    Ok(fin.trim().to_string())
}

/// The team chat's Unity: one local-brain answer for whoever called her (their tools, time and place); pictures go to
/// Images/<scope>.
pub async fn ask(hub: &Arc<Hub>, a: &Account, text: &str, extra_system: &str, scope: &str) -> Result<(String, Vec<String>, Vec<String>), String> {
    let mut ci = hub.mods.unity.client_info(&json!({}), a); ci.scope = scope.into();
    let cancel = super::openai::Cancel::default();
    let r = run_local(hub, a, text, &mut ci, &cancel, Opts { extra_system: extra_system.into(), no_history: true, ..Default::default() }).await?;
    Ok((r, ci.images, ci.media))
}
/// Facts for a sign-in hello: their local time, weather and headlines.
pub async fn greeting(hub: &Arc<Hub>, a: &Account) -> String {
    let u = &hub.mods.unity; let mut ci = u.client_info(&json!({}), a);
    let mut facts = vec![format!("Their local time: {} ({}).", ut::local_now(&ci.tz), ci.tz)];
    if ci.place.lat.is_some() {
        if let Ok(w) = u.web.weather(&mut ci.place, "", 1).await { facts.push(format!("Weather in {}: {}; today {}.", s(&w, "place"), s(&w, "now"), w["days"][0].as_str().unwrap_or(""))); }
        if let Ok(n) = u.web.news(&mut ci.place, "", true).await { let items: Vec<String> = n["items"].as_array().cloned().unwrap_or_default().iter().take(3).map(|x| s(x, "title")).collect(); if !items.is_empty() { facts.push(format!("Local news ({}): {}", s(&n, "about"), items.join(" | "))); } }
    } else { facts.push("Their location is unknown (no weather or local news).".into()); }
    facts.join("\n")
}
/// Due reminders become notices their page picks up (every 15 s).
pub fn start(hub: &Arc<Hub>) {
    let h = hub.clone();
    tokio::spawn(async move { loop {
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        let u = &h.mods.unity;
        for a in h.mods.auth.accounts() {
            if !a.dir.join("assistant.json").exists() { continue; }
            let fired: Vec<Value> = { let st = u.store(&a); arr(&st, "reminders").into_iter().filter(|r| r["fired"] != json!(true) && r["at"].as_i64().map(|t| t <= now()).unwrap_or(false)).collect() };
            if fired.is_empty() { continue; }
            u.with_store(&a, |st| { for r in st["reminders"].as_array_mut().unwrap() { if fired.iter().any(|f| f["id"] == r["id"]) { r["fired"] = json!(true); } } });
            let mut n = u.notices.lock().unwrap(); let l = n.entry(a.username.clone()).or_default();
            for r in fired { l.push(json!({ "kind": "reminder", "id": r["id"], "text": r["text"], "at": r["at"] })); }
        }
    } });
    // warm up at start (not in a test hub): Ollama and the engines
    if hub.cfg.get("assistant") != Some(&json!(false)) && hub.cfg.get("claudeCommand").is_none() {
        let h = hub.clone();
        tokio::spawn(async move { let u = &h.mods.unity; u.ensure_ollama(&h).await; tokio::join!(u.ensure_image(&h), u.ensure_voice(&h), u.ensure_ears(&h)); });
    }
}
