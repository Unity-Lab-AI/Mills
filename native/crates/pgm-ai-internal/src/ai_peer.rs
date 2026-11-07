//! Dev borrows live's AI hosts (T-335, the team note plans/dev-hub.md § 9). Every AI host polls live, so the dev hub has
//! none of its own: live LENDS its connected hosts to dev over the data bridge (127.0.0.1, the shared secret, never through
//! a proxy: pgm-sync-internal's gate). The host never knows dev exists.
//!
//! Live's side ([`providers_answer`], [`may_fetch`], [`may_work`]): the route `/__sync/ai/*` of the hub program (vr-hub
//! `sync.rs`) answers `providers?user=` / `?users=a,b`, `fetch`, `work`, `cancel`, `pc-tool`, the same calls the frozen
//! JavaScript hub makes through `/__front/ai/*`, plus the account they are for; live applies that account's sharing,
//! scopes, caps and owner checks exactly as for its own people.
//!
//! Dev's side ([`Peer`]): a peer source in [`AiHosts`]. Live's lists of providers (per account), its hosts and its engines
//! are kept in memory only, refreshed every few seconds in the background for all of dev's accounts in one call, so a
//! dropdown never waits; a call for a host that is not connected to dev goes to live's route and streams back unchanged.
//! A 2.x host's Work run keeps dev as its hub (`hubUrl` / `mcpBase`): dev keeps the job's id and token ([`PeerJob`]), so the
//! host's MCP relay calls dev's `/node/job/<id>/internal/*` and the run's team tools land on dev.
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::ai_hosts::{AiHosts, HostErr, Relayed, WorkMsg};
use pgm_core_internal::now_ms;

/// What dev adds to "No AI is available …" (and to a borrowed call that failed) while live's AI hosts cannot be reached.
pub const NOT_REACHABLE: &str = "(live's AI hosts are not reachable from dev right now)";
/// How often dev asks live what it lends (and how old a list may be before it is dropped).
pub const REFRESH_MS: u64 = 3000;
const STALE_MS: u64 = 12_000;
/// The header a fetch's description travels in (base64 of its JSON), as `/__front/ai/fetch` takes it.
pub const META_HEADER: &str = "x-vr-ai";
/// Which hub is asking (its sync name, "dev"): live's audit and its host owners' panels say "via <this>".
pub const PEER_HEADER: &str = "x-vr-peer";
/// On live's answer to a borrowed fetch: this is the host's answer (whatever its status), not the route's.
pub const HOST_HEADER: &str = "x-vr-ai-host";

fn herr(status: u16, m: impl Into<String>) -> HostErr { HostErr { status, msg: m.into() } }
fn s(v: &Value, k: &str) -> String { v[k].as_str().unwrap_or("").to_string() }

/// Who a call over the bridge is for (live's side): the account dev names, and the hub that asks.
#[derive(Clone, Debug)]
pub struct Who { pub account: String, pub peer: String }

