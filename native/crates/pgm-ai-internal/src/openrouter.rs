//! OpenRouter and the split routing of Work (T-370, team note plans/split-routing.md).
//!
//! **The provider.** A provider of kind `openrouter` (admins only, like an `openai` endpoint) is an OpenAI-compatible
//! provider whose address is fixed (`https://openrouter.ai/api/v1`), with the team's key (stored like every provider key,
//! never shown back) and an allow-list of **pairs**: `{ id, label, model, providers, allowFallbacks }` = what people pick
//! (`id`, also the `model` on /v1/*), the OpenRouter model id, OpenRouter's `provider.order` and whether OpenRouter may
//! fall back to other providers (default false). The provider's `models` are the pair ids, so only pairs are offered and
//! routed. Every request to OpenRouter ([`post`]) carries the pair's model, `provider: { order, allow_fallbacks }`,
//! `usage: { include: true }` (OpenRouter then puts the answer's cost in `usage.cost`), `HTTP-Referer` (the hub's public
//! address) and `X-Title: VibeRoller`. The gateway's raw passthrough, its translations and the hub's own text paths all
//! send through [`post`].
//!
//! **The month budget.** Per person per calendar month (UTC) in USD, added up from `usage.cost` of each answer (streamed
//! or not; [`note_cost`]); the cap is config `openrouter.monthlyUsd` (default 20) unless an admin set the person's own.
//! At or over it the next request is refused ([`refusal`]: 429 in the caller's shape, naming the cap and when it resets).
//! Kept in `<root>/profiles/<user>/openrouter.json` `{ capUsd?, fallback?: { on, provider, pair }, months: { "YYYY-MM":
//! { usd, calls } } }`: written by the Rust hub only (the frozen JS hub neither reads nor needs it), in the person's
//! profile folder, which is data in every deploy / pack / restore / hand-off list already.
//!
//! **The router (Work).** The engine follows the provider: a Claude provider (an AI host's login, this hub's Claude, an
//! Anthropic key, a Claude Code login) runs Claude Code as before; an `openrouter` provider, or another one the hub calls
//! itself over chat completions (`openai`, `host-openai`), runs **Jcode PGM on this server** ([`work_plan`]): config
//! `jcode.command` (+ optional `jcode.args`; empty = this path is off and such providers are not offered for Work, with
//! the reason), in the session's worktree through the shared jcode harness (harness.rs / jcode.rs, the same code the AI
//! host's jcode Work runs with: private jcode home, the team MCP servers as its mcp.json, the system prompt as its
//! overlay, pgm-proxy's shared pool off). jcode's OpenRouter provider is pointed at this hub's **run relay**
//! (`/or-relay/<run token>`, this machine only, for the run's life): the relay sends the call on through [`post`] with the
//! hub's key, so the key never enters the run (not its command line, not its environment, where the AI's own shell could
//! read it), the pair's model and routing are enforced whatever jcode asks for, and the cost lands in the budget.
//!
//! **Fallback (opt-in).** A person may turn on "use OpenRouter when Claude is unavailable" with a pair: when the session's
//! Claude (an AI host) is offline or at its usage cap, the run uses that pair through Jcode PGM and the transcript says so
//! in one line ([`fallback_for`]). Off by default (OpenRouter costs money).
use chrono::Datelike;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use crate::hub::{answer, AiHub, Resp};
use pgm_core_internal::json::{read_json, s, write_json_private as write_json};
use pgm_core_internal::now_ms;
use pgm_identity_internal::Account;
use pgm_net_internal::http::Req;

/// The provider kind (bridge/providers.json).
pub const KIND: &str = "openrouter";
/// OpenRouter's API (the only address an `openrouter` provider talks to; config `openrouter.baseUrl` is for the
/// regression runner's fake).
pub const BASE: &str = "https://openrouter.ai/api/v1";
pub const DEFAULT_MONTHLY_USD: f64 = 20.0;
/// What Work says when the server has no Jcode PGM to run these providers with.
pub const NO_JCODE: &str = "OpenRouter models (and the other API models this hub calls itself) run Work through Jcode PGM on the server, and this hub has none set up: an admin installs jcodepgm and sets jcode.command in the hub's config. Pick a Claude in the dropdown, or use this model through Settings → API access.";
const RELAY: &str = "/or-relay/";
const MAX_PAIRS: usize = 30;

