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
//!
//! **A person's own key (T-372, plans/pgm-accounts-keys-app.md section 2).** A person may keep their own OpenRouter key on
//! the hub (`<root>/profiles/<user>/openrouter-key.json` `{ apiKey, pairs? }`, owner-only, never returned: the page only
//! learns `hasKey` and the last four characters). It shows to that person alone as the virtual provider `or-me:<user>`
//! ("my OpenRouter", `personal: true`, same kind as the team's) with the team's pairs unless they listed their own
//! (`pairs`), is routed on /v1/* as `my/<pair id>` (and by the plain pair id when no team pair has it), runs Work through the
//! same per-run relay, and is a fallback choice. It carries no hub budget (they pay): its answers add to the `mine` counter
//! of the same month file and never to the team-key spend. A request OpenRouter refuses with 401 / 403 says so in plain
//! words ([`REFUSED`]) and nothing falls back to the team key. Config `openrouter.personalKeys` (default true; an admin
//! switch, `POST /api/openrouter/personal`) forbids it: such providers are not offered, stored keys stay unused.
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
/// What a person gets when OpenRouter refused their own key (revoked, wrong, out of credit): never the team's key instead.
pub const REFUSED: &str = "Your own OpenRouter key was refused by OpenRouter (revoked, or out of credit?). Nothing was sent with the team's key instead: replace your key in Settings → OpenRouter budget, or remove it to use the team's models.";
/// What creating a personal key says when an admin switched them off.
pub const PERSONAL_OFF: &str = "Personal OpenRouter keys are switched off on this hub: an admin can allow them in Settings → OpenRouter budget.";
const PERSONAL: &str = "or-me:";
const MAX_PAIRS: usize = 30;

/// Where OpenRouter is: `https://openrouter.ai/api/v1`, or config `openrouter.baseUrl` (the regression runner's fake).
pub fn base_url(hub: &dyn AiHub) -> String {
    hub.cfg().get("openrouter").and_then(|o| o["baseUrl"].as_str()).map(|b| b.trim().trim_end_matches('/').to_string()).filter(|b| b.starts_with("http://") || b.starts_with("https://")).unwrap_or_else(|| BASE.into())
}
/// The provider kinds a Work run reaches through Jcode PGM on this hub: the ones the hub calls itself over chat completions.
pub fn direct_kind(kind: &str) -> bool { kind == KIND }
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

