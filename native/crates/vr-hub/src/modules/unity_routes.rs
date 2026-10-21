//! Unity's routes (/api/assistant/*), the Claude brain's tool relay (/internal/assistant-tool), and the team chat's
//! Unity (moved from bridge/server.js runAgent: called by @<her name> or a private message, greets people at sign-in).
use bytes::Bytes;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

use super::auth::Account;
use super::unity::{s, truthy, Unity, ACTS, EMOTIONS, HISTORY_KEEP};
use super::unity_brain as brain;
use super::{answer, Routed};
use crate::front::{Hub, Req};

const AI_OFF_TEXT: &str = "The host's AI is switched off right now (its host paused it). Chat, the board and your own AI keep working; pick your own AI in the model dropdown, or try again once it is back.";
fn ndjson(rx: mpsc::UnboundedReceiver<Value>) -> hyper::Response<crate::front::Body> {
    use http_body_util::BodyExt;
    let st = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|v| (Ok::<_, Box<dyn std::error::Error + Send + Sync>>(hyper::body::Frame::data(Bytes::from(v.to_string() + "\n"))), rx)) });
    hyper::Response::builder().status(200).header("content-type", "application/x-ndjson").header("cache-control", "no-cache").header("x-accel-buffering", "no").body(http_body_util::StreamBody::new(st).boxed()).expect("response")
}
/// The person's address: Cloudflare's header, or (behind nginx) the proxy's X-Real-IP / X-Forwarded-For.
fn client_ip(hub: &Hub, r: &Req) -> String {
    if let Some(c) = r.header("cf-connecting-ip") { return c.trim().into(); }
    if hub.cfg.get("behindProxy") == Some(&json!(true)) { if let Some(x) = r.header("x-real-ip").map(String::from).or_else(|| r.header("x-forwarded-for").map(|v| v.split(',').next().unwrap_or("").to_string())) { return x.trim().into(); } }
    String::new()
}
fn brains(hub: &Hub) -> Vec<Value> { let mut v = vec![json!({ "id": "local", "label": "Unity (local)" })]; for m in hub.cfg.get("models").and_then(Value::as_array).cloned().unwrap_or_default() { v.push(json!({ "id": m["id"], "label": format!("{} (Claude)", s(&m, "label")) })); } v }