/// Where OpenRouter is: `https://openrouter.ai/api/v1`, or config `openrouter.baseUrl` (the regression runner's fake).
pub fn base_url(hub: &dyn AiHub) -> String {
    hub.cfg().get("openrouter").and_then(|o| o["baseUrl"].as_str()).map(|b| b.trim().trim_end_matches('/').to_string()).filter(|b| b.starts_with("http://") || b.starts_with("https://")).unwrap_or_else(|| BASE.into())
}
/// The provider kinds a Work run reaches through Jcode PGM on this hub: the ones the hub calls itself over chat completions.
pub fn direct_kind(kind: &str) -> bool { matches!(kind, KIND | "openai" | "host-openai") }
fn list_of(v: &Value) -> Vec<String> {
    match v { Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(|t| t.trim().to_string())).filter(|t| !t.is_empty()).collect(), Value::String(t) => t.split(',').map(|x| x.trim().to_string()).filter(|t| !t.is_empty()).collect(), _ => vec![] }
}

// ---------- pairs ----------
/// The allow-list as an admin typed it, checked: `[{ id, label, model, providers, allowFallbacks }]`.
pub fn clean_pairs(v: &Value) -> Result<Vec<Value>, String> {
    let Some(a) = v.as_array() else { return Err("pairs must be a list of { id, label, model, providers, allowFallbacks }.".into()) };
    if a.is_empty() { return Err("List at least one model + provider pair (id, model, providers).".into()); }
    if a.len() > MAX_PAIRS { return Err(format!("At most {MAX_PAIRS} pairs.")); }
    let id_ok = |t: &str| !t.is_empty() && t.len() <= 60 && t.chars().all(|c| c.is_ascii_alphanumeric() || "-._/:@".contains(c));
    let mut out: Vec<Value> = vec![];
    for x in a {
        let id = s(x, "id").trim().to_string();
        if !id_ok(&id) { return Err(format!("A pair's id is what people pick (e.g. or/sonnet): 1 to 60 letters, digits and - . _ / : @ (got \"{}\").", id.chars().take(60).collect::<String>())); }
        if out.iter().any(|p| p["id"] == json!(id)) { return Err(format!("The pair id \"{id}\" is there twice.")); }
        let model = s(x, "model").trim().to_string();
        if model.is_empty() || model.len() > 120 || model.chars().any(|c| c.is_whitespace() || c.is_control()) { return Err(format!("The pair \"{id}\" needs its OpenRouter model id (e.g. anthropic/claude-sonnet-4.5).")); }
        let label: String = { let l = s(x, "label").trim().chars().take(60).collect::<String>(); if l.is_empty() { id.clone() } else { l } };
        let providers = list_of(&x["providers"]);
        if providers.len() > 10 || providers.iter().any(|p| p.len() > 60 || !p.chars().all(|c| c.is_ascii_alphanumeric() || "-._/ ".contains(c))) { return Err(format!("The pair \"{id}\": providers are OpenRouter provider names (at most 10, e.g. anthropic, amazon-bedrock).")); }
        out.push(json!({ "id": id, "label": label, "model": model, "providers": providers, "allowFallbacks": x["allowFallbacks"] == json!(true) }));
    }
    Ok(out)
}
pub fn pairs_of(p: &Value) -> Vec<Value> { p["pairs"].as_array().cloned().unwrap_or_default() }
/// The pair with this id (exactly).
pub fn pair(p: &Value, id: &str) -> Option<Value> { pairs_of(p).into_iter().find(|x| x["id"] == json!(id)) }
/// A request body as OpenRouter gets it: the pair's model (the one `model` names, else the first: nothing outside the
/// allow-list ever goes), its provider routing, and the usage with the cost.
pub fn shape(p: &Value, body: &mut Value) {
    if !body.is_object() { return; }
    let ps = pairs_of(p); let asked = s(body, "model");
    if let Some(pr) = ps.iter().find(|x| s(x, "id") == asked).or(ps.first()) {
        body["model"] = pr["model"].clone();
        let mut route = json!({ "allow_fallbacks": pr["allowFallbacks"] == json!(true) });
        let order = list_of(&pr["providers"]);
        if !order.is_empty() { route["order"] = json!(order); }
        body["provider"] = route;
    }
    body["usage"] = json!({ "include": true });
}
/// A POST to OpenRouter at `path` (under its API base) with the provider's key, the hub's referer and title, the body
/// shaped by [`shape`].
pub fn post(hub: &dyn AiHub, p: &Value, path: &str, body: &[u8]) -> reqwest::RequestBuilder {
    let mut b: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    shape(p, &mut b);
    crate::openai::self_http(hub).post(format!("{}{path}", base_url(hub))).header("content-type", "application/json").header("authorization", format!("Bearer {}", s(p, "apiKey")))
        .header("HTTP-Referer", hub.public_url()).header("X-Title", "VibeRoller").body(serde_json::to_vec(&b).unwrap_or_default())
}

