//! An MCP client over a command run inside a copy (`podman exec -i`): one long-lived stdio session per copy, so a
//! program (the hub for Work runs, Jcode PGM, Hyper Prod Man) can use the MCP tools that live INSIDE the container
//! (REA's 229) without installing anything on the host. Newline-delimited JSON-RPC 2.0 (the MCP stdio transport).
//!
//! [`McpSession::open`] starts the server, does `initialize` + `notifications/initialized`; [`McpSession::tools`] lists
//! (cached); [`McpSession::call`] calls one tool. Calls may overlap (each waits for its own answer). A server that
//! exits, or a pipe that breaks, makes the session dead ([`McpSession::alive`]); the caller opens a new one.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::runtime::{Proc, Runtime};

/// Why a request did not give a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpError {
    /// the server did not answer in time (the call may still be running inside; it was asked to cancel)
    Timeout,
    /// the server ended or its pipe broke (with the last of its stderr)
    Closed(String),
    /// the server answered with a JSON-RPC error: (code, message)
    Rpc(i64, String),
}
impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::Timeout => f.write_str("The tool did not answer in time."),
            McpError::Closed(e) => write!(f, "The MCP server inside the copy has ended{}.", if e.is_empty() { String::new() } else { format!(" ({})", e.trim()) }),
            McpError::Rpc(c, m) => write!(f, "{m} (MCP error {c})"),
        }
    }
}

type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>;
type Stdin = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct McpSession {
    stdin: Stdin,
    pending: Pending,
    next: AtomicU64,
    kill: Mutex<Option<Box<dyn FnMut() + Send>>>,
    stderr_tail: Arc<Mutex<String>>,
    alive: Arc<AtomicBool>,
    tools: Mutex<Option<Vec<Value>>>,
    /// the server's `initialize` answer (serverInfo, protocolVersion, capabilities)
    pub info: Value,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }
fn send(w: &Stdin, v: &Value) -> bool { let mut w = lock(w); writeln!(w, "{v}").and_then(|_| w.flush()).is_ok() }

impl McpSession {
    /// Run `argv` inside the copy `name` and initialize an MCP session with it.
    pub fn open(rt: &dyn Runtime, name: &str, argv: &[String], init_timeout: Duration) -> Result<McpSession, McpError> {
        let proc = rt.spawn(name, argv).map_err(McpError::Closed)?;
        Self::over(proc, init_timeout)
    }

