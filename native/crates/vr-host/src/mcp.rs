//! `vr-host mcp pc|board|team`: the MCP stdio servers a Work run's Claude Code starts (bridge/pc-tools-mcp.js,
//! board-mcp.js, team-mcp.js). Each tool call is relayed to the hub (MILLS_BRIDGE + /internal/<pc-tool|board|team>)
//! with the run's own token (MILLS_INTERNAL), naming the person (MILLS_ACCOUNT). The tool lists are the JavaScript
//! servers' own (mcp-tools.json, taken from them; the regression suite checks they stay the same).
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const TOOLS: &str = include_str!("mcp-tools.json");

pub async fn serve(which: &str) -> i32 {
    let (route, server) = match which { "pc" => ("pc-tool", "mills-pc"), "board" => ("board", "mills-board"), "team" => ("team", "mills-team"), _ => { eprintln!("vr-host mcp pc | board | team"); return 1; } };
    let bridge = std::env::var("MILLS_BRIDGE").unwrap_or_else(|_| "http://127.0.0.1:7777".into());
    let secret = std::env::var("MILLS_INTERNAL").unwrap_or_default();
    let user = std::env::var("MILLS_USER").unwrap_or_else(|_| "the user".into());
    let account = std::env::var("MILLS_ACCOUNT").unwrap_or_default();
    let tools: Value = serde_json::from_str(&TOOLS.replace("{USER}", &user.replace('"', "'"))).expect("mcp-tools.json");
    let tools = tools[which].clone();
    let http = match vr_core::hub::http_client(std::time::Duration::from_secs(15)) { Ok(c) => c, Err(e) => { eprintln!("{e}"); return 1; } };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || { for line in std::io::stdin().lock().lines() { match line { Ok(l) => { if tx.send(l).is_err() { break; } } Err(_) => break } } });
    let out = std::sync::Arc::new(std::sync::Mutex::new(std::io::stdout()));
    let say = move |v: Value| { let mut o = out.lock().unwrap(); let _ = writeln!(o, "{v}"); let _ = o.flush(); };
    while let Some(line) = rx.recv().await {
        let line = line.trim(); if line.is_empty() { continue; }
        let Ok(msg) = serde_json::from_str::<Value>(line) else { continue };
        let Some(id) = msg.get("id").cloned() else { continue }; // a notification
        let method = msg["method"].as_str().unwrap_or("");
        let reply = |result: Value| json!({ "jsonrpc": "2.0", "id": id, "result": result });
        match method {
            "initialize" => say(reply(json!({ "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"), "capabilities": { "tools": {} }, "serverInfo": { "name": server, "version": "1.0.0" } }))),
            "ping" => say(reply(json!({}))),
            "tools/list" => say(reply(json!({ "tools": tools }))),
            "tools/call" => {
                let (http, bridge, secret, account, say) = (http.clone(), bridge.clone(), secret.clone(), account.clone(), say.clone());
                let (name, args) = (msg["params"]["name"].clone(), msg["params"].get("arguments").cloned().unwrap_or(json!({})));
                let id = id.clone();
                tokio::spawn(async move {
                    let r = http.post(format!("{bridge}/internal/{route}")).header("X-Mills-Internal", &secret).header("X-Mills-Account", &account).json(&json!({ "name": name, "args": args })).timeout(std::time::Duration::from_secs(300)).send().await;
                    let result = match r { Ok(r) => match r.json::<Value>().await {
                        Ok(out) => { let ok = out["ok"].as_bool() == Some(true); let text = if ok { if which_is_pc(route) && out["result"].is_string() { out["result"].as_str().unwrap_or("").to_string() } else { serde_json::to_string_pretty(&out["result"]).unwrap_or_default() } } else { format!("Error: {}", out["error"].as_str().unwrap_or("failed")) }; json!({ "content": [{ "type": "text", "text": text }], "isError": !ok }) }
                        Err(e) => json!({ "content": [{ "type": "text", "text": format!("Hub unreachable: {e}") }], "isError": true }) },
                        Err(e) => json!({ "content": [{ "type": "text", "text": format!("Hub unreachable: {e}") }], "isError": true }) };
                    say(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                });
            }
            m => say(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("Method not found: {m}") } })),
        }
    }
    0
}
fn which_is_pc(route: &str) -> bool { route == "pc-tool" }

/// The tool names per server (for the regression suite's comparison with the JavaScript servers).
pub fn tool_names() -> Value {
    let t: Value = serde_json::from_str(TOOLS).expect("mcp-tools.json");
    json!({ "pc": names(&t["pc"]), "board": names(&t["board"]), "team": names(&t["team"]) })
}
fn names(v: &Value) -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|t| t["name"].as_str().map(String::from)).collect()).unwrap_or_default() }