// ---------- the month budget: <root>/profiles/<user>/openrouter.json ----------
static LOCK: Mutex<()> = Mutex::new(());
fn rec_file(hub: &dyn AiHub, u: &str) -> PathBuf { hub.root().join("profiles").join(u).join("openrouter.json") }
fn rec(hub: &dyn AiHub, u: &str) -> Value { read_json(&rec_file(hub, u)).filter(Value::is_object).unwrap_or_else(|| json!({})) }
fn save(hub: &dyn AiHub, u: &str, v: &Value) { let f = rec_file(hub, u); if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); } write_json(&f, v); }
fn edit(hub: &dyn AiHub, u: &str, f: impl FnOnce(&mut Value)) { let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner()); let mut r = rec(hub, u); f(&mut r); save(hub, u, &r); }
/// This month (UTC), "YYYY-MM".
pub fn month() -> String { chrono::Utc::now().format("%Y-%m").to_string() }
/// When this month's budget resets: the first of next month, 00:00 UTC.
pub fn resets_on() -> String { let t = chrono::Utc::now().date_naive(); let (y, m) = if t.month() == 12 { (t.year() + 1, 1) } else { (t.year(), t.month() + 1) }; format!("{y:04}-{m:02}-01") }
fn month_name(m: &str) -> String {
    const N: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let (y, mm) = m.split_once('-').unwrap_or(("", "")); mm.parse::<usize>().ok().filter(|n| (1..=12).contains(n)).map(|n| format!("{} {y}", N[n - 1])).unwrap_or_else(|| m.to_string())
}
fn usd(x: f64) -> f64 { (x * 1_000_000.0).round() / 1_000_000.0 }
/// The cap everyone has unless an admin set their own: config `openrouter.monthlyUsd`, else 20 USD.
pub fn default_cap(hub: &dyn AiHub) -> f64 { hub.cfg().get("openrouter").and_then(|o| o["monthlyUsd"].as_f64()).filter(|n| *n >= 0.0).unwrap_or(DEFAULT_MONTHLY_USD) }
/// (the person's cap in USD, whether it is their own)
pub fn cap_of(hub: &dyn AiHub, u: &str) -> (f64, bool) { match rec(hub, u)["capUsd"].as_f64().filter(|n| *n >= 0.0) { Some(c) => (c, true), None => (default_cap(hub), false) } }
/// (spent this month in USD, answers this month)
pub fn spent_of(hub: &dyn AiHub, u: &str) -> (f64, u64) { let r = rec(hub, u); let m = &r["months"][month()]; (m["usd"].as_f64().unwrap_or(0.0), m["calls"].as_u64().unwrap_or(0)) }
/// The plain refusal when the person is at or over this month's cap (None = they may go on).
pub fn refusal(hub: &dyn AiHub, u: &str) -> Option<String> {
    let (spent, _) = spent_of(hub, u); let (cap, _) = cap_of(hub, u);
    (spent >= cap).then(|| format!("Your OpenRouter budget for {} is used up: {:.2} of {:.2} USD. It resets on {} (00:00 UTC); an admin can raise your cap in Settings → OpenRouter budget.", month_name(&month()), spent, cap, resets_on()))
}
/// Adds one answer's cost (`usage.cost`, OpenRouter's own number in USD) to the person's month.
pub fn note_cost(hub: &dyn AiHub, u: &str, usage: &Value) {
    if !pgm_identity_internal::valid_username(u) || !usage.is_object() { return; }
    let cost = usage["cost"].as_f64().or_else(|| usage["cost"].as_str().and_then(|t| t.trim().parse().ok())).filter(|c: &f64| c.is_finite() && *c > 0.0).unwrap_or(0.0);
    let m = month();
    edit(hub, u, |r| {
        let e = &mut r["months"][m.as_str()];
        e["usd"] = json!(usd(e["usd"].as_f64().unwrap_or(0.0) + cost)); e["calls"] = json!(e["calls"].as_u64().unwrap_or(0) + 1);
        // a year and a month of history is plenty
        if let Some(ms) = r["months"].as_object_mut() { let mut keys: Vec<String> = ms.keys().cloned().collect(); keys.sort(); let n = keys.len(); for k in keys.into_iter().take(n.saturating_sub(13)) { ms.remove(&k); } }
    });
}
/// The person's month as the page shows it.
pub fn month_view(hub: &dyn AiHub, u: &str) -> Value {
    let (spent, calls) = spent_of(hub, u); let (cap, own) = cap_of(hub, u);
    json!({ "month": month(), "spentUsd": usd(spent), "capUsd": cap, "ownCap": own, "defaultCapUsd": default_cap(hub), "calls": calls, "leftUsd": usd((cap - spent).max(0.0)), "over": spent >= cap, "resetsAt": format!("{}T00:00:00Z", resets_on()), "fallback": fallback_of(hub, u) })
}
/// Everyone's month (admins).
pub fn all_view(hub: &dyn AiHub) -> Value {
    let mut people: Vec<Value> = hub.accounts().iter().map(|a| { let (spent, calls) = spent_of(hub, &a.username); let (cap, own) = cap_of(hub, &a.username); json!({ "username": a.username, "name": hub.name_of(&a.username), "spentUsd": usd(spent), "capUsd": cap, "ownCap": own, "calls": calls, "over": spent >= cap }) }).collect();
    people.sort_by(|a, b| b["spentUsd"].as_f64().unwrap_or(0.0).total_cmp(&a["spentUsd"].as_f64().unwrap_or(0.0)).then(s(a, "username").cmp(&s(b, "username"))));
    json!({ "month": month(), "resetsAt": format!("{}T00:00:00Z", resets_on()), "defaultCapUsd": default_cap(hub), "people": people })
}