// ---------------------------------------------------------------- live's side ----------------------------------------
/// `providers?users=`: what live lends each of these accounts (its own connected hosts only, as each would see them on
/// live: shared with them, cut to their scope, with usage and caps), its hosts (online, nickname, owner, session limits)
/// and the engines it can lend. `known` says whether an account exists here: one that does not gets an error instead.
pub fn providers_answer(ai: &AiHosts, users: &[String], known: &dyn Fn(&str) -> bool) -> Value {
    let mut per = Map::new();
    for u in users {
        if !known(u) { per.insert(u.clone(), json!({ "error": format!("Live does not know the account {u}.") })); continue; }
        per.insert(u.clone(), json!({ "providers": ai.providers_for_here(u) }));
    }
    json!({ "users": per, "hosts": ai.lent_hosts(), "engines": crate::ai_hosts::ENGINES.iter().filter(|e| !ai.engine_hosts(e).is_empty()).collect::<Vec<_>>(), "at": now_ms() })
}
/// May `who` make this fetch (provider / login: the provider is shared with them, and the use + model are within their
/// scope; engine: Unity's engines are the hub's, lent to the hub, not to a person)? The error is live's own words.
pub fn may_fetch(ai: &AiHosts, who: &Who, meta: &Value) -> Result<(), HostErr> {
    match s(meta, "mode").as_str() {
        "engine" => Ok(()),
        "provider" | "login" => {
            let (host, prov) = (s(meta, "host"), s(meta, "provider"));
            let pid = format!("h:{host}/{prov}");
            if ai.provider(&pid, &who.account).is_none() { return Err(herr(403, format!("That AI ({pid}) is not shared with {} on live.", who.account))); }
            let use_ = s(meta, "use");
            if use_.is_empty() { return Ok(()); }
            ai.scope_check(&who.account, &pid, &use_, &s(meta, "model")).map_err(|m| herr(403, m))
        }
        m => Err(herr(400, format!("unknown mode {m:?}"))),
    }
}
/// May `who` start this Work run on the host (shared with them, Work within their scope, and a run on the host's own
/// computer only for its owner, as live's own Work refuses it)? The payload's account is the account named.
pub fn may_work(ai: &AiHosts, who: &Who, host: &str, prov: &str, payload: &mut Value) -> Result<(), HostErr> {
    let pid = format!("h:{host}/{prov}");
    let Some(v) = ai.provider(&pid, &who.account) else { return Err(herr(403, format!("That AI ({pid}) is not shared with {} on live.", who.account))) };
    ai.scope_check(&who.account, &pid, "work", &s(payload, "model")).map_err(|m| herr(403, m))?;
    let owner = s(&v, "owner");
    // a sandboxed answer (the API on a 2.x host) touches nothing of the host's; a Work run there runs on its owner's computer
    if payload["sandbox"] == json!(true) { return Ok(()); }
    let on_host = s(&v, "remoteKind") == "claude-cli-work" || s(&v, "kind") == "host-jcode";
    if on_host && owner != who.account { return Err(herr(403, format!("{} runs Work on {}'s computer, and only its owner may work on that computer.", s(&v, "nickname"), if owner.is_empty() { "its own".to_string() } else { owner }))); }
    payload["account"] = json!(who.account);
    Ok(())
}

// ---------------------------------------------------------------- dev's side -----------------------------------------
/// A borrowed host's Work run, as dev keeps it: the job's token (live minted it; the host's MCP relay sends it to dev) and
/// whose run it is.
#[derive(Clone, Debug)]
pub struct PeerJob { pub token: String, pub account: String, pub display: String, pub host: String }

#[derive(Default)]
struct State { ok: bool, why: String, at: u64, checked: u64, users: HashMap<String, Vec<Value>>, hosts: Map<String, Value>, engines: Vec<String>, said: String }

/// Live, as dev borrows from it.
pub struct Peer { url: String, secret: String, name: String, http: reqwest::Client, state: Mutex<State>, pub jobs: Mutex<HashMap<String, PeerJob>> }
/// What live said to a PC tool call.
pub enum PeerPc { Done(Value), Refused(u16, String), Nothing }