// ---------- a person's own key: <root>/profiles/<user>/openrouter-key.json ----------
/// The id of a person's own OpenRouter provider.
pub fn personal_id(u: &str) -> String { format!("{PERSONAL}{u}") }
/// Is this provider a person's own (no hub budget, their key)?
pub fn is_personal(p: &Value) -> bool { p["personal"] == json!(true) }
/// Does config allow personal keys (`openrouter.personalKeys`, default true)? Read fresh: an admin's switch needs no restart.
pub fn personal_on(root: &Path) -> bool { pgm_core_internal::config::Config::load(root).get("openrouter").and_then(|o| o["personalKeys"].as_bool()).unwrap_or(true) }
/// The admin's switch: `openrouter.personalKeys` into the machine's own config.local.json (add-only; keeps its permissions).
fn set_personal_on(root: &Path, on: bool) -> Result<(), String> {
    let f = root.join("config.local.json");
    let mut c = read_json(&f).filter(Value::is_object).unwrap_or_else(|| json!({}));
    if !c["openrouter"].is_object() { c["openrouter"] = json!({}); }
    c["openrouter"]["personalKeys"] = json!(on);
    let mode = std::fs::metadata(&f).ok().map(|m| m.permissions());
    write_json(&f, &c);
    if let Some(m) = mode { let _ = std::fs::set_permissions(&f, m); }
    if personal_on(root) == on { Ok(()) } else { Err("Could not save the setting (config.local.json).".into()) }
}
fn key_file(root: &Path, u: &str) -> PathBuf { root.join("profiles").join(u).join("openrouter-key.json") }
fn key_rec(root: &Path, u: &str) -> Value { if !pgm_identity_internal::valid_username(u) { return json!({}); } read_json(&key_file(root, u)).filter(Value::is_object).unwrap_or_else(|| json!({})) }
/// A key as OpenRouter issues them: `sk-or-…`, letters, digits, - and _, a plausible length. No call is made.
pub fn key_ok(k: &str) -> bool { k.starts_with("sk-or-") && (20..=400).contains(&k.len()) && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') }
/// `…abcd`: the last four characters, all the page ever learns of a key.
pub fn last4(k: &str) -> String { let n: Vec<char> = k.chars().collect(); format!("…{}", n[n.len().saturating_sub(4)..].iter().collect::<String>()) }
/// The person's own provider: their key, and their own pairs else the team's (`team` = the team's shared OpenRouter
/// provider). None when they have no key, personal keys are off, or there are no pairs at all.
pub fn personal_provider(root: &Path, team: Option<&Value>, u: &str) -> Option<Value> {
    if u.is_empty() || !pgm_identity_internal::valid_username(u) || !key_file(root, u).is_file() { return None; }
    let rec = key_rec(root, u); let k = s(&rec, "apiKey");
    if k.is_empty() || !personal_on(root) { return None; }
    let own = pairs_of(&rec); let ps = if own.is_empty() { team.map(pairs_of).unwrap_or_default() } else { own };
    if ps.is_empty() { return None; }
    Some(json!({ "id": personal_id(u), "kind": KIND, "label": "my OpenRouter", "owner": u, "shared": false, "personal": true, "apiKey": k, "models": ps.iter().map(|x| x["id"].clone()).collect::<Vec<_>>(), "pairs": ps, "createdAt": rec["createdAt"] }))
}
/// What the pickers get of a personal provider: no key.
pub fn personal_public(p: &Value) -> Value { json!({ "id": p["id"], "kind": KIND, "label": p["label"], "owner": p["owner"], "shared": false, "personal": true, "models": p["models"], "hasKey": true, "createdAt": p["createdAt"], "baseUrl": BASE, "pairs": p["pairs"] }) }
/// A 401 / 403 from OpenRouter on a person's own key: the plain message (None for the team's provider and other statuses).
pub fn refused(p: &Value, status: u16) -> Option<String> { (is_personal(p) && (status == 401 || status == 403)).then(|| REFUSED.to_string()) }
/// The person's key settings as the page shows them (never the key).
pub fn mykey_view(hub: &dyn AiHub, u: &str) -> Value {
    let root = hub.root(); let rec = key_rec(root, u); let k = s(&rec, "apiKey");
    let team = hub.work().team_provider();
    json!({ "personalKeys": personal_on(root), "hasKey": !k.is_empty(), "last4": if k.is_empty() { Value::Null } else { json!(last4(&k)) }, "providerId": personal_id(u),
        "pairs": pairs_of(&rec), "teamPairs": team.map(|t| pairs_of(&t)).unwrap_or_default(), "offered": hub.work().provider_get(hub.ai(), &personal_id(u), u).is_some() })
}
fn set_mykey(hub: &dyn AiHub, u: &str, b: &Value) -> Result<(), (u16, String)> {
    let root = hub.root();
    if !personal_on(root) { return Err((403, PERSONAL_OFF.into())); }
    let mut rec = key_rec(root, u);
    if let Some(k) = b.get("apiKey").filter(|v| !v.is_null()) {
        let k = k.as_str().unwrap_or("").trim().to_string();
        if !key_ok(&k) { return Err((400, "That does not look like an OpenRouter key (sk-or-…, letters, digits, - and _).".into())); }
        rec["apiKey"] = json!(k); rec["createdAt"] = json!(pgm_core_internal::time::iso_now());
    }
    if s(&rec, "apiKey").is_empty() { return Err((400, "Paste your OpenRouter key (sk-or-…) first.".into())); }
    if let Some(p) = b.get("pairs") {
        if p.is_null() || p.as_array().map(|a| a.is_empty()).unwrap_or(false) { if let Some(o) = rec.as_object_mut() { o.remove("pairs"); } }
        else { rec["pairs"] = json!(clean_pairs(p).map_err(|e| (400, e))?); }
    }
    if pairs_of(&rec).is_empty() && hub.work().team_provider().is_none() { return Err((400, "The team has no OpenRouter models to share: list your own pairs (id, model, providers) too.".into())); }
    if let Some(d) = key_file(root, u).parent() { let _ = std::fs::create_dir_all(d); }
    write_json(&key_file(root, u), &rec);
    Ok(())
}
fn remove_mykey(hub: &dyn AiHub, u: &str) { let _ = std::fs::remove_file(key_file(hub.root(), u)); }
/// GET OpenRouter's key endpoint (through the configurable base) with the person's key: the optional "test" button.
pub async fn test_mykey(hub: &Arc<dyn AiHub>, u: &str) -> Resp {
    let k = s(&key_rec(hub.root(), u), "apiKey");
    if k.is_empty() { return answer(400, json!({ "error": "No key of yours is stored." })); }
    if !personal_on(hub.root()) { return answer(403, json!({ "error": PERSONAL_OFF })); }
    let r = crate::openai::self_http(&**hub).get(format!("{}/key", base_url(&**hub))).header("authorization", format!("Bearer {k}")).header("HTTP-Referer", hub.public_url()).header("X-Title", "VibeRoller").send().await;
    match r {
        Err(e) => answer(502, json!({ "ok": false, "error": format!("OpenRouter could not be reached: {}", e.to_string().replace(&k, "…")) })),
        Ok(x) if matches!(x.status().as_u16(), 401 | 403) => answer(200, json!({ "ok": false, "error": REFUSED })),
        Ok(x) if !x.status().is_success() => answer(502, json!({ "ok": false, "error": format!("OpenRouter answered {}.", x.status().as_u16()) })),
        Ok(x) => { let j: Value = x.json().await.unwrap_or(Value::Null); let d = &j["data"]; answer(200, json!({ "ok": true, "label": d["label"].as_str().map(|l| l.chars().take(60).collect::<String>().replace(&k, "…")), "usage": d["usage"], "limit": d["limit"] })) }
    }
}
/// The plain refusal when the person is at or over the team cap, for a provider the team pays for; None for a person's own.
pub fn guard(hub: &dyn AiHub, u: &str, p: &Value) -> Option<String> { if is_personal(p) { None } else { refusal(hub, u) } }
/// An answer's cost: the team key's counts against the month budget, a person's own in their `mine` counter.
pub fn note_for(hub: &dyn AiHub, u: &str, personal: bool, usage: &Value) { if personal { note_mine(hub, u, usage) } else { note_cost(hub, u, usage) } }
fn cost_of(usage: &Value) -> f64 { usage["cost"].as_f64().or_else(|| usage["cost"].as_str().and_then(|t| t.trim().parse().ok())).filter(|c: &f64| c.is_finite() && *c > 0.0).unwrap_or(0.0) }
/// Adds one answer of the person's own key to their month's `mine` counter (information only: no cap applies).
pub fn note_mine(hub: &dyn AiHub, u: &str, usage: &Value) {
    if !pgm_identity_internal::valid_username(u) || !usage.is_object() { return; }
    let cost = cost_of(usage); let m = month();
    edit(hub, u, |r| { let e = &mut r["months"][m.as_str()]["mine"]; e["usd"] = json!(usd(e["usd"].as_f64().unwrap_or(0.0) + cost)); e["calls"] = json!(e["calls"].as_u64().unwrap_or(0) + 1); });
}
/// (this month's USD on the person's own key, answers)
pub fn mine_of(hub: &dyn AiHub, u: &str) -> (f64, u64) { let r = rec(hub, u); let m = &r["months"][month()]["mine"]; (m["usd"].as_f64().unwrap_or(0.0), m["calls"].as_u64().unwrap_or(0)) }

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
    let (spent, calls) = spent_of(hub, u); let (cap, own) = cap_of(hub, u); let (mine, mine_calls) = mine_of(hub, u);
    json!({ "mineUsd": usd(mine), "mineCalls": mine_calls, "personalKeys": personal_on(hub.root()), "hasOwnKey": key_file(hub.root(), u).is_file() && !s(&key_rec(hub.root(), u), "apiKey").is_empty(), "month": month(), "spentUsd": usd(spent), "capUsd": cap, "ownCap": own, "defaultCapUsd": default_cap(hub), "calls": calls, "leftUsd": usd((cap - spent).max(0.0)), "over": spent >= cap, "resetsAt": format!("{}T00:00:00Z", resets_on()), "fallback": fallback_of(hub, u) })
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
    if (jcode_cmd(hub).is_err() && hub.pgm_server().is_none()) || guard(hub, u, &p).is_some() { return None; }
    Some((p, s(&pr, "id"), format!("{why}, so this run uses OpenRouter ({}) through Jcode PGM on the server, as your setting \"use OpenRouter when Claude is unavailable\" says.", s(&pr, "label"))))
}