// ---------- fallback (opt-in) ----------
/// The person's setting: `{ on, provider, pair }`.
pub fn fallback_of(hub: &dyn AiHub, u: &str) -> Value { let f = rec(hub, u)["fallback"].clone(); json!({ "on": f["on"] == json!(true), "provider": s(&f, "provider"), "pair": s(&f, "pair") }) }
fn set_fallback(hub: &dyn AiHub, u: &str, b: &Value) -> Result<Value, String> {
    let on = b["on"] == json!(true);
    let (prov, pr) = (s(b, "provider"), s(b, "pair"));
    if on {
        let Some(p) = hub.work().provider_get(hub.ai(), &prov, u).filter(|p| p["kind"] == json!(KIND)) else { return Err("Pick an OpenRouter provider you may use.".into()) };
        if pair(&p, &pr).is_none() { return Err(format!("\"{pr}\" is not one of that provider's models.")); }
    }
    edit(hub, u, |r| { r["fallback"] = json!({ "on": on, "provider": prov, "pair": pr }); });
    Ok(fallback_of(hub, u))
}
/// When the session's Claude cannot run now (its AI host is offline or at its usage cap, or there is no AI at all) and
/// the person turned the fallback on: (the OpenRouter provider, the pair id, the line for the transcript). None = run as
/// chosen (the usual refusal then says why it cannot).
pub fn fallback_for(hub: &dyn AiHub, u: &str, chosen: Option<&Value>) -> Option<(Value, String, String)> {
    let why = match chosen {
        None => "No Claude is available right now".to_string(),
        Some(p) if p["kind"] == json!("host-claude") => {
            let n = { let x = s(p, "nickname"); if x.is_empty() { s(p, "label") } else { x } };
            if !hub.ai().online(&s(p, "hostId")) { format!("Claude ({n}) is offline") } else if !s(p, "capped").is_empty() { format!("Claude ({n}) is at its usage cap") } else { return None }
        }
        _ => return None,
    };
    let f = fallback_of(hub, u);
    if f["on"] != json!(true) { return None; }
    let p = hub.work().provider_get(hub.ai(), &s(&f, "provider"), u).filter(|p| p["kind"] == json!(KIND))?;
    let pr = pair(&p, &s(&f, "pair"))?;
    if jcode_cmd(hub).is_err() || refusal(hub, u).is_some() { return None; }
    Some((p, s(&pr, "id"), format!("{why}, so this run uses OpenRouter ({}) through Jcode PGM on the server, as your setting \"use OpenRouter when Claude is unavailable\" says.", s(&pr, "label"))))
}

