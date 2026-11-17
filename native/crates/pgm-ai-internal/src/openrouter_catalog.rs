//! OpenRouter's model catalogue for AI Admin → Team OpenRouter (T-381, team note plans/openrouter-team-admin.md): the
//! models (`GET /models`), one model's providers (`GET /models/{author}/{slug}/endpoints`) with what they cost, how fast
//! and how reliable they are, and each one's data policy from the ZDR list (`GET /endpoints/zdr`).
//!
//! The types and the one-line detail are jcode's (`jcode-provider-openrouter`: `ModelInfo`, `ModelPricing`,
//! `EndpointInfo::detail_string`), ported here so the hub and, later, Jcode PGM use one copy (library law; pgm-ai is
//! mirrored to pgm-libs). Speed (`throughput_last_30m`, tokens per second) and latency (`latency_last_30m`, ms to the first
//! token) are only given to a request with a key, so the catalogue asks with the team's key when there is one.
//!
//! **Data policy** (OpenRouter's three tiers, docs guides/features/zdr.md): an endpoint in the ZDR list is **no retention**
//! (`zdr`), or **retains, no training** (`safety`) when the list marks it `safety_retention`; an endpoint not in the list
//! is **retains, may train** (`train`): OpenRouter itself takes that conservative stance when a policy is unclear.
//!
//! Cached in memory: the model list and the provider list 24 h, a model's endpoints and the ZDR list 1 h (OpenRouter's own
//! catalogue changes slowly; jcode uses the same times), the deals index 6 h.
//!
//! **Everything an endpoint says** (T-385): every price (per token, per item, tiered above a prompt size), the provider's
//! deal (`pricing.discount`: already taken off the listed prices, so the price before it is price / (1 − discount)), its
//! service tier (flex / priority / …), uptime over 5 min, 30 min and a day, its limits and parameters, and who runs it
//! (`GET /providers`: privacy policy, terms, status page, headquarters, datacenters).
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::hub::{answer, AiHub, Resp};
use pgm_core_internal::json::s;
use pgm_identity_internal::Account;
use pgm_net_internal::http::Req;
use std::sync::Arc;