    /// An MCP session over any process's pipes (a copy's `exec -i`, or a local command in a test).
    pub fn over(proc: Proc, init_timeout: Duration) -> Result<McpSession, McpError> {
        let Proc { stdin, stdout, stderr, kill } = proc;
        let stdin: Stdin = Arc::new(Mutex::new(stdin));
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        { // stdout: answers go to the waiting request; the server's own requests (ping) are answered; notifications ignored
            let (pending, alive, stdin) = (pending.clone(), alive.clone(), stdin.clone());
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue }; // a banner line, not JSON-RPC
                    match (v.get("id").cloned().filter(|i| !i.is_null()), v.get("method").is_some()) {
                        (Some(id), true) => {
                            let reply = if v["method"] == json!("ping") { json!({ "jsonrpc": "2.0", "id": id, "result": {} }) } else { json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "Method not supported by this client." } }) };
                            send(&stdin, &reply);
                        }
                        (Some(id), false) => { if let Some(tx) = id.as_u64().and_then(|n| lock(&pending).remove(&n)) { let _ = tx.send(v); } }
                        _ => {}
                    }
                }
                alive.store(false, Ordering::SeqCst);
                lock(&pending).clear(); // every waiting request is told: its sender is dropped
            });
        }
        { // stderr: the last 2 KB, for the message when the server dies
            let tail = stderr_tail.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(stderr); let mut buf = String::new();
                while let Ok(n) = r.read_line(&mut buf) {
                    if n == 0 { break; }
                    let mut t = lock(&tail); t.push_str(&buf);
                    if t.len() > 2048 { let at = (t.len() - 2048..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(t.len()); t.drain(..at); }
                    buf.clear();
                }
            });
        }
        let mut s = McpSession { stdin, pending, next: AtomicU64::new(1), kill: Mutex::new(Some(kill)), stderr_tail, alive, tools: Mutex::new(None), info: Value::Null };
        let init = s.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "pgm-sandbox-internal", "version": env!("CARGO_PKG_VERSION") } }), init_timeout);
        match init {
            Ok(v) => { s.info = v; s.notify("notifications/initialized", json!({})); Ok(s) }
            Err(e) => { s.close(); Err(e) }
        }
    }

    pub fn alive(&self) -> bool { self.alive.load(Ordering::SeqCst) }

    fn closed(&self) -> McpError { McpError::Closed(lock(&self.stderr_tail).trim().lines().last().unwrap_or("").to_string()) }

    /// A notification (no answer).
    pub fn notify(&self, method: &str, params: Value) { send(&self.stdin, &json!({ "jsonrpc": "2.0", "method": method, "params": params })); }

    /// One request: the `result`, or why not. A timed-out call is cancelled (`notifications/cancelled`) and forgotten.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, McpError> {
        if !self.alive() { return Err(self.closed()); }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        lock(&self.pending).insert(id, tx);
        if !send(&self.stdin, &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })) { lock(&self.pending).remove(&id); return Err(self.closed()); }
        match rx.recv_timeout(timeout) {
            Ok(v) => match v.get("error") {
                Some(e) => Err(McpError::Rpc(e["code"].as_i64().unwrap_or(-32000), e["message"].as_str().unwrap_or("error").to_string())),
                None => Ok(v["result"].clone()),
            },
            Err(mpsc::RecvTimeoutError::Timeout) => { lock(&self.pending).remove(&id); self.notify("notifications/cancelled", json!({ "requestId": id, "reason": "timeout" })); Err(McpError::Timeout) }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.closed()),
        }
    }

    /// Every tool the server has (all pages), as the server described them; cached for the life of the session.
    pub fn tools(&self, timeout: Duration) -> Result<Vec<Value>, McpError> {
        if let Some(t) = &*lock(&self.tools) { return Ok(t.clone()); }
        let (mut all, mut cursor) = (vec![], None::<String>);
        for _ in 0..50 {
            let r = self.request("tools/list", cursor.as_ref().map(|c| json!({ "cursor": c })).unwrap_or(json!({})), timeout)?;
            all.extend(r["tools"].as_array().cloned().unwrap_or_default());
            match r["nextCursor"].as_str().filter(|c| !c.is_empty()) { Some(c) => cursor = Some(c.to_string()), None => break }
        }
        *lock(&self.tools) = Some(all.clone());
        Ok(all)
    }

    /// One tool call: the server's `tools/call` result (`content`, `isError`, …) unchanged, so its markers survive.
    pub fn call(&self, tool: &str, args: &Value, timeout: Duration) -> Result<Value, McpError> {
        self.request("tools/call", json!({ "name": tool, "arguments": if args.is_object() { args.clone() } else { json!({}) } }), timeout)
    }

    /// End the server (and the process holding its pipes). Idempotent.
    pub fn close(&self) {
        self.alive.store(false, Ordering::SeqCst);
        *lock(&self.stdin) = Box::new(std::io::sink()); // the server sees its input end and quits by itself
        if let Some(mut k) = lock(&self.kill).take() { k(); }
        lock(&self.pending).clear();
    }
}
impl Drop for McpSession { fn drop(&mut self) { self.close(); } }

/// The tool list as a short answer for a model: with no filter `{ count, of, names }` (names only); with `filter`, the tools whose name or description contains it (case-insensitive) as `{ name, description (first line, 160 chars) }`; with `tool`, that one tool in full (229 full schemas would be far too big to read; this is the way in.)
pub fn summarize(tools: &[Value], filter: &str, tool: &str) -> Value {
    if !tool.is_empty() {
        return match tools.iter().find(|t| t["name"] == json!(tool)) { Some(t) => json!({ "tool": t }), None => json!({ "error": format!("No tool named {tool}: call re_tools without a tool for the list, or with filter to search it.") }) };
    }
    let f = filter.to_lowercase();
    if f.is_empty() { // 229 descriptions would be about 12k tokens: the names alone first, then filter to read descriptions
        let names: Vec<Value> = tools.iter().map(|t| t["name"].clone()).collect();
        return json!({ "count": names.len(), "of": tools.len(), "names": names, "note": "Names only. Call again with filter (a word) for names with a one-line description, or tool=<name> for one tool's full description and input schema." });
    }
    let list: Vec<Value> = tools.iter().filter(|t| f.is_empty() || t["name"].as_str().unwrap_or("").to_lowercase().contains(&f) || t["description"].as_str().unwrap_or("").to_lowercase().contains(&f)).map(|t| {
        let d = t["description"].as_str().unwrap_or("").lines().next().unwrap_or("");
        let d: String = d.chars().take(160).collect();
        json!({ "name": t["name"], "description": d })
    }).collect();
    json!({ "count": list.len(), "of": tools.len(), "tools": list })
}

/// A call result that is too big for a model's context: `Some((head, bytes))` when the text of its `content` blocks is
/// longer than `cap`; `head` is the first `cap` bytes (cut at a character), `bytes` the full size. The caller saves the whole.
pub fn too_big(result: &Value, cap: usize) -> Option<(String, usize)> {
    let full = result.to_string();
    if full.len() <= cap { return None; }
    let text: String = result["content"].as_array().map(|a| a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")).filter(|t| !t.is_empty()).unwrap_or_else(|| full.clone());
    let mut end = cap.min(text.len()); while !text.is_char_boundary(end) { end -= 1; }
    Some((text[..end].to_string(), full.len()))
}