// ---------- Jcode PGM on this server (Work) ----------
/// The Jcode PGM this server runs Work with: config `jcode.command` (a path, or a name on PATH) and `jcode.args`. Err =
/// why this path is off (empty, or not there).
pub fn jcode_cmd(hub: &dyn AiHub) -> Result<(String, Vec<String>), String> {
    let j = hub.cfg().get("jcode").cloned().unwrap_or(Value::Null);
    let c = s(&j, "command").trim().to_string();
    if c.is_empty() { return Err(NO_JCODE.into()); }
    let found = if c.contains('/') || c.contains('\\') { Path::new(&c).is_file() } else { std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(&c).is_file() || d.join(format!("{c}.exe")).is_file())).unwrap_or(false) };
    if !found { return Err(format!("OpenRouter models run Work through Jcode PGM on the server, and the jcodepgm this hub is set up with ({c}) is not there: an admin installs it or fixes jcode.command. Pick a Claude in the dropdown meanwhile.")); }
    Ok((c, list_of(&j["args"])))
}
/// Can this provider run Work here? Some(reason) when it cannot (a direct kind with no Jcode PGM). None = it can, or it is
/// not a direct kind.
pub fn work_off(hub: &dyn AiHub, p: &Value) -> Option<String> { if direct_kind(p["kind"].as_str().unwrap_or("")) { jcode_cmd(hub).err() } else { None } }
/// How a Work run of a direct-kind provider goes: the jcode harness's provider entry (jcode.rs `command`: the binary, its
/// args, `-p openrouter`, the model), the model jcode runs, the model id the relay routes to (a pair id for OpenRouter),
/// the environment (no key in it: the relay's address is added once the run has its token, [`relay_env`]).
pub struct WorkPlan { pub prov: Value, pub model: String, pub routed: String, pub env: Vec<(String, String)>, pub label: String }
pub fn work_plan(hub: &dyn AiHub, u: &str, p: &Value, state_model: &str) -> Result<WorkPlan, String> {
    let (cmd, args) = jcode_cmd(hub)?;
    let kind = s(p, "kind");
    let (routed, model, label, order, fallbacks) = if kind == KIND {
        if let Some(m) = refusal(hub, u) { return Err(m); }
        if s(p, "apiKey").is_empty() { return Err("This OpenRouter provider has no key set; an admin adds it in Settings → My AIs.".into()); }
        let ps = pairs_of(p);
        let Some(pr) = ps.iter().find(|x| s(x, "id") == state_model).or(ps.first()).cloned() else { return Err("This OpenRouter provider offers no models.".into()) };
        (s(&pr, "id"), s(&pr, "model"), format!("OpenRouter · {}", s(&pr, "label")), list_of(&pr["providers"]), pr["allowFallbacks"] == json!(true))
    } else {
        let ms = list_of(&p["models"]);
        let m = if ms.iter().any(|x| x == state_model) { state_model.to_string() } else { ms.first().cloned().unwrap_or_default() };
        (m.clone(), m, s(p, "label"), vec![], true)
    };
    if model.is_empty() { return Err("This provider lists no model to run Work with.".into()); }
    let mut env: Vec<(String, String)> = vec![
        // jcode needs a key to start its OpenRouter provider; this is not one (the relay adds the hub's own)
        ("OPENROUTER_API_KEY".into(), "viberoller-relay".into()),
        ("JCODE_OPENROUTER_PROVIDER_FEATURES".into(), "0".into()), // the hub routes (provider.order) on the relay
        ("JCODE_OPENROUTER_MODEL_CATALOG".into(), "0".into()),
        ("JCODE_OPENROUTER_MODEL".into(), model.clone()),
    ];
    if !order.is_empty() { env.push(("JCODE_OPENROUTER_PROVIDER".into(), order.join(","))); }
    if !fallbacks { env.push(("JCODE_OPENROUTER_NO_FALLBACK".into(), "1".into())); }
    let prov = json!({ "kind": crate::jcode::KIND, "label": label, "command": cmd, "args": args, "provider": "openrouter", "models": [model] });
    Ok(WorkPlan { prov, model, routed, env, label })
}
struct RelayEnt { user: String, prov: String, routed: String, exp: u64 }
static RELAYS: LazyLock<Mutex<HashMap<String, RelayEnt>>> = LazyLock::new(Mutex::default);
/// A run's own relay token (for its life; 12 hours at most).
pub fn relay_token(user: &str, prov: &str, routed: &str) -> String {
    let t = pgm_core_internal::crypto::random_hex(24); let now = now_ms();
    let mut m = RELAYS.lock().unwrap_or_else(|e| e.into_inner());
    m.retain(|_, e| e.exp > now);
    m.insert(t.clone(), RelayEnt { user: user.into(), prov: prov.into(), routed: routed.into(), exp: now + 12 * 3_600_000 });
    t
}
pub fn relay_forget(t: &str) { RELAYS.lock().unwrap_or_else(|e| e.into_inner()).remove(t); }
/// The run's jcode reaches OpenRouter (or the provider) through this hub: its API base.
pub fn relay_env(port: u16, token: &str) -> Vec<(String, String)> { vec![("JCODE_OPENROUTER_API_BASE".into(), format!("http://127.0.0.1:{port}{RELAY}{token}"))] }
fn work_key() -> crate::openai::KeyCtx { crate::openai::KeyCtx { id: "work-run".into(), label: "Work run".into(), named: true, ..Default::default() } }
/// `/or-relay/<token>/chat/completions` (and `/models`): a Work run's jcode reaching its provider, from this machine only.
/// The call goes on through the gateway's raw chat passthrough as the run's person, to the run's provider and model
/// (whatever model the call names), so the key, the routing, the budget and the usage are the hub's.
pub async fn relay(hub: &Arc<dyn AiHub>, r: &Req) -> Resp {
    let rest = &r.path[RELAY.len().min(r.path.len())..];
    let (tok, sub) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let behind = hub.cfg().get("behindProxy") == Some(&json!(true));
    let outside = r.header("cf-connecting-ip").is_some() || (behind && (r.header("x-forwarded-for").is_some() || r.header("x-real-ip").is_some()));
    let ent = if tok.len() == 48 && r.peer.ip().is_loopback() && !outside { RELAYS.lock().unwrap_or_else(|e| e.into_inner()).get(tok).filter(|e| e.exp > now_ms()).map(|e| (e.user.clone(), e.prov.clone(), e.routed.clone())) } else { None };
    let Some((user, pid, routed)) = ent else { return crate::openai::oa_error(401, "Unknown relay.", "authentication_error") };
    let Some(p) = hub.work().provider_get(hub.ai(), &pid, &user) else { return crate::openai::oa_error(503, "The provider of this run is gone.", "service_unavailable") };
    let real = if p["kind"] == json!(KIND) { pair(&p, &routed).map(|x| s(&x, "model")).unwrap_or_else(|| routed.clone()) } else { routed.clone() };
    match (r.method.as_str(), sub) {
        ("GET", "/models") => answer(200, json!({ "object": "list", "data": [{ "id": real, "object": "model", "created": 0, "owned_by": s(&p, "label") }] })),
        ("POST", "/chat/completions") => {
            let Ok(mut b) = serde_json::from_slice::<Value>(&r.body) else { return crate::openai::oa_error(400, "Body is not JSON.", "invalid_request_error") };
            if !b.is_object() { return crate::openai::oa_error(400, "Body is not a JSON object.", "invalid_request_error"); }
            b["model"] = json!(routed);
            crate::gateway::chat_raw(hub, &user, &work_key(), &p, &routed, b).await
        }
        _ => crate::openai::oa_error(404, "This relay serves POST /chat/completions and GET /models.", "not_found_error"),
    }
}