const MODELS_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const ENDPOINTS_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_MODELS: usize = 400;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelPricing {
    #[serde(default, deserialize_with = "num_or_text")]
    pub prompt: Option<String>,
    #[serde(default, deserialize_with = "num_or_text")]
    pub completion: Option<String>,
    #[serde(default, deserialize_with = "num_or_text")]
    pub input_cache_read: Option<String>,
    #[serde(default, deserialize_with = "num_or_text")]
    pub input_cache_write: Option<String>,
    /// The rest of OpenRouter's price list (T-385): per token unless [`ModelPricing::EACH`] says otherwise.
    #[serde(default, deserialize_with = "num_or_text")]
    pub input_cache_write_1h: Option<String>,
    #[serde(default, deserialize_with = "num_or_text")]
    pub internal_reasoning: Option<String>,
    #[serde(default, deserialize_with = "num_or_text")]
    pub audio: Option<String>,
    #[serde(default, deserialize_with = "num_or_text")]
    pub input_audio_cache: Option<String>,
    #[serde(default, deserialize_with = "num_or_text")]
    pub image_token: Option<String>,
    #[serde(default, deserialize_with = "num_or_text")]
    pub image_output: Option<String>,
    #[serde(default, deserialize_with = "num_or_text")]
    pub image: Option<String>,
    #[serde(default, deserialize_with = "num_or_text")]
    pub web_search: Option<String>,
    #[serde(default, deserialize_with = "num_or_text")]
    pub request: Option<String>,
    /// The provider's deal: a share off (0.41 = 41 % off), already taken off the prices listed here.
    #[serde(default, deserialize_with = "num_or_text")]
    pub discount: Option<String>,
    /// Tiered prices: from `min_prompt_tokens` on, these prices instead (e.g. above 200k tokens of prompt).
    #[serde(default)]
    pub overrides: Option<Vec<Value>>,
}
/// OpenRouter sends prices as text ("0.00000025"); some catalogues send numbers.
fn num_or_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? { Some(Value::String(t)) => Some(t), Some(Value::Number(n)) => Some(n.to_string()), _ => None })
}
fn round4(x: f64) -> f64 { (x * 10_000.0).round() / 10_000.0 }
impl ModelPricing {
    /// USD per million tokens (OpenRouter's prices are per token); None when absent or negative (`-1` = varies).
    fn per_m(v: &Option<String>) -> Option<f64> { Self::usd(v).map(|x| round4(x * 1e6)) }
    /// A price as it is (USD); None when absent or negative.
    fn usd(v: &Option<String>) -> Option<f64> { v.as_deref().and_then(|t| t.trim().parse::<f64>().ok()).filter(|x| x.is_finite() && *x >= 0.0) }
    pub fn in_per_m(&self) -> Option<f64> { Self::per_m(&self.prompt) }
    pub fn out_per_m(&self) -> Option<f64> { Self::per_m(&self.completion) }
    pub fn cache_read_per_m(&self) -> Option<f64> { Self::per_m(&self.input_cache_read) }
    pub fn cache_write_per_m(&self) -> Option<f64> { Self::per_m(&self.input_cache_write) }
    /// The deal, as a share off (0 < d < 1); None without one.
    pub fn discount(&self) -> Option<f64> { Self::usd(&self.discount).filter(|d| *d > 0.0 && *d < 1.0) }
    /// What a price was before the deal (the listed price already has it taken off): price / (1 − discount).
    pub fn before_deal(&self, p: Option<f64>) -> Option<f64> { Some(round4(p? / (1.0 - self.discount()?))) }
    /// Free: both input and output cost nothing.
    pub fn free(&self) -> bool { self.in_per_m() == Some(0.0) && self.out_per_m() == Some(0.0) }
    /// Every other price OpenRouter lists, by the page's names: per million tokens (…PerM), or per item (…Each: an input
    /// image, a web search, a request); only those given, a free one (0) too, as OpenRouter lists it.
    pub fn more(&self) -> Value {
        let mut m = serde_json::Map::new();
        for (v, k) in [(&self.input_cache_write_1h, "cacheWrite1hPerM"), (&self.internal_reasoning, "reasoningPerM"), (&self.audio, "audioPerM"), (&self.input_audio_cache, "audioCachePerM"), (&self.image_token, "imageTokenPerM"), (&self.image_output, "imageOutputPerM")] {
            if let Some(p) = Self::per_m(v) { m.insert(k.into(), json!(p)); }
        }
        for (v, k) in [(&self.image, "imageEach"), (&self.web_search, "webSearchEach"), (&self.request, "requestEach")] {
            if let Some(p) = Self::usd(v) { m.insert(k.into(), json!((p * 1e6).round() / 1e6)); }
        }
        Value::Object(m)
    }
    /// The tiered prices, these per-million prices instead of the listed ones: from `minPromptTokens` on (dearer above
    /// 200k tokens of prompt, say), or between `fromUtc` and `toUtc` ("16:00" to "00:00": an off-peak price; OpenRouter
    /// sends `utc_start` / `utc_end` as hhmm). Smallest threshold first, then the earliest time; one of neither is skipped.
    pub fn tiers(&self) -> Vec<Value> {
        let hhmm = |v: &Value| v.as_u64().filter(|x| *x <= 2400 && x % 100 < 60).map(|x| format!("{:02}:{:02}", x / 100 % 24, x % 100));
        let mut t: Vec<((u64, u64), Value)> = self.overrides.iter().flatten().filter_map(|o| {
            let p: ModelPricing = serde_json::from_value(o.clone()).ok()?;
            let mut v = json!({ "inPerM": p.in_per_m(), "outPerM": p.out_per_m(), "cacheReadPerM": p.cache_read_per_m(), "cacheWritePerM": p.cache_write_per_m() });
            let key = if let Some(min) = o["min_prompt_tokens"].as_u64() { v["minPromptTokens"] = json!(min); (min, 0) }
                else { let (a, b) = (hhmm(&o["utc_start"])?, hhmm(&o["utc_end"])?); v["fromUtc"] = json!(a); v["toUtc"] = json!(b); (0, o["utc_start"].as_u64().unwrap_or(0)) };
            Some((key, v))
        }).collect();
        t.sort_by_key(|x| x.0);
        t.into_iter().map(|x| x.1).collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub pricing: ModelPricing,
    #[serde(default)]
    pub created: Option<u64>,
    #[serde(default)]
    pub architecture: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EndpointInfo {
    #[serde(default)]
    pub provider_name: String,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub pricing: ModelPricing,
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub max_completion_tokens: Option<u64>,
    #[serde(default)]
    pub quantization: Option<String>,
    #[serde(default)]
    pub uptime_last_30m: Option<f64>,
    #[serde(default)]
    pub latency_last_30m: Option<Value>,
    #[serde(default)]
    pub throughput_last_30m: Option<Value>,
    #[serde(default)]
    pub supports_implicit_caching: Option<bool>,
    #[serde(default)]
    pub status: Option<i64>,
    // T-385: the rest of what OpenRouter says about an endpoint
    /// flex (cheaper, may wait), priority (dearer, first in line), ultrafast …; None = the provider's standard.
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub uptime_last_5m: Option<f64>,
    #[serde(default)]
    pub uptime_last_1d: Option<f64>,
    #[serde(default)]
    pub max_prompt_tokens: Option<u64>,
    #[serde(default)]
    pub supported_parameters: Option<Vec<Value>>,
    #[serde(default)]
    pub supports_tool_choice: Option<Value>,
}
impl EndpointInfo {
    /// The median of a percentile set ({p50, …}) or a plain number; None for 0 / missing (OpenRouter sends null without a key).
    fn p50(v: &Option<Value>) -> Option<f64> {
        match v.as_ref()? { Value::Number(n) => n.as_f64(), Value::Object(m) => m.get("p50").and_then(Value::as_f64), _ => None }.filter(|x| x.is_finite() && *x > 0.0)
    }
    pub fn tps_p50(&self) -> Option<f64> { Self::p50(&self.throughput_last_30m) }
    pub fn latency_p50_ms(&self) -> Option<f64> { Self::p50(&self.latency_last_30m) }
    /// What to name in `provider.only` to route to exactly this endpoint: its tag ("deepinfra/fp4"), else the provider.
    pub fn route_name(&self) -> String { self.tag.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| self.provider_name.clone()) }
    /// jcode's one-line detail ("in $0.25/M, out $0.95/M, 100%, 230ms p50, 45tps, cache on, fp4").
    pub fn detail_string(&self) -> String {
        let mut parts = vec![];
        if let Some(p) = self.pricing.in_per_m() { parts.push(format!("in ${p:.2}/M")); }
        if let Some(p) = self.pricing.out_per_m() { parts.push(format!("out ${p:.2}/M")); }
        if let Some(d) = self.pricing.discount() { parts.push(format!("{:.0}% off", d * 100.0)); }
        if let Some(t) = self.service_tier.as_deref().filter(|t| !t.is_empty()) { parts.push(t.to_string()); }
        if let Some(p) = ModelPricing::per_m(&self.pricing.input_cache_write).filter(|x| *x > 0.0) { parts.push(format!("cache write ${p:.2}/M")); }
        if let Some(p) = self.pricing.cache_read_per_m().filter(|x| *x > 0.0) { parts.push(format!("cache read ${p:.2}/M")); }
        if let Some(u) = self.uptime_last_30m { parts.push(format!("{u:.0}%")); }
        if let Some(l) = self.latency_p50_ms() { parts.push(format!("{l:.0}ms p50")); }
        if let Some(t) = self.tps_p50() { parts.push(format!("{t:.0}tps")); }
        if let Some(c) = self.supports_implicit_caching { parts.push(if c { "cache on" } else { "cache off" }.into()); }
        if let Some(q) = self.quantization.as_deref().filter(|q| *q != "unknown" && !q.is_empty()) { parts.push(q.to_string()); }
        parts.join(", ")
    }
}

/// An endpoint's data policy tier and its words: no retention (ZDR), retains without training (safety retention), retains
/// and may train (not in the ZDR list).
pub fn data_tier(zdr: &Value, model: &str, ep: &EndpointInfo) -> (&'static str, &'static str) {
    let same = |z: &Value| match ep.tag.as_deref().filter(|t| !t.is_empty()) { Some(t) => s(z, "tag") == t, None => s(z, "provider_name") == ep.provider_name };
    let hit = zdr.as_array().into_iter().flatten().find(|z| s(z, "model_id") == model && same(z));
    match hit { Some(z) if z["safety_retention"] == json!(true) => ("safety", "Retains, no training"), Some(_) => ("zdr", "No retention"), None => ("train", "Retains, may train") }
}

/// Who runs an endpoint, from OpenRouter's provider list (`GET /providers`): matched on the endpoint's tag ("deepinfra/fp4"
/// → slug "deepinfra"), else on the provider's name; their privacy policy, terms and status page, headquarters and
/// datacenters (countries). Null when the list does not have them.
pub fn provider_info(providers: &Value, ep: &EndpointInfo) -> Value {
    let slug = ep.tag.as_deref().and_then(|t| t.split('/').next()).unwrap_or_default();
    let list = providers.as_array().into_iter().flatten();
    let hit = list.clone().find(|p| !slug.is_empty() && s(p, "slug") == slug).or_else(|| list.clone().find(|p| s(p, "name").eq_ignore_ascii_case(&ep.provider_name)));
    match hit {
        Some(p) => json!({ "slug": s(p, "slug"), "privacyUrl": p["privacy_policy_url"], "termsUrl": p["terms_of_service_url"], "statusUrl": p["status_page_url"], "headquarters": p["headquarters"], "datacenters": p["datacenters"] }),
        None => Value::Null,
    }
}

/// The search's filters beyond its words: only models with a deal (from the deals index), only free ones.
#[derive(Default, Clone, Copy)]
pub struct ModelFilter { pub deals: bool, pub free: bool }

/// The models as the page lists them, those matching every word of `q` (id or name, any case) and the filter, newest
/// first; at most [`MAX_MODELS`]. `deals` is the deals index (model id → its biggest deal) when it has been built.
pub fn models_view(raw: &Value, q: &str, deals: Option<&HashMap<String, f64>>, f: ModelFilter) -> Value {
    let models: Vec<ModelInfo> = raw["data"].as_array().into_iter().flatten().filter_map(|m| serde_json::from_value(m.clone()).ok()).collect();
    let words: Vec<String> = q.to_lowercase().split_whitespace().map(String::from).collect();
    let deal = |m: &ModelInfo| deals.and_then(|d| d.get(&m.id)).copied(); // a variant (…:free) is its own model: no deal of the paid one
    let mut hit: Vec<&ModelInfo> = models.iter().filter(|m| { let hay = format!("{} {}", m.id, m.name).to_lowercase(); words.iter().all(|w| hay.contains(w.as_str())) })
        .filter(|m| !f.deals || deal(m).is_some()).filter(|m| !f.free || m.pricing.free()).collect();
    hit.sort_by(|a, b| b.created.unwrap_or(0).cmp(&a.created.unwrap_or(0)));
    let total = hit.len();
    let list: Vec<Value> = hit.into_iter().take(MAX_MODELS).map(|m| json!({
        "id": m.id, "name": if m.name.is_empty() { m.id.clone() } else { m.name.clone() }, "context": m.context_length,
        "inPerM": m.pricing.in_per_m(), "outPerM": m.pricing.out_per_m(), "cacheReadPerM": m.pricing.cache_read_per_m(), "cacheWritePerM": m.pricing.cache_write_per_m(),
        "more": m.pricing.more(), "tiers": m.pricing.tiers(), "free": m.pricing.free(), "bestDeal": deal(m),
        "input": m.architecture.as_ref().and_then(|a| a["input_modalities"].as_array().cloned()).unwrap_or_default(),
    })).collect();
    json!({ "models": list, "total": total })
}

/// One model's providers as the page lists them, fastest first (then the cheapest), each with everything OpenRouter says
/// about it: prices (all of them, the deal and what it was before, tiered prices), speed, first token, uptime (5 min, 30 min,
/// a day), limits, parameters, its data policy and who runs it.
pub fn endpoints_view(raw: &Value, zdr: &Value, providers: &Value) -> Value {
    let d = &raw["data"];
    let model = s(d, "id");
    let mut eps: Vec<EndpointInfo> = d["endpoints"].as_array().into_iter().flatten().filter_map(|e| serde_json::from_value(e.clone()).ok()).collect();
    eps.sort_by(|a, b| b.tps_p50().unwrap_or(0.0).total_cmp(&a.tps_p50().unwrap_or(0.0)).then(a.pricing.in_per_m().unwrap_or(f64::MAX).total_cmp(&b.pricing.in_per_m().unwrap_or(f64::MAX))));
    let list: Vec<Value> = eps.iter().map(|e| {
        let (tier, words) = data_tier(zdr, &model, e);
        let pr = &e.pricing;
        json!({
            "provider": e.provider_name, "route": e.route_name(), "tag": e.tag, "quantization": e.quantization.clone().filter(|q| q != "unknown"),
            "inPerM": pr.in_per_m(), "outPerM": pr.out_per_m(), "cacheReadPerM": pr.cache_read_per_m(), "cacheWritePerM": pr.cache_write_per_m(),
            "discount": pr.discount(), "inPerMBefore": pr.before_deal(pr.in_per_m()), "outPerMBefore": pr.before_deal(pr.out_per_m()),
            "free": pr.free(), "more": pr.more(), "tiers": pr.tiers(), "serviceTier": e.service_tier.clone().filter(|t| !t.is_empty()),
            "context": e.context_length, "maxOut": e.max_completion_tokens, "maxPrompt": e.max_prompt_tokens,
            "uptime30m": e.uptime_last_30m, "uptime5m": e.uptime_last_5m, "uptime1d": e.uptime_last_1d,
            "latencyP50Ms": e.latency_p50_ms().map(|x| x.round()), "tpsP50": e.tps_p50().map(|x| (x * 10.0).round() / 10.0),
            "latency": e.latency_last_30m, "throughput": e.throughput_last_30m, "status": e.status,
            "implicitCaching": e.supports_implicit_caching, "params": e.supported_parameters.clone().unwrap_or_default(), "toolChoice": e.supports_tool_choice,
            "data": tier, "dataLabel": words, "providerInfo": provider_info(providers, e), "detail": e.detail_string(),
        })
    }).collect();
    json!({ "model": { "id": model, "name": s(d, "name") }, "endpoints": list })
}

// ---------- fetching, with the team's key when there is one ----------
struct Cached { at: Instant, v: Value }
static CACHE: LazyLock<Mutex<HashMap<String, Cached>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
fn cached(key: &str, ttl: Duration) -> Option<Value> { CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(key).filter(|c| c.at.elapsed() < ttl).map(|c| c.v.clone()) }
fn keep(key: &str, v: &Value) { CACHE.lock().unwrap_or_else(|e| e.into_inner()).insert(key.into(), Cached { at: Instant::now(), v: v.clone() }); }

/// GET `path` under OpenRouter's API base, with the team key when the hub has one; the JSON answer, or the plain reason.
async fn get(hub: &dyn AiHub, path: &str) -> Result<Value, String> {
    let mut rq = crate::openai::self_http(hub).get(format!("{}{path}", crate::openrouter::base_url(hub))).header("HTTP-Referer", hub.public_url()).header("X-Title", "VibeRoller");
    let key = hub.work().team_key_provider().map(|p| s(&p, "apiKey")).unwrap_or_default();
    if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
    let r = tokio::time::timeout(Duration::from_secs(25), rq.send()).await.map_err(|_| "OpenRouter did not answer in time.".to_string())?.map_err(|e| format!("OpenRouter could not be reached: {}", e.to_string().replace(&key, "…")))?;
    let st = r.status().as_u16();
    if !(200..300).contains(&st) { return Err(if st == 404 { "OpenRouter does not know that model.".into() } else { format!("OpenRouter answered {st}.") }); }
    r.json::<Value>().await.map_err(|_| "OpenRouter answered in an unknown shape.".to_string())
}
/// A model id as OpenRouter's path wants it (author/slug, nothing else).
pub fn model_ok(id: &str) -> bool { let mut p = id.split('/'); let (a, b, c) = (p.next(), p.next(), p.next()); c.is_none() && [a, b].iter().all(|x| x.is_some_and(|t| !t.is_empty() && !t.chars().all(|ch| ch == '.') && t.len() <= 100 && t.chars().all(|ch| ch.is_ascii_alphanumeric() || "-._:".contains(ch)))) }

async fn models_raw(hub: &dyn AiHub) -> Result<Value, String> {
    match cached("models", MODELS_TTL) { Some(v) => Ok(v), None => { let v = get(hub, "/models").await?; keep("models", &v); Ok(v) } }
}
/// The model list, searched and filtered, with the deals index's state: `deals` = { ready, building, at (ms), models (how
/// many have a deal) }. Asking with `deals` starts the index when it is missing or old; the list says so until it is built.
pub async fn models(hub: &Arc<dyn AiHub>, q: &str, f: ModelFilter) -> Result<Value, String> {
    let raw = models_raw(&**hub).await?;
    let (map, at, _) = deals_state();
    if map.is_none() || at.is_some_and(|t| t.elapsed() >= DEALS_TTL) { start_deals(hub.clone()); }
    let (map, at, building) = deals_state();
    let mut v = models_view(&raw, q, map.as_ref(), f);
    v["deals"] = json!({ "ready": map.is_some(), "building": building, "ageMin": at.map(|t| t.elapsed().as_secs() / 60), "models": map.as_ref().map(|m| m.len()) });
    Ok(v)
}
pub async fn zdr(hub: &dyn AiHub) -> Value {
    if let Some(v) = cached("zdr", ENDPOINTS_TTL) { return v; }
    match get(hub, "/endpoints/zdr").await { Ok(v) => { let d = v["data"].clone(); keep("zdr", &d); d } Err(_) => Value::Null } // without it every endpoint shows as "retains, may train"
}
/// OpenRouter's provider list (who they are, their policies, where they run); a day in the cache. Null when it cannot be had.
pub async fn providers(hub: &dyn AiHub) -> Value {
    if let Some(v) = cached("providers", MODELS_TTL) { return v; }
    match get(hub, "/providers").await { Ok(v) => { let d = v["data"].clone(); keep("providers", &d); d } Err(_) => Value::Null }
}
async fn endpoints_raw(hub: &dyn AiHub, model: &str) -> Result<Value, String> {
    let k = format!("ep:{model}");
    match cached(&k, ENDPOINTS_TTL) { Some(v) => Ok(v), None => { let v = get(hub, &format!("/models/{model}/endpoints")).await?; keep(&k, &v); Ok(v) } }
}
pub async fn endpoints(hub: &dyn AiHub, model: &str) -> Result<Value, String> {
    if !model_ok(model) { return Err("A model is its OpenRouter id, author/slug (e.g. deepseek/deepseek-chat-v3.1).".into()); }
    let raw = endpoints_raw(hub, model).await?;
    let (z, p) = tokio::join!(zdr(hub), providers(hub));
    Ok(endpoints_view(&raw, &z, &p))
}

// ---------- the deals index (T-385) ----------
// OpenRouter says a deal only on a model's endpoints, never in the model list; so "only models with a deal" needs every
// model's endpoints. The index asks for them in the background, a few at a time (DEALS_AT_ONCE), keeps each answer in the
// cache (the provider lists then open at once) and holds, per model, its biggest deal. It is rebuilt when older than
// DEALS_TTL and someone searches.
const DEALS_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const DEALS_AT_ONCE: usize = 4;
#[derive(Default)]
struct Deals { map: Option<HashMap<String, f64>>, at: Option<Instant>, building: bool }
static DEALS: LazyLock<Mutex<Deals>> = LazyLock::new(|| Mutex::new(Deals::default()));
fn deals_state() -> (Option<HashMap<String, f64>>, Option<Instant>, bool) { let d = DEALS.lock().unwrap_or_else(|e| e.into_inner()); (d.map.clone(), d.at, d.building) }
/// The biggest deal among a model's endpoints (its `/endpoints` answer); None without one.
pub fn best_deal(raw: &Value) -> Option<f64> {
    raw["data"]["endpoints"].as_array().into_iter().flatten().filter_map(|e| serde_json::from_value::<ModelPricing>(e["pricing"].clone()).ok()?.discount()).reduce(f64::max)
}
fn start_deals(hub: Arc<dyn AiHub>) {
    { let mut d = DEALS.lock().unwrap_or_else(|e| e.into_inner()); if d.building { return; } d.building = true; }
    tokio::spawn(async move {
        use futures_util::StreamExt;
        let built = async {
            let raw = models_raw(&*hub).await?;
            let mut ids: Vec<String> = raw["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(|i| i.split(':').next().unwrap_or(i).to_string())).filter(|i| model_ok(i)).collect();
            ids.sort(); ids.dedup();
            let found: Vec<(String, Option<f64>)> = futures_util::stream::iter(ids).map(|id| { let hub = hub.clone(); async move { let d = endpoints_raw(&*hub, &id).await.ok().as_ref().and_then(best_deal); (id, d) } }).buffer_unordered(DEALS_AT_ONCE).collect().await;
            Ok::<_, String>(found.into_iter().filter_map(|(id, d)| Some((id, d?))).collect::<HashMap<_, _>>())
        }.await;
        let mut d = DEALS.lock().unwrap_or_else(|e| e.into_inner());
        d.building = false;
        match built { Ok(m) => { d.map = Some(m); d.at = Some(Instant::now()); } Err(e) => eprintln!("[openrouter] the deals index could not be built: {e}") }
    });
}

/// `GET /api/openrouter/catalog/models?q=&deals=1&free=1` and `GET /api/openrouter/catalog/endpoints?model=<author/slug>`:
/// AI Admin's model and provider picker (everyone signed in since T-383: anyone may add a key of their own and pick its
/// entries; the catalogue is OpenRouter's public one).
pub async fn route(hub: &Arc<dyn AiHub>, r: &Req, _a: &Account) -> Option<Resp> {
    if r.method != hyper::Method::GET || !r.path.starts_with("/api/openrouter/catalog/") { return None; }
    let on = |k: &str| r.param(k).is_some_and(|v| v == "1" || v == "true");
    let got = match r.path.as_str() {
        "/api/openrouter/catalog/models" => models(hub, &r.param("q").unwrap_or_default().chars().take(100).collect::<String>(), ModelFilter { deals: on("deals"), free: on("free") }).await,
        "/api/openrouter/catalog/endpoints" => endpoints(&**hub, r.param("model").unwrap_or_default().trim()).await,
        _ => return Some(answer(404, json!({ "error": "Not found" }))),
    };
    Some(match got { Ok(v) => answer(200, v), Err(e) => answer(502, json!({ "error": e })) })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ep(provider: &str, tag: &str, inp: &str, out: &str, tps: Option<f64>) -> Value {
        json!({ "provider_name": provider, "tag": tag, "quantization": tag.split('/').nth(1), "pricing": { "prompt": inp, "completion": out, "input_cache_read": "0.00000013" },
            "context_length": 163840, "uptime_last_30m": 99.0, "throughput_last_30m": tps.map(|t| json!({ "p50": t, "p90": t * 1.5 })), "latency_last_30m": { "p50": 420.0 }, "status": 0 })
    }
    #[test]
    fn a_models_providers_show_price_per_million_speed_and_their_data_policy_fastest_first() {
        let raw = json!({ "data": { "id": "deepseek/deepseek-chat-v3.1", "name": "DeepSeek V3.1", "endpoints": [
            ep("SiliconFlow", "siliconflow/fp8", "0.00000027", "0.000001", Some(30.0)), ep("DeepInfra", "deepinfra/fp4", "0.00000025", "0.00000095", Some(80.0)), ep("Chutes", "chutes", "0.0000002", "0.0000008", None) ] } });
        let zdr = json!([{ "model_id": "deepseek/deepseek-chat-v3.1", "provider_name": "DeepInfra", "tag": "deepinfra/fp4", "safety_retention": false },
            { "model_id": "deepseek/deepseek-chat-v3.1", "provider_name": "SiliconFlow", "tag": "siliconflow/fp8", "safety_retention": true },
            { "model_id": "other/model", "provider_name": "Chutes", "tag": "chutes" }]);
        let v = endpoints_view(&raw, &zdr, &Value::Null); let e = v["endpoints"].as_array().unwrap();
        assert_eq!(v["model"], json!({ "id": "deepseek/deepseek-chat-v3.1", "name": "DeepSeek V3.1" }));
        assert_eq!(e.iter().map(|x| s(x, "provider")).collect::<Vec<_>>(), ["DeepInfra", "SiliconFlow", "Chutes"]);
        assert_eq!(e[0]["inPerM"], json!(0.25)); assert_eq!(e[0]["outPerM"], json!(0.95)); assert_eq!(e[0]["cacheReadPerM"], json!(0.13));
        assert_eq!(e[0]["tpsP50"], json!(80.0)); assert_eq!(e[0]["latencyP50Ms"], json!(420.0)); assert_eq!(e[0]["route"], "deepinfra/fp4"); assert_eq!(e[0]["quantization"], "fp4");
        assert_eq!((s(&e[0], "data"), s(&e[1], "data"), s(&e[2], "data")), ("zdr".into(), "safety".into(), "train".into())); // the ZDR list for another model does not count
        assert_eq!(s(&e[2], "dataLabel"), "Retains, may train"); assert!(e[2]["tpsP50"].is_null());
        assert_eq!(s(&e[0], "detail"), "in $0.25/M, out $0.95/M, cache read $0.13/M, 99%, 420ms p50, 80tps, fp4");
    }
    #[test]
    fn the_model_list_is_searched_on_every_word_newest_first() {
        let raw = json!({ "data": [{ "id": "deepseek/deepseek-chat-v3.1", "name": "DeepSeek: V3.1", "created": 2, "pricing": { "prompt": "0.0000002", "completion": "0.0000008" }, "context_length": 163840, "architecture": { "input_modalities": ["text"] } },
            { "id": "deepseek/deepseek-r1", "name": "DeepSeek: R1", "created": 1, "pricing": { "prompt": "-1", "completion": 0.000002 } },
            { "id": "qwen/qwen3-coder", "name": "Qwen3 Coder", "created": 3 }] });
        let v = models_view(&raw, "DeepSeek ", None, ModelFilter::default());
        assert_eq!(v["total"], 2); assert_eq!(v["models"][0]["id"], "deepseek/deepseek-chat-v3.1"); assert_eq!(v["models"][0]["inPerM"], json!(0.2)); assert_eq!(v["models"][0]["input"], json!(["text"]));
        assert!(v["models"][1]["inPerM"].is_null(), "a varying price (-1) is no price"); assert_eq!(v["models"][1]["outPerM"], json!(2.0));
        assert_eq!(models_view(&raw, "deepseek r1", None, ModelFilter::default())["total"], 1); assert_eq!(models_view(&raw, "", None, ModelFilter::default())["models"][0]["id"], "qwen/qwen3-coder");
    }
    #[test]
    fn a_deal_shows_with_the_price_before_it_and_every_other_fact_an_endpoint_has() {
        // OpenRouter's own shape (2026-11-17): StreamLake 41 % off deepseek-v4-flash, the listed price already discounted
        let raw = json!({ "data": { "id": "deepseek/deepseek-v4-flash", "name": "DeepSeek V4 Flash", "endpoints": [
            { "provider_name": "StreamLake", "tag": "streamlake", "pricing": { "prompt": "0.0000000826", "completion": "0.0000001652", "discount": 0.41, "web_search": "0.01", "internal_reasoning": "0.0000001652",
                "overrides": [{ "min_prompt_tokens": 200000, "prompt": "0.0000001652", "completion": "0.0000003304" }, { "min_prompt_tokens": 128000, "prompt": "0.0000001", "completion": "0.0000002" }] },
              "uptime_last_5m": 100.0, "uptime_last_30m": 99.5, "uptime_last_1d": 98.25, "max_prompt_tokens": 120000, "supported_parameters": ["tools", "reasoning"], "supports_tool_choice": { "literal_none": true } },
            { "provider_name": "DeepInfra", "tag": "deepinfra/fp8", "service_tier": "flex", "pricing": { "prompt": "0.000000072", "completion": "0.000000144", "discount": 0, "input_cache_write": "0.0000001" } },
            { "provider_name": "Mancer 2", "tag": "mancer-2", "pricing": { "prompt": "0", "completion": "0" } },
            // Tencent's own shape: cheaper from 16:00 to midnight UTC (utc_start / utc_end as hhmm)
            { "provider_name": "Tencent", "tag": "tencent", "pricing": { "prompt": "0.000000132", "completion": "0.000000528", "overrides": [
                { "utc_start": 1600, "utc_end": 0, "prompt": "0.0000000825", "completion": "0.00000033" }, { "utc_start": 0, "utc_end": 1600, "prompt": "0.000000132", "completion": "0.000000528" }, { "utc_start": 2500, "utc_end": 0, "prompt": "0" } ] } } ] } });
        let providers = json!([{ "name": "StreamLake", "slug": "streamlake", "privacy_policy_url": "https://streamlake.example/privacy", "terms_of_service_url": "https://streamlake.example/terms", "status_page_url": null, "headquarters": "CN", "datacenters": ["CN", "SG"] },
            { "name": "Mancer 2", "slug": "mancer-other", "privacy_policy_url": null, "headquarters": "US", "datacenters": ["US"] }]);
        let v = endpoints_view(&raw, &Value::Null, &providers); let e = v["endpoints"].as_array().unwrap();
        let by = |n: &str| e.iter().find(|x| s(x, "provider") == n).unwrap().clone();
        let (sl, di, mc) = (by("StreamLake"), by("DeepInfra"), by("Mancer 2"));
        assert_eq!((sl["discount"].clone(), sl["inPerM"].clone(), sl["inPerMBefore"].clone(), sl["outPerMBefore"].clone()), (json!(0.41), json!(0.0826), json!(0.14), json!(0.28)));
        assert!(di["discount"].is_null() && di["inPerMBefore"].is_null(), "no deal (0) is no deal: {di}");
        assert_eq!(sl["tiers"], json!([{ "minPromptTokens": 128000, "inPerM": 0.1, "outPerM": 0.2, "cacheReadPerM": null, "cacheWritePerM": null }, { "minPromptTokens": 200000, "inPerM": 0.1652, "outPerM": 0.3304, "cacheReadPerM": null, "cacheWritePerM": null }]));
        assert_eq!(sl["more"], json!({ "reasoningPerM": 0.1652, "webSearchEach": 0.01 }));
        assert_eq!((sl["uptime5m"].clone(), sl["uptime1d"].clone(), sl["maxPrompt"].clone(), sl["params"].clone()), (json!(100.0), json!(98.25), json!(120000), json!(["tools", "reasoning"])));
        assert_eq!(sl["providerInfo"], json!({ "slug": "streamlake", "privacyUrl": "https://streamlake.example/privacy", "termsUrl": "https://streamlake.example/terms", "statusUrl": null, "headquarters": "CN", "datacenters": ["CN", "SG"] }));
        assert_eq!(mc["providerInfo"]["slug"], "mancer-other", "no slug match: by the provider's name");
        assert_eq!((di["serviceTier"].clone(), di["cacheWritePerM"].clone()), (json!("flex"), json!(0.1)));
        assert!(mc["free"] == json!(true) && sl["free"] == json!(false));
        assert_eq!(s(&sl, "detail"), "in $0.08/M, out $0.17/M, 41% off, 100%"); assert_eq!(s(&di, "detail"), "in $0.07/M, out $0.14/M, flex, cache write $0.10/M");
        assert_eq!(by("Tencent")["tiers"], json!([{ "inPerM": 0.132, "outPerM": 0.528, "cacheReadPerM": null, "cacheWritePerM": null, "fromUtc": "00:00", "toUtc": "16:00" },
            { "inPerM": 0.0825, "outPerM": 0.33, "cacheReadPerM": null, "cacheWritePerM": null, "fromUtc": "16:00", "toUtc": "00:00" }]), "an off-peak price by time of day; a time that is no time (2500) is skipped");
        assert_eq!(best_deal(&raw), Some(0.41)); assert_eq!(best_deal(&json!({ "data": { "endpoints": [{ "pricing": { "discount": 0 } }] } })), None);
    }
    #[test]
    fn the_model_list_can_be_only_the_deals_or_only_the_free_ones() {
        let raw = json!({ "data": [{ "id": "deepseek/deepseek-v4-flash", "name": "V4 Flash", "created": 3, "pricing": { "prompt": "0.00000009", "completion": "0.00000018" } },
            { "id": "qwen/qwen3-coder", "name": "Qwen3 Coder", "created": 2, "pricing": { "prompt": "0.0000002", "completion": "0.0000008" } },
            { "id": "qwen/qwen3-coder:free", "name": "Qwen3 Coder (free)", "created": 1, "pricing": { "prompt": "0", "completion": "0" } }] });
        let deals: HashMap<String, f64> = [("deepseek/deepseek-v4-flash".to_string(), 0.41), ("qwen/qwen3-coder".to_string(), 0.1)].into();
        let ids = |v: Value| v["models"].as_array().unwrap().iter().map(|m| format!("{} {}", s(m, "id"), m["bestDeal"])).collect::<Vec<_>>().join(", ");
        assert_eq!(ids(models_view(&raw, "", Some(&deals), ModelFilter { deals: true, free: false })), "deepseek/deepseek-v4-flash 0.41, qwen/qwen3-coder 0.1");
        assert_eq!(ids(models_view(&raw, "", Some(&deals), ModelFilter { deals: false, free: true })), "qwen/qwen3-coder:free null");
        assert_eq!(ids(models_view(&raw, "qwen", None, ModelFilter { deals: true, free: false })), "", "no index yet: no model is known to have a deal");
        assert_eq!(models_view(&raw, "", None, ModelFilter::default())["models"][2]["free"], json!(true));
    }
    #[test]
    fn only_a_real_model_id_reaches_openrouters_path() {
        assert!(model_ok("deepseek/deepseek-chat-v3.1") && model_ok("openai/gpt-oss-120b:free"));
        for bad in ["", "deepseek", "a/b/c", "../x", "a/ b", "a/b?x=1", "a/b#c"] { assert!(!model_ok(bad), "{bad}"); }
    }
}