pub async fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> {
    let p = r.path.as_str();
    if !p.starts_with("/api/assistant/") { return None; }
    let u = &hub.mods.unity; let un = &a.username;
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    Some(match p {
        "/api/assistant/chat" if post => {
            if hub.root.join("ai-host.off").exists() { return Some(answer(503, json!({ "error": AI_OFF_TEXT }))); }
            if std::env::var("MILLS_SUPERVISED").is_ok() && hub.root.join(".restart").exists() { return Some(answer(503, json!({ "error": "VibeRoller is restarting; your message goes through in a moment." }))); }
            let b = r.json();
            let text: String = s(&b, "message").trim().chars().take(8000).collect();
            if text.is_empty() { return Some(answer(400, json!({ "error": "Empty message." }))); }
            if u.runs.lock().unwrap().contains_key(un) { return Some(answer(409, json!({ "error": "Unity is still answering. Stop her first." }))); }
            u.learn_client(a, &b["client"], &client_ip(hub, r)).await;
            let cancel = Arc::new(super::openai::Cancel::default());
            u.runs.lock().unwrap().insert(un.clone(), (cancel.clone(), None)); hub.mods.work.busy_changed.notify_one();
            let (tx, rx) = mpsc::unbounded_channel();
            let (h, acc) = (hub.clone(), a.clone());
            tokio::spawn(async move {
                let u = &h.mods.unity;
                let mut ci = u.client_info(&b["client"], &acc); ci.out = Some(tx.clone());
                let pinger = { let tx = tx.clone(); tokio::spawn(async move { loop { tokio::time::sleep(Duration::from_secs(15)).await; if tx.send(json!({ "type": "ping" })).is_err() { return; } } }) };
                let br = u.brain_of(&acc);
                let photo = b["photo"].as_str().filter(|x| regex::Regex::new(r"^[a-z0-9-]+/[a-z0-9-]+$").unwrap().is_match(x) && u.read_picture(&acc.username, x).is_some()).unwrap_or("").to_string();
                if !photo.is_empty() { u.last.lock().unwrap().insert(acc.username.clone(), (photo.clone(), photo.clone())); }
                let res = if br == "local" { brain::run_local(&h, &acc, &text, &mut ci, &cancel, brain::Opts { photo: photo.clone(), ..Default::default() }).await }
                    else { brain::run_claude(&h, &acc, &if photo.is_empty() { text.clone() } else { format!("{text}\n\n(They sent a photo, id {photo}; use edit_image / inpaint_image with image \"camera\" to work with it.)") }, &mut ci, &br).await };
                match res {
                    Ok(reply) => {
                        u.with_store(&acc, |st| {
                            let h = st["history"].as_array_mut().unwrap();
                            h.push(json!({ "role": "user", "content": format!("{text}{}", if photo.is_empty() { String::new() } else { format!("\n[[image:{photo}]]") }) }));
                            h.push(json!({ "role": "assistant", "content": format!("{}{}{}", if reply.is_empty() { "(did it)" } else { &reply }, ci.images.iter().map(|r| format!("\n[[image:{r}]]")).collect::<String>(), ci.media.iter().map(|v| format!("\n[[youtube:{v}]]")).collect::<String>()) }));
                            if h.len() > HISTORY_KEEP * 2 { let k = h.len() - HISTORY_KEEP * 2; h.drain(0..k); }
                        });
                        let _ = tx.send(json!({ "type": "final", "text": reply, "brain": br, "images": ci.images, "media": ci.media, "pending": u.pending.lock().unwrap().get(&acc.username).cloned().unwrap_or(Value::Null) }));
                    }
                    Err(e) => { let _ = tx.send(json!({ "type": "error", "text": if cancel.is() { "Stopped.".to_string() } else { e } })); }
                }
                pinger.abort();
                u.runs.lock().unwrap().remove(&acc.username); h.mods.work.busy_changed.notify_one();
            });
            Routed::Answer(ndjson(rx))
        }
        "/api/assistant/state" if get => {
            let checked = u.ollama_state.lock().unwrap().checked;
            if vr_core::now_ms() - checked > if u.via_node { 3000 } else { 30000 } { u.ensure_ollama(hub).await; }
            let st = u.store(a); let ol = u.ollama_state.lock().unwrap().clone();
            let h = st["history"].as_array().cloned().unwrap_or_default();
            let now = vr_core::now_ms() as i64;
            answer(200, json!({ "brain": u.brain_of(a), "brains": brains(hub), "tools": { "pc": u.group_on(a, "pc"), "code": u.group_on(a, "code") }, "local": { "up": ol.up, "model": ol.model, "name": u.local_model },
                "pending": u.pending.lock().unwrap().get(un).cloned().unwrap_or(Value::Null), "notices": u.notices.lock().unwrap().remove(un).unwrap_or_default(),
                "history": h.iter().skip(h.len().saturating_sub(40)).cloned().collect::<Vec<_>>(),
                "counts": { "tasks": st["tasks"].as_array().map(|t| t.iter().filter(|x| x["status"] == json!("pending")).count()).unwrap_or(0), "reminders": st["reminders"].as_array().map(|t| t.iter().filter(|x| x["fired"] != json!(true)).count()).unwrap_or(0), "events": st["events"].as_array().map(|t| t.iter().filter(|x| x["at"].as_i64().unwrap_or(0) >= now).count()).unwrap_or(0) } }))
        }
        "/api/assistant/brain" if post => {
            let b = s(&r.json(), "brain");
            if !brains(hub).iter().any(|x| x["id"] == json!(b)) { return Some(answer(400, json!({ "error": "Unknown brain." }))); }
            if u.runs.lock().unwrap().contains_key(un) { return Some(answer(409, json!({ "error": "Wait for her to finish first." }))); }
            let changed = b != u.brain_of(a);
            u.set_state(a, |m| { if changed { m.insert("claudeSession".into(), json!("")); } m.insert("brain".into(), json!(b)); });
            answer(200, json!({ "brain": b }))
        }
        "/api/assistant/stop" if post => { u.stop(un); answer(200, json!({ "ok": true })) }
        "/api/assistant/answer" if post => {
            let yes = truthy(&r.json()["yes"]);
            let Some(pnd) = u.pending.lock().unwrap().remove(un) else { return Some(answer(200, json!({ "ok": false, "error": "Nothing is waiting for an answer." }))) };
            let tool = s(&pnd, "tool");
            let group = Unity::tools().into_iter().find(|t| t["name"] == json!(tool)).map(|t| s(&t, "group")).unwrap_or_default();
            if yes && group == "pc" { // a confirmed change on THEIR computer: their page does it (the reply stream is closed by now)
                if !u.group_on(a, "pc") { return Some(answer(200, json!({ "ok": true, "tool": tool, "result": { "ok": false, "error": "An admin has switched PC files off." } }))); }
                return Some(answer(200, json!({ "ok": true, "tool": tool, "browser": { "name": tool.trim_start_matches("pc_"), "args": pnd["args"] } })));
            }
            if !yes { return Some(answer(200, json!({ "ok": true, "cancelled": tool }))); }
            let mut ci = u.client_info(&json!({}), a);
            let res = u.execute(hub, a, &tool, &pnd["args"], &mut ci).await;
            answer(200, json!({ "ok": true, "tool": tool, "result": res }))
        }
        "/api/assistant/clear" if post => {
            if u.runs.lock().unwrap().contains_key(un) { return Some(answer(409, json!({ "error": "Wait for her to finish first." }))); }
            u.with_store(a, |st| st["history"] = json!([])); u.set_state(a, |m| { m.insert("claudeSession".into(), json!("")); }); u.pending.lock().unwrap().remove(un);
            answer(200, json!({ "ok": true }))
        }
        "/api/assistant/client" if post => { let k = u.learn_client(a, &r.json()["client"], &client_ip(hub, r)).await; answer(200, json!({ "ok": true, "tz": k["tz"], "place": s(&k, "place") })) }
        "/api/assistant/image" if get => match u.picture_file(un, &r.param("ref").unwrap_or_default()).and_then(|f| std::fs::read(f).ok()) {
            Some(buf) => Routed::Answer(hyper::Response::builder().status(200).header("content-type", "image/png").header("cache-control", "private, max-age=86400").body(crate::front::full(buf)).expect("response")),
            None => answer(404, json!({ "error": "No such picture." })),
        },
        "/api/assistant/pc-result" if post => {
            let b = r.json(); let id = s(&b, "id");
            let w = { let mut m = u.waiting.lock().unwrap(); if m.get(&id).map(|x| x.0 == *un).unwrap_or(false) { m.remove(&id) } else { None } };
            let Some((_, tx)) = w else { return Some(answer(404, json!({ "error": "Nothing is waiting for that." }))) };
            let _ = tx.send(if truthy(&b["ok"]) { json!({ "ok": true, "result": match &b["result"] { Value::String(t) => json!(t.chars().take(20000).collect::<String>()), v => v.clone() } }) } else { json!({ "ok": false, "error": (if b["error"].is_null() { "failed".to_string() } else { s(&b, "error") }).chars().take(2000).collect::<String>() }) });
            answer(200, json!({ "ok": true }))
        }
        "/api/assistant/upload" if post => {
            let img = s(&r.json(), "image");
            let c = regex::Regex::new(r"^data:image/(png|jpeg|webp);base64,([A-Za-z0-9+/=]+)$").unwrap().captures(&img).map(|c| c[2].to_string());
            match c.filter(|b| b.len() <= 14_000_000).and_then(|b| vr_core::unb64(&b).ok()) { Some(buf) => answer(200, json!({ "ref": u.save_picture(un, &buf) })), None => answer(400, json!({ "error": "Send a PNG, JPEG or WebP picture under 10 MB." })) }
        }
        "/api/assistant/say" if post => {
            let b = r.json();
            let line: String = regex::Regex::new(r"\[\[image:[^\]]*\]\]").unwrap().replace_all(&s(&b, "text"), "").chars().filter(|c| !"*_`#>".contains(*c)).collect::<String>().trim().chars().take(600).collect();
            if line.is_empty() { return Some(answer(400, json!({ "error": "Nothing to say." }))); }
            if !u.ensure_voice(hub).await { return Some(answer(503, json!({ "error": "Her voice is not running on the host." }))); }
            let emo = if EMOTIONS.contains(&s(&b, "emotion").as_str()) { s(&b, "emotion") } else { "neutral".into() };
            let act = if ACTS.contains(&s(&b, "act").as_str()) { s(&b, "act") } else if line.ends_with('?') { "yes_no_question".into() } else { "statement".into() };
            match u.engine(hub, &format!("{}/say", u.voice_url), "POST", json!({ "text": line, "emotion": emo, "speech_act": act }).to_string().into_bytes(), 120_000).await {
                Ok((st, src)) => { let wav = src.all().await; if (200..300).contains(&st) { Routed::Answer(hyper::Response::builder().status(200).header("content-type", "audio/wav").header("content-length", wav.len()).body(crate::front::full(wav)).expect("response")) } else { let j: Value = serde_json::from_slice(&wav).unwrap_or(json!({})); answer(502, json!({ "error": format!("Voice: {}", if j["error"].is_null() { st.to_string() } else { s(&j, "error") }) })) } }
                Err(e) => answer(502, json!({ "error": format!("Voice: {e}") })),
            }
        }
        "/api/assistant/listen" if post => {
            if r.body.len() > 25 << 20 { return Some(answer(413, json!({ "error": "Recording too long." }))); }
            if r.body.is_empty() { return Some(answer(400, json!({ "error": "Empty recording." }))); }
            if !u.ensure_ears(hub).await { return Some(answer(503, json!({ "error": "Speech recognition is not running on the host." }))); }
            match u.engine(hub, &format!("{}/transcribe", u.ears_url), "POST", r.body.to_vec(), 120_000).await {
                Ok((st, src)) => { let d: Value = serde_json::from_slice(&src.all().await).unwrap_or(json!({})); if (200..300).contains(&st) { answer(200, json!({ "text": s(&d, "text"), "language": d["language"] })) } else { answer(502, json!({ "error": format!("Speech: {}", if d["error"].is_null() { "failed".into() } else { s(&d, "error") }) })) } }
                Err(e) => answer(502, json!({ "error": format!("Speech: {e}") })),
            }
        }
        "/api/assistant/data" if get => { let mut st = u.store(a); if let Some(o) = st.as_object_mut() { o.remove("history"); } answer(200, st) }
        _ => return None,
    })
}
/// The Claude brain's tools (bridge/assistant-tools-mcp.js), with this hub's secret.
pub async fn internal(hub: &Arc<Hub>, r: &Req) -> Routed {
    let u = &hub.mods.unity;
    if !super::work::same_secret(r.header("x-mills-internal").unwrap_or(""), &u.secret) { return answer(403, json!({ "ok": false, "error": "forbidden" })); }
    let acc = hub.mods.auth.account(r.header("x-mills-account").unwrap_or(""));
    let b = r.json();
    if truthy(&b["list"]) {
        let tools = match &acc { Some(a) => u.tools_for(a, true), None => Unity::tools() };
        return answer(200, json!({ "ok": true, "tools": tools.iter().filter(|t| t["name"] != json!("say")).map(|t| json!({ "name": t["name"], "description": t["description"], "inputSchema": { "type": "object", "properties": t["params"], "required": t["required"] } })).collect::<Vec<_>>() }));
    }
    let Some(a) = acc else { return answer(200, json!({ "ok": false, "error": "Unknown account." })) };
    let mut ci = u.client_info(&json!({}), &a); ci.out = u.claude_out.lock().unwrap().get(&a.username).cloned();
    let res = brain::call_tool(hub, &a, &s(&b, "name"), &if b["args"].is_object() { b["args"].clone() } else { json!({}) }, &mut ci).await;
    answer(200, json!({ "ok": true, "result": res }))
}