// ---------- routes (signed in) ----------
/// `GET /api/openrouter/budget` (the person's month and fallback), `GET /api/openrouter/budget/all` (admins: everyone's
/// month), `POST /api/openrouter/budget/cap { username, capUsd | null }` (admins), `POST /api/openrouter/fallback { on,
/// provider, pair }` (the person's own).
pub fn route(hub: &Arc<dyn AiHub>, r: &Req, a: &Account) -> Option<Resp> {
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let u = &a.username;
    Some(match r.path.as_str() {
        "/api/openrouter/budget" if get => answer(200, month_view(&**hub, u)),
        "/api/openrouter/budget/all" if get => if hub.is_admin(u) { answer(200, all_view(&**hub)) } else { answer(403, json!({ "error": "Admins only." })) },
        "/api/openrouter/budget/cap" if post => {
            if !hub.is_admin(u) { return Some(answer(403, json!({ "error": "Admins only." }))); }
            let b = r.json(); let who = s(&b, "username");
            if hub.account(&who).is_none() { return Some(answer(400, json!({ "error": "No such account." }))); }
            let cap = match &b["capUsd"] { Value::Null => None, v => match v.as_f64().filter(|n| n.is_finite() && *n >= 0.0 && *n <= 100_000.0) { Some(n) => Some(usd(n)), None => return Some(answer(400, json!({ "error": "capUsd is a number of US dollars from 0 to 100000, or null for the default." }))) } };
            edit(&**hub, &who, |rc| { match cap { Some(c) => { rc["capUsd"] = json!(c); } None => { if let Some(o) = rc.as_object_mut() { o.remove("capUsd"); } } } });
            pgm_core_internal::audit::record(u, "openrouter.cap", &who, json!({ "capUsd": cap }), "");
            answer(200, all_view(&**hub))
        }
        "/api/openrouter/fallback" if post => match set_fallback(&**hub, u, &r.json()) { Ok(v) => answer(200, json!({ "fallback": v })), Err(e) => answer(400, json!({ "error": e })) },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pairs_are_checked_and_cleaned() {
        let ok = clean_pairs(&json!([{ "id": "or/sonnet", "label": "Sonnet", "model": "anthropic/claude-sonnet-4.5", "providers": "anthropic, amazon-bedrock" }, { "id": "or/qwen", "model": "qwen/qwen3-coder", "providers": ["cerebras"], "allowFallbacks": true }])).unwrap();
        assert_eq!(ok[0], json!({ "id": "or/sonnet", "label": "Sonnet", "model": "anthropic/claude-sonnet-4.5", "providers": ["anthropic", "amazon-bedrock"], "allowFallbacks": false }));
        assert_eq!(ok[1]["label"], "or/qwen"); assert_eq!(ok[1]["allowFallbacks"], true);
        for bad in [json!([]), json!("x"), json!([{ "id": "", "model": "m" }]), json!([{ "id": "a b", "model": "m" }]), json!([{ "id": "a", "model": "" }]), json!([{ "id": "a", "model": "m" }, { "id": "a", "model": "n" }]), json!([{ "id": "a", "model": "m", "providers": ["x;y"] }])] {
            assert!(clean_pairs(&bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn a_request_gets_the_pairs_model_routing_and_usage_and_nothing_outside_the_list() {
        let p = json!({ "kind": "openrouter", "pairs": [{ "id": "or/sonnet", "model": "anthropic/claude-sonnet-4.5", "providers": ["anthropic"], "allowFallbacks": false }, { "id": "or/qwen", "model": "qwen/qwen3-coder", "providers": [], "allowFallbacks": true }] });
        let mut b = json!({ "model": "or/sonnet", "messages": [{ "role": "user", "content": "hi" }], "stream": true, "tools": [{ "type": "function" }] });
        shape(&p, &mut b);
        assert_eq!(b["model"], "anthropic/claude-sonnet-4.5"); assert_eq!(b["provider"], json!({ "allow_fallbacks": false, "order": ["anthropic"] })); assert_eq!(b["usage"], json!({ "include": true }));
        assert_eq!(b["tools"][0]["type"], "function"); assert_eq!(b["stream"], true); // everything else as it came
        let mut q = json!({ "model": "or/qwen" }); shape(&p, &mut q); assert_eq!(q["provider"], json!({ "allow_fallbacks": true })); assert_eq!(q["model"], "qwen/qwen3-coder");
        let mut other = json!({ "model": "openai/gpt-5" }); shape(&p, &mut other); assert_eq!(other["model"], "anthropic/claude-sonnet-4.5"); // an unlisted model never goes
    }
    #[test]
    fn months_and_names() {
        assert_eq!(month_name("2026-11"), "November 2026"); assert_eq!(month_name("x"), "x");
        assert_eq!(month().len(), 7); assert!(resets_on().ends_with("-01")); assert!(resets_on() > month());
        assert!(direct_kind("openrouter") && direct_kind("openai") && direct_kind("host-openai") && !direct_kind("host-claude") && !direct_kind("host-codex") && !direct_kind("jcode"));
        assert_eq!(list_of(&json!("a, b,,c")), ["a", "b", "c"]);
    }
}