impl Peer {
    pub fn new(url: &str, secret: &str, name: &str) -> Option<Peer> {
        let http = pgm_net_internal::client::no_redirects(Duration::from_secs(3)).ok()?;
        Some(Peer { url: url.trim_end_matches('/').to_string(), secret: secret.to_string(), name: if name.is_empty() { "dev".into() } else { name.to_string() }, http, state: Mutex::default(), jobs: Mutex::default() })
    }
    fn st(&self) -> std::sync::MutexGuard<'_, State> { self.state.lock().unwrap_or_else(|e| e.into_inner()) }
    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder { self.http.request(method, format!("{}/__sync/ai/{path}", self.url)).header(pgm_sync_internal_header(), &self.secret).header(PEER_HEADER, &self.name) }
    /// Is live lending right now (its last answer came, and recently)?
    pub fn up(&self) -> bool { let st = self.st(); st.ok && now_ms().saturating_sub(st.at) < STALE_MS }
    /// "" while live lends, else [`NOT_REACHABLE`] (to end an error text with).
    pub fn note(&self) -> &'static str { if self.up() { "" } else { NOT_REACHABLE } }
    /// What live lends this person (each marked viaPeer), when it answered lately.
    pub fn list(&self, user: &str) -> Vec<Value> { if !self.up() { return vec![]; } self.st().users.get(user).cloned().unwrap_or_default() }
    /// One of live's hosts (online, nickname, owner, sessionLimits), when live lends it.
    pub fn host(&self, id: &str) -> Option<Value> { if !self.up() { return None; } self.st().hosts.get(id).cloned() }
    pub fn has_engine(&self, e: &str) -> bool { self.up() && self.st().engines.iter().any(|x| x == e) }
    /// For /api/status (this machine only): lending or not, why not, when dev last asked, how many hosts live lends.
    pub fn summary(&self) -> Value { let up = self.up(); let st = self.st(); json!({ "up": up, "why": st.why, "checkedAt": st.checked, "okAt": st.at, "hosts": st.hosts.len(), "jobs": self.jobs.lock().unwrap_or_else(|e| e.into_inner()).len() }) }
    /// Asks live what it lends these accounts now. A change of state (lending / not, and why) is logged once.
    pub async fn refresh(&self, users: &[String]) {
        // (usernames are lowercase letters, digits and dashes: nothing to encode)
        let q: Vec<&str> = users.iter().map(String::as_str).filter(|u| !u.is_empty() && u.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')).collect();
        let res = async {
            let r = self.req(reqwest::Method::GET, &format!("providers?users={}", q.join(","))).timeout(Duration::from_secs(5)).send().await.map_err(|e| if e.is_connect() { "live is not answering".to_string() } else { e.to_string() })?;
            let st = r.status().as_u16();
            if st == 404 { return Err("live does not lend its AI hosts (no route: an older live, the bridge off there, or sync.shareAi off)".to_string()); }
            let v: Value = r.json().await.map_err(|e| e.to_string())?;
            if st != 200 { return Err(format!("live answered {st}: {}", v["error"].as_str().unwrap_or(""))); }
            Ok::<Value, String>(v)
        }.await;
        let mut st = self.st();
        st.checked = now_ms();
        match res {
            Ok(v) => {
                st.ok = true; st.why.clear(); st.at = now_ms();
                st.users = v["users"].as_object().map(|m| m.iter().map(|(u, x)| (u.clone(), x["providers"].as_array().cloned().unwrap_or_default().into_iter().map(|mut p| { p["viaPeer"] = json!(true); p }).collect())).collect()).unwrap_or_default();
                st.hosts = v["hosts"].as_object().cloned().unwrap_or_default();
                st.engines = v["engines"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect()).unwrap_or_default();
                let n: usize = st.users.values().map(|l| l.len()).max().unwrap_or(0);
                if st.said != "ok" { println!("[ai-host] live's AI hosts are lent to this dev hub ({} host(s), up to {n} provider(s) a person)", st.hosts.len()); st.said = "ok".into(); }
            }
            Err(e) => {
                st.ok = false; st.why = e.clone(); st.users.clear(); st.hosts.clear(); st.engines.clear();
                if st.said != e { println!("[ai-host] live's AI hosts are not reachable from dev right now: {e}"); st.said = e; }
            }
        }
    }
    fn unreachable(&self, what: &str, e: impl std::fmt::Display) -> HostErr { herr(503, format!("{what} could not be reached through live ({e}) {NOT_REACHABLE}")) }
    /// One call carried to a borrowed host (engine / provider / login), as `/__front/ai/fetch` takes it; the answer as it
    /// comes. A refusal (a cap, a scope, the host offline) comes back in live's words.
    pub async fn fetch(&self, meta: Value, body: Vec<u8>) -> Result<Relayed, HostErr> {
        use base64::Engine;
        let what = if s(&meta, "mode") == "engine" { format!("Unity's {} engine", s(&meta, "engine")) } else { "That AI host".to_string() };
        let r = self.req(reqwest::Method::POST, "fetch").header(META_HEADER, base64::engine::general_purpose::STANDARD.encode(meta.to_string())).body(body).timeout(Duration::from_secs(1800)).send().await.map_err(|e| self.unreachable(&what, e))?; // (the whole answer, streamed: a long model answer included)
        let status = r.status().as_u16();
        // a 404 the route itself gave (an older live, the bridge or sync.shareAi off there); a host's own 404 comes marked
        if status == 404 && r.headers().get("x-vr-ai-error").is_none() && r.headers().get(HOST_HEADER).is_none() { return Err(self.unreachable(&what, "no route")); }
        if r.headers().get("x-vr-ai-error").is_some() || status == 403 {
            let v: Value = r.json().await.unwrap_or(Value::Null);
            return Err(herr(status, v["error"].as_str().unwrap_or("Live refused the call.").to_string()));
        }
        let headers: Vec<(String, String)> = r.headers().iter().filter(|(k, _)| pass_header(k.as_str())).filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string()))).collect();
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            use futures_util::StreamExt;
            let mut st = r.bytes_stream();
            while let Some(c) = st.next().await { let m = c.map_err(|e| e.to_string()); let end = m.is_err(); if tx.send(m).await.is_err() || end { return; } }
        });
        Ok(Relayed { status, headers, body: rx })
    }
    /// A Work run on a borrowed host: live's NDJSON (`{id, token}`, `{ev}`…, `{done}` | `{error}`) as the run's messages.
    /// Dev keeps the job's id and token, so the host's MCP relay (which calls dev: the payload's hubUrl is dev's) is known.
    pub async fn work(self: &Arc<Self>, account: &str, host: &str, pid: &str, payload: Value) -> Result<(String, String, mpsc::UnboundedReceiver<WorkMsg>), HostErr> {
        let account = account.to_string();
        let body = json!({ "host": host, "provider": pid, "payload": payload, "account": account });
        let r = self.req(reqwest::Method::POST, "work").json(&body).timeout(Duration::from_secs(24 * 3600)).send().await.map_err(|e| self.unreachable("That AI host", e))?;
        let status = r.status().as_u16();
        if status != 200 { let v: Value = r.json().await.unwrap_or(Value::Null); return Err(if status == 404 { self.unreachable("That AI host", "no route") } else { herr(status, v["error"].as_str().unwrap_or("Live refused the run.").to_string()) }); }
        use futures_util::StreamExt;
        let mut stream = r.bytes_stream();
        let mut buf: Vec<u8> = vec![];
        let first: Value = loop {
            if let Some(i) = buf.iter().position(|b| *b == b'\n') { let l: Vec<u8> = buf.drain(..=i).collect(); if let Ok(v) = serde_json::from_slice::<Value>(&l) { break v; } continue; }
            match stream.next().await { Some(Ok(c)) => buf.extend_from_slice(&c), Some(Err(e)) => return Err(self.unreachable("That AI host", e)), None => return Err(self.unreachable("That AI host", "the answer ended")) }
        };
        if let Some(e) = first["error"].as_str() { return Err(herr(first["status"].as_u64().unwrap_or(503) as u16, e)); }
        let (id, token) = (s(&first, "id"), s(&first, "token"));
        if id.is_empty() { return Err(herr(502, "Live did not say which job the run is.")); }
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), PeerJob { token: token.clone(), account, display: s(&body["payload"], "display"), host: host.to_string() });
        let (tx, rx) = mpsc::unbounded_channel();
        let (me, id2) = (self.clone(), id.clone());
        tokio::spawn(async move {
            let mut ended = false;
            'read: loop {
                while let Some(i) = buf.iter().position(|b| *b == b'\n') {
                    let l: Vec<u8> = buf.drain(..=i).collect();
                    let Ok(v) = serde_json::from_slice::<Value>(&l) else { continue };
                    let (m, end) = if !v["ev"].is_null() { (WorkMsg::Event(v["ev"].clone()), false) } else if !v["done"].is_null() { (WorkMsg::Done(v["done"].as_i64().unwrap_or(0) as i32), true) } else if let Some(e) = v["error"].as_str() { (WorkMsg::Error(e.to_string()), true) } else { continue };
                    // the run went away on dev: the host is told to stop it (through live)
                    if tx.send(m).is_err() { me.cancel(&id2).await; break 'read; }
                    if end { ended = true; break 'read; }
                }
                match stream.next().await { Some(Ok(c)) => buf.extend_from_slice(&c), _ => break }
            }
            if !ended { let _ = tx.send(WorkMsg::Error(format!("The run on the borrowed AI host was cut off {NOT_REACHABLE}"))); }
            // the job's tools stay known a little after its end (a last tool call may still be on its way)
            tokio::time::sleep(Duration::from_secs(30)).await;
            me.jobs.lock().unwrap_or_else(|e| e.into_inner()).remove(&id2);
        });
        Ok((id, token, rx))
    }
    pub fn is_job(&self, id: &str) -> bool { self.jobs.lock().unwrap_or_else(|e| e.into_inner()).contains_key(id) }
    /// Stop on dev: live tells the host to stop the run.
    pub async fn cancel(&self, id: &str) {
        let account = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).get(id).map(|j| j.account.clone()).unwrap_or_default();
        if let Err(e) = self.req(reqwest::Method::POST, "cancel").json(&json!({ "id": id, "account": account })).timeout(Duration::from_secs(10)).send().await { eprintln!("[ai-host] could not ask live to stop the borrowed run {id}: {e}"); }
    }
    /// A PC tool on the account's own machine connected to live (named, or their pc host): live picks it with the T-308
    /// rules and signs the request for them. Nothing = live has none for them (dev goes on as before).
    pub async fn pc(&self, named: &str, name: &str, args: &Value, actor: &pgm_net_internal::machine::Actor) -> PeerPc {
        if !self.up() { return PeerPc::Nothing; }
        let body = json!({ "account": actor.account, "machine": named, "name": name, "args": args, "session": actor.session, "run": actor.run });
        let r = match self.req(reqwest::Method::POST, "pc-tool").json(&body).timeout(Duration::from_secs(150)).send().await { Ok(r) => r, Err(e) => return if named.is_empty() { PeerPc::Nothing } else { PeerPc::Refused(503, format!("Your machine {named} could not be reached through live ({e}) {NOT_REACHABLE}")) } };
        let status = r.status().as_u16();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if status == 404 && v["none"] != json!(true) { return PeerPc::Nothing; }
        if status != 200 { return PeerPc::Refused(status, v["error"].as_str().unwrap_or("Live refused the call.").to_string()); }
        if v["none"] == json!(true) { return PeerPc::Nothing; }
        if let Some(e) = v["error"].as_str() { let st = v["status"].as_u64().unwrap_or(502) as u16; return if st == 404 && named.is_empty() { PeerPc::Nothing } else { PeerPc::Refused(st, e.to_string()) }; }
        PeerPc::Done(v["result"].clone())
    }
}
fn pgm_sync_internal_header() -> &'static str { "x-vr-sync-secret" }
/// The headers of a host's answer passed on (the same names the hub passes on from a host).
pub fn pass_header(k: &str) -> bool { let k = k.to_lowercase(); matches!(k.as_str(), "content-type" | "request-id" | "retry-after") || k.starts_with("anthropic-ratelimit-") || k.starts_with("x-codex-") || k.starts_with("openai-") }

/// The background refresh of what live lends (every [`REFRESH_MS`], for the accounts `users` names).
pub fn start(peer: Arc<Peer>, users: Arc<dyn Fn() -> Vec<String> + Send + Sync>) {
    tokio::spawn(async move { loop {
        let us = users();
        if !us.is_empty() { peer.refresh(&us).await; }
        tokio::time::sleep(Duration::from_millis(REFRESH_MS)).await;
    } });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_known_headers_pass_and_the_note_names_live() {
        assert!(pass_header("Content-Type") && pass_header("anthropic-ratelimit-unified-5h-utilization") && !pass_header("set-cookie") && !pass_header("authorization"));
        assert!(NOT_REACHABLE.starts_with('(') && NOT_REACHABLE.contains("live's AI hosts"));
    }
}