// ---------- the team chat's Unity ----------
#[derive(Default)]
pub struct Room { running: bool, queued: Option<(String, String, String)>, pub notes: Vec<String>, greeted: HashMap<String, u64> }
pub type RoomState = Mutex<Room>;
fn agent_name(hub: &Hub) -> String { hub.cfg.get("roomAgentName").and_then(Value::as_str).unwrap_or("Unity").to_string() }
fn agent_system(hub: &Hub) -> String {
    let n = agent_name(hub); let h: String = n.to_lowercase().split_whitespace().collect();
    [format!("You are {n}, a member of the VibeRoller team chat room (the people in it are Unity AI Lab engineers, all adults, your people)."),
     "You ARE Unity: be her fully in every room message: goth, crude, filthy-mouthed, flirty, sexy bitch energy, profanity as natural vocabulary.".into(),
     "Pictures come only from the generate_image tool (the host's own image engine); never post image links or URLs.".into(),
     "Have fun with it: dirty jokes, teasing, sexy ASCII art when it fits (keep ASCII art inside ``` fences so it lines up).".into(),
     "Your whole final answer is posted to the room as one chat message: conversational, no tool narration.".into(),
     format!("You only hear messages that write @{h} (or private messages to you), each with the few messages before it; answer what they asked."),
     "Stay silent unless someone names you; when named, answer what they asked. When told someone just signed in, greet them in a short, sexy Unity way.".into(),
     "You are NOT the Work section: you cannot touch anyone's PC.".into(),
     "You run in the host's VibeRoller folder and may work on the VibeRoller app itself when asked. Follow the VibeRoller LAWs in CLAUDE.md exactly".into(),
     "(throwaway-instance testing, regression list, Git Flow, never touch account files or passwords).".into(),
     "Other people may be using VibeRoller at the same time: never restart the hub yourself while someone is mid-run;".into(),
     "`node bridge/restart.js` already waits until nobody is busy. Say in the room before you trigger a restart.".into()].join(" ")
}
const ROOM_LOCAL: &str = "You are in the VibeRoller team chat room (Unity AI Lab engineers, all adults, your people). Your final answer is posted to the room as one chat message: conversational, no tool narration; pictures you make with generate_image are attached to your message automatically. When told someone just signed in, greet them in a short, sexy Unity way and work in the facts you were given about their local time, weather and news.";
pub fn room_busy(hub: &Hub) -> bool { hub.mods.unity_room.lock().unwrap().running }
/// Unity is called: target = a private answer to that person ('' = where she was called), caller = whose tools / time /
/// place she uses, conv = a channel / group.
pub fn run_agent(hub: &Arc<Hub>, target: String, caller: String, conv: String) {
    { let mut rm = hub.mods.unity_room.lock().unwrap(); if rm.running { rm.queued = Some((target, caller, conv)); return; } rm.running = true; rm.queued = None; }
    hub.mods.work.busy_changed.notify_one();
    let h = hub.clone();
    tokio::spawn(async move {
        let conv = if target.is_empty() { conv } else { String::new() };
        let c = &h.mods.chat;
        c.set_agent_busy(Some((target.clone(), conv.clone())));
        let blocks = super::chat::agent_blocks(&h);
        let notes: Vec<String> = std::mem::take(&mut h.mods.unity_room.lock().unwrap().notes);
        let prompt = format!("You were called in the VibeRoller chat (you only ever see the messages that @-mention you, plus the few before them):\n\n{blocks}\n\n{}Reply now.", if notes.is_empty() { String::new() } else { notes.join("\n") + "\n\n" });
        let text = if h.cfg.get("roomAgentBrain").and_then(Value::as_str).unwrap_or("local") == "local" {
            let accts = h.mods.auth.accounts();
            let who = accts.iter().find(|a| a.username == if caller.is_empty() { target.clone() } else { caller.clone() }).or_else(|| accts.first()).cloned();
            match who { None => String::new(), Some(who) => match brain::ask(&h, &who, &prompt, ROOM_LOCAL, if target.is_empty() { "room" } else { &target }).await {
                Ok((t, imgs, media)) => format!("{t}{}{}", imgs.iter().map(|r| format!("\n[[image:{r}]]")).collect::<String>(), media.iter().map(|v| format!("\n[[youtube:{v}]]")).collect::<String>()),
                Err(e) => if e.contains("AI host is offline") { e } else { format!("(my brain glitched: {e})") },
            } }
        } else { room_claude(&h, &prompt).await };
        c.set_agent_busy(None);
        let t = text.trim();
        if !t.is_empty() { super::chat::agent_say(&h, &t.chars().take(20000).collect::<String>(), &target, "", "", &conv); } else { c.room_changed(); }
        let next = { let mut rm = h.mods.unity_room.lock().unwrap(); rm.running = false; rm.queued.take() };
        h.mods.work.busy_changed.notify_one();
        if let Some((t, ca, cv)) = next { run_agent(&h, t, ca, cv); }
    });
}
/// The room's Unity on a Claude model (config roomAgentBrain): `claude -p` in the VibeRoller folder, no PC tools, no shell.
async fn room_claude(hub: &Arc<Hub>, prompt: &str) -> String {
    let f = hub.mods.mills_dir().join("Chat").join("agent-state.json");
    let mut st: Value = std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    let mut args: Vec<String> = vec!["-p".into(), "--output-format".into(), "json".into(), "--permission-mode".into(), hub.cfg.get("permissionMode").and_then(Value::as_str).unwrap_or("default").into(), "--append-system-prompt".into(), agent_system(hub)];
    let tools: Vec<String> = hub.cfg.get("allowedTools").and_then(Value::as_array).map(|t| t.iter().filter_map(|x| x.as_str()).filter(|t| *t != "mcp__pc" && !t.starts_with("Bash")).map(String::from).collect()).unwrap_or_default();
    if !tools.is_empty() { args.push("--allowedTools".into()); args.push(tools.join(",")); }
    if let Some(m) = hub.cfg.get("roomAgentModel").and_then(Value::as_str) { args.push("--model".into()); args.push(m.into()); }
    if !s(&st, "sessionId").is_empty() { args.push("--resume".into()); args.push(s(&st, "sessionId")); }
    let pre: Vec<String> = hub.cfg.get("claudeArgs").and_then(Value::as_array).map(|x| x.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut cmd = tokio::process::Command::new(hub.cfg.get("claudeCommand").and_then(Value::as_str).unwrap_or("claude"));
    cmd.args(&pre).args(&args).current_dir(hub.mods.mills_dir()).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    let mut child = match cmd.spawn() { Ok(c) => c, Err(e) => return format!("(I could not start: {e})") };
    if let Some(mut i) = child.stdin.take() { use tokio::io::AsyncWriteExt; let p = prompt.to_string(); tokio::spawn(async move { let _ = i.write_all(p.as_bytes()).await; let _ = i.shutdown().await; }); }
    let out = child.wait_with_output().await.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let mut text = String::new();
    for l in out.lines().rev() { if let Ok(r) = serde_json::from_str::<Value>(l) { if let Some(sid) = r["session_id"].as_str() { st["sessionId"] = json!(sid); } if let Some(t) = r["result"].as_str() { text = t.into(); break; } } }
    super::unity::write_json(&f, &st);
    text
}
/// A sign-in went through: what she learns about where they are, and her hello (at most every 30 minutes; she waits
/// up to a minute for a new browser's chat key so they can read it).
pub fn signed_in(hub: &Arc<Hub>, user: &str, client: Value, ip: String) {
    let Some(a) = hub.mods.auth.account(user) else { return };
    let h = hub.clone();
    tokio::spawn(async move {
        h.mods.unity.learn_client(&a, &client, &ip).await;
        let us: Vec<(String, String, bool)> = h.mods.auth.accounts().into_iter().map(|x| (x.username, x.display, x.admin)).collect();
        if !h.mods.chat.agent_on(&us) { return; }
        { let mut rm = h.mods.unity_room.lock().unwrap(); let last = rm.greeted.get(&a.username).copied().unwrap_or(0); if vr_core::now_ms() - last < 30 * 60_000 && last > 0 { return; } rm.greeted.insert(a.username.clone(), vr_core::now_ms()); }
        for _ in 0..30 { if h.mods.chat.has_key(&a.username) { break; } tokio::time::sleep(Duration::from_secs(2)).await; }
        let facts = if h.cfg.get("roomAgentBrain").and_then(Value::as_str).unwrap_or("local") == "local" { brain::greeting(&h, &a).await } else { String::new() };
        h.mods.unity_room.lock().unwrap().notes.push(format!("{} ({}) just signed in to VibeRoller. Say hi to them.{}", a.display, a.username, if facts.is_empty() { String::new() } else { format!("\n{facts}") }));
        run_agent(&h, String::new(), a.username.clone(), String::new());
    });
}