// ---------- Jcode PGM on this server (Work) ----------
/// The Jcode PGM this server runs Work with: config `jcode.command` (a path, or a name on PATH) and `jcode.args`. Err =
/// why this path is off (empty, or not there).
pub fn jcode_cmd(hub: &dyn AiHub) -> Result<(String, Vec<String>), String> {
    // read fresh: an admin who sets jcode.command in the hub's config needs no restart (config.json, config.local.json over it)
    let j = pgm_core_internal::config::Config::load(hub.root()).get("jcode").cloned().unwrap_or(Value::Null);
    let c = s(&j, "command").trim().to_string();
    if c.is_empty() { return Err(NO_JCODE.into()); }
    let found = if c.contains('/') || c.contains('\\') { Path::new(&c).is_file() } else { std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(&c).is_file() || d.join(format!("{c}.exe")).is_file())).unwrap_or(false) };
    if !found { return Err(format!("OpenRouter models run Work through Jcode PGM on the server, and the jcodepgm this hub is set up with ({c}) is not there: an admin installs it or fixes jcode.command. Pick a Claude in the dropdown meanwhile.")); }
    Ok((c, list_of(&j["args"])))
}
/// Can this provider run Work here? Some(reason) when it cannot (a direct kind with no Jcode PGM). None = it can, or it is
/// not a direct kind.
/// With a Jcode PGM Server next to the hub (T-376 stage 3) the server runs it: on.
pub fn work_off(hub: &dyn AiHub, p: &Value) -> Option<String> { if direct_kind(p["kind"].as_str().unwrap_or("")) && hub.pgm_server().is_none() { jcode_cmd(hub).err() } else { None } }
/// How a Work run of a direct-kind provider goes: the jcode harness's provider entry (jcode.rs `command`: the binary, its
/// args, `-p openrouter`, the model), the model jcode runs, the model id the relay routes to (a pair id for OpenRouter),
/// the environment (no key in it: the relay's address is added once the run has its token, [`relay_env`]).
pub struct WorkPlan { pub prov: Value, pub model: String, pub routed: String, pub env: Vec<(String, String)>, pub label: String }
pub fn work_plan(hub: &dyn AiHub, u: &str, p: &Value, state_model: &str) -> Result<WorkPlan, String> {
    let (cmd, args) = jcode_cmd(hub)?;
    if let Some(m) = guard(hub, u, p) { return Err(m); }
    if s(p, "apiKey").is_empty() { return Err("This OpenRouter provider has no key set; an admin adds it in Settings → My AIs.".into()); }
    let ps = pairs_of(p);
    let Some(pr) = ps.iter().find(|x| s(x, "id") == state_model).or(ps.first()).cloned() else { return Err("This OpenRouter provider offers no models.".into()) };
    let (routed, model, label, order, fallbacks) = (s(&pr, "id"), s(&pr, "model"), format!("{} · {}", if is_personal(p) { "my OpenRouter" } else { "OpenRouter" }, s(&pr, "label")), list_of(&pr["providers"]), pr["allowFallbacks"] == json!(true));
    let mut env: Vec<(String, String)> = vec![
        // jcode needs a key to start its OpenRouter provider; this is not one: the run's relay adds the hub's own, so the key
        // never enters the run's command line or environment
        ("OPENROUTER_API_KEY".into(), "viberoller-relay".into()),
        ("JCODE_OPENROUTER_MODEL_CATALOG".into(), "0".into()),
        ("JCODE_OPENROUTER_MODEL".into(), model.clone()),
        // the routing as jcode names it (the relay applies the same on every call whatever jcode asks for)
        ("JCODE_OPENROUTER_ALLOW_FALLBACKS".into(), fallbacks.to_string()),
        // one person's cached answer must never reach another's session: pgm-proxy's shared pool stays off for hub runs
        ("JCODE_PGM_PROXY_SHARED".into(), "off".into()),
    ];
    if !order.is_empty() { env.push(("JCODE_OPENROUTER_PROVIDER_ORDER".into(), order.join(","))); }
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

// ---------- the Jcode PGM Server (T-376, team note plans/jcode-pgm-server.md B.1) ----------
// The server next to the hub serves these same pairs to jcode clients: it asks the hub (/__pgm/*) what a person may use,
// the key for one request, and charges each answer here, so keys, pairs and budgets have one source of truth.
/// The OpenRouter providers a person may pick (the Work dropdown's: the team's / shared ones, then their own key's), as
/// full records (the key is in them: never hand one on but through [`pgm_route`]).
fn usable(hub: &dyn AiHub, u: &str) -> Vec<Value> {
    let w = hub.work();
    let mut out: Vec<Value> = crate::work::providers_list(hub, u).into_iter().filter(|p| p["kind"] == json!(KIND)).filter_map(|p| w.provider_get(hub.ai(), &s(&p, "id"), u)).collect();
    out.sort_by_key(is_personal);
    out
}
/// The pair a Work session of this provider runs (the person's pick, else the provider's first) as the server's clients name
/// it (`my/<id>` for their own key's), and its label; Err = the plain reason it cannot run (over the budget, no key, no pairs).
pub fn pgm_work_pair(hub: &dyn AiHub, u: &str, p: &Value, state_model: &str) -> Result<(String, String), String> {
    if let Some(m) = guard(hub, u, p) { return Err(m); }
    if s(p, "apiKey").is_empty() { return Err("This OpenRouter provider has no key set; an admin adds it in Settings → My AIs.".into()); }
    let ps = pairs_of(p);
    let Some(pr) = ps.iter().find(|x| s(x, "id") == state_model).or(ps.first()).cloned() else { return Err("This OpenRouter provider offers no models.".into()) };
    Ok((pgm_id(p, &pr), format!("{} · {}", if is_personal(p) { "my OpenRouter" } else { "OpenRouter" }, s(&pr, "label"))))
}
/// The pair ids as the server's clients pick them: a team pair by its id, a pair of the person's own key as `my/<id>` (as on /v1).
fn pgm_id(p: &Value, pr: &Value) -> String { if is_personal(p) { format!("my/{}", s(pr, "id")) } else { s(pr, "id") } }
/// The pairs a person may use (team and their own key's): `{ id, label, model, providers, allowFallbacks, source, provider }`.
pub fn pgm_pairs(hub: &dyn AiHub, u: &str) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    for p in usable(hub, u) { for pr in pairs_of(&p) {
        let id = pgm_id(&p, &pr);
        if out.iter().any(|x| x["id"] == json!(id)) { continue; }
        out.push(json!({ "id": id, "label": pr["label"], "model": pr["model"], "providers": pr["providers"], "allowFallbacks": pr["allowFallbacks"] == json!(true), "source": if is_personal(&p) { "mine" } else { "team" }, "provider": p["id"] }));
    } }
    out
}
/// The person's month as the server's clients see it.
pub fn pgm_budget(hub: &dyn AiHub, u: &str) -> Value { let m = month_view(hub, u); json!({ "capUsd": m["capUsd"], "spentUsd": m["spentUsd"], "resetsAt": m["resetsAt"] }) }
/// The provider and pair an id names for that person: `my/<id>` = their own key's; a plain id = the team's pair, else their
/// own pair with that id (as /v1 routes it).
pub fn pgm_resolve(hub: &dyn AiHub, u: &str, asked: &str) -> Option<(Value, Value)> {
    let all = usable(hub, u);
    let find = |own: bool, id: &str| all.iter().filter(|p| is_personal(p) == own).find_map(|p| pair(p, id).map(|pr| (p.clone(), pr)));
    match asked.strip_prefix("my/") { Some(id) => find(true, id).or_else(|| find(false, asked)), None => find(false, asked).or_else(|| find(true, asked)) }
}
/// The key and routing for exactly one request of that person on that pair: `{ baseUrl, apiKey, model, provider, kind,
/// pair, headers }` (`provider` exactly as the hub itself sends it to OpenRouter: [`shape`]). Err((403 | 503, the plain
/// reason)) when it is not a pair they may use, a team pair over their month budget, or a provider without a key.
pub fn pgm_route(hub: &dyn AiHub, u: &str, asked: &str) -> Result<Value, (u16, String)> {
    let Some((p, pr)) = pgm_resolve(hub, u, asked) else { return Err((403, format!("\"{}\" is not one of the OpenRouter models you may use (see the models list).", asked.chars().take(80).collect::<String>()))) };
    if let Some(m) = guard(hub, u, &p) { return Err((403, m)); }
    if s(&p, "apiKey").is_empty() { return Err((503, "This OpenRouter provider has no key set; an admin adds it in Settings → My AIs.".into())); }
    let mut b = json!({ "model": pr["id"] });
    shape(&p, &mut b);
    Ok(json!({ "baseUrl": base_url(hub), "apiKey": s(&p, "apiKey"), "model": b["model"], "provider": b["provider"], "kind": if is_personal(&p) { "mine" } else { "team" }, "pair": pgm_id(&p, &pr),
        "headers": { "HTTP-Referer": hub.public_url(), "X-Title": "VibeRoller" } }))
}
/// The provider an answer the server made is charged to: the pair's (when it still resolves), else a stand-in of the
/// right kind (a key removed between the request and its charge still counts).
pub fn pgm_charge_provider(hub: &dyn AiHub, u: &str, asked: &str, mine: bool) -> (Value, String) {
    let plain = asked.strip_prefix("my/").unwrap_or(asked).to_string();
    let want = if mine { format!("my/{plain}") } else { plain.clone() };
    match pgm_resolve(hub, u, &want).filter(|(p, _)| is_personal(p) == mine) {
        Some((p, pr)) => (p, s(&pr, "id")),
        None => (json!({ "id": if mine { personal_id(u) } else { "pgm-server".to_string() }, "kind": KIND, "personal": mine }), plain),
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
        // T-372: the person's own key. The key itself is never in an answer: hasKey and the last four characters.
        "/api/openrouter/mykey" if get => answer(200, mykey_view(&**hub, u)),
        "/api/openrouter/mykey" if post => match set_mykey(&**hub, u, &r.json()) {
            Ok(()) => { pgm_core_internal::audit::record(u, "openrouter.mykey.set", u, json!({}), ""); answer(200, mykey_view(&**hub, u)) }
            Err((st, e)) => answer(st, json!({ "error": e })),
        },
        "/api/openrouter/mykey/remove" if post => { remove_mykey(&**hub, u); pgm_core_internal::audit::record(u, "openrouter.mykey.remove", u, json!({}), ""); answer(200, mykey_view(&**hub, u)) }
        "/api/openrouter/mykey" if r.method == hyper::Method::DELETE => { remove_mykey(&**hub, u); pgm_core_internal::audit::record(u, "openrouter.mykey.remove", u, json!({}), ""); answer(200, mykey_view(&**hub, u)) }
        "/api/openrouter/personal" if post => {
            if !hub.is_admin(u) { return Some(answer(403, json!({ "error": "Admins only." }))); }
            let b = r.json();
            let Some(on) = b["on"].as_bool() else { return Some(answer(400, json!({ "error": "on is true or false." }))) };
            if let Err(e) = set_personal_on(hub.root(), on) { return Some(answer(500, json!({ "error": e }))); }
            pgm_core_internal::audit::record(u, "openrouter.personal", "", json!({ "on": on }), "");
            answer(200, json!({ "personalKeys": personal_on(hub.root()) }))
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
    fn a_personal_key_is_checked_shown_as_four_characters_and_its_refusal_is_plain() {
        assert!(key_ok("sk-or-v1-abcdef0123456789abcd") && key_ok("sk-or-v1-r606_mine-0123456789"));
        for bad in ["", "sk-ant-api03-abcdefghijklmnop", "sk-or-short", "sk-or-v1-has space-0123456789", "sk-or-v1-0123456789abcdef\n", "sk-or-v1-ünï-0123456789abcdef"] { assert!(!key_ok(bad), "{bad:?}"); }
        assert_eq!(last4("sk-or-v1-abcdef0123456789wxyz"), "…wxyz");
        let mine = json!({ "kind": "openrouter", "personal": true }); let team = json!({ "kind": "openrouter" });
        assert!(refused(&mine, 401).is_some() && refused(&mine, 403).is_some() && refused(&mine, 429).is_none() && refused(&mine, 200).is_none() && refused(&team, 401).is_none());
        assert!(refused(&mine, 401).unwrap().contains("Your own OpenRouter key was refused"));
        assert_eq!(personal_id("mills"), "or-me:mills"); assert!(is_personal(&mine) && !is_personal(&team));
    }
    #[test]
    fn a_personal_provider_has_the_team_pairs_unless_the_person_listed_their_own_and_hides_nothing_it_should_not() {
        let dir = std::env::temp_dir().join(format!("vr-or-me-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("profiles").join("ann")).unwrap();
        let team = json!({ "kind": "openrouter", "pairs": [{ "id": "or/sonnet", "model": "anthropic/claude-sonnet-4.5", "providers": [], "allowFallbacks": false }] });
        assert!(personal_provider(&dir, Some(&team), "ann").is_none(), "no key, no provider");
        std::fs::write(key_file(&dir, "ann"), r#"{"apiKey":"sk-or-v1-unit-0123456789abcdef"}"#).unwrap();
        let p = personal_provider(&dir, Some(&team), "ann").unwrap();
        assert_eq!(p["id"], "or-me:ann"); assert_eq!(p["models"], json!(["or/sonnet"])); assert_eq!(p["personal"], true);
        assert!(personal_provider(&dir, None, "ann").is_none(), "no pairs at all");
        std::fs::write(key_file(&dir, "ann"), r#"{"apiKey":"sk-or-v1-unit-0123456789abcdef","pairs":[{"id":"mine/q","model":"qwen/qwen3-coder","providers":[],"allowFallbacks":false}]}"#).unwrap();
        assert_eq!(personal_provider(&dir, Some(&team), "ann").unwrap()["models"], json!(["mine/q"]));
        assert!(!personal_public(&personal_provider(&dir, Some(&team), "ann").unwrap()).to_string().contains("unit-0123"));
        std::fs::write(dir.join("config.local.json"), r#"{"openrouter":{"personalKeys":false}}"#).unwrap();
        assert!(personal_provider(&dir, Some(&team), "ann").is_none(), "an admin forbade them");
        assert!(personal_provider(&dir, Some(&team), "../x").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn months_and_names() {
        assert_eq!(month_name("2026-11"), "November 2026"); assert_eq!(month_name("x"), "x");
        assert_eq!(month().len(), 7); assert!(resets_on().ends_with("-01")); assert!(resets_on() > month());
        assert!(direct_kind("openrouter") && !direct_kind("openai") && !direct_kind("host-openai") && !direct_kind("host-claude") && !direct_kind("host-codex") && !direct_kind("jcode"));
        assert_eq!(list_of(&json!("a, b,,c")), ["a", "b", "c"]);
    }
}
