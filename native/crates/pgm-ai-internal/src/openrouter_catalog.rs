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
//! Cached in memory: the model list 24 h, a model's endpoints and the ZDR list 1 h (OpenRouter's own catalogue changes
//! slowly; jcode uses the same times).
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
}
/// OpenRouter sends prices as text ("0.00000025"); some catalogues send numbers.
fn num_or_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? { Some(Value::String(t)) => Some(t), Some(Value::Number(n)) => Some(n.to_string()), _ => None })
}
impl ModelPricing {
    /// USD per million tokens (OpenRouter's prices are per token); None when absent or negative (`-1` = varies).
    fn per_m(v: &Option<String>) -> Option<f64> { v.as_deref().and_then(|t| t.trim().parse::<f64>().ok()).filter(|x| x.is_finite() && *x >= 0.0).map(|x| (x * 1e6 * 10_000.0).round() / 10_000.0) }
    pub fn in_per_m(&self) -> Option<f64> { Self::per_m(&self.prompt) }
    pub fn out_per_m(&self) -> Option<f64> { Self::per_m(&self.completion) }
    pub fn cache_read_per_m(&self) -> Option<f64> { Self::per_m(&self.input_cache_read) }
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

/// The models as the page lists them, those matching every word of `q` (id or name, any case), newest first; at most
/// [`MAX_MODELS`].
pub fn models_view(raw: &Value, q: &str) -> Value {
    let models: Vec<ModelInfo> = raw["data"].as_array().into_iter().flatten().filter_map(|m| serde_json::from_value(m.clone()).ok()).collect();
    let words: Vec<String> = q.to_lowercase().split_whitespace().map(String::from).collect();
    let mut hit: Vec<&ModelInfo> = models.iter().filter(|m| { let hay = format!("{} {}", m.id, m.name).to_lowercase(); words.iter().all(|w| hay.contains(w.as_str())) }).collect();
    hit.sort_by(|a, b| b.created.unwrap_or(0).cmp(&a.created.unwrap_or(0)));
    let total = hit.len();
    let list: Vec<Value> = hit.into_iter().take(MAX_MODELS).map(|m| json!({
        "id": m.id, "name": if m.name.is_empty() { m.id.clone() } else { m.name.clone() }, "context": m.context_length,
        "inPerM": m.pricing.in_per_m(), "outPerM": m.pricing.out_per_m(),
        "input": m.architecture.as_ref().and_then(|a| a["input_modalities"].as_array().cloned()).unwrap_or_default(),
    })).collect();
    json!({ "models": list, "total": total })
}

/// One model's providers as the page lists them, fastest first (then the cheapest), each with its data policy.
pub fn endpoints_view(raw: &Value, zdr: &Value) -> Value {
    let d = &raw["data"];
    let model = s(d, "id");
    let mut eps: Vec<EndpointInfo> = d["endpoints"].as_array().into_iter().flatten().filter_map(|e| serde_json::from_value(e.clone()).ok()).collect();
    eps.sort_by(|a, b| b.tps_p50().unwrap_or(0.0).total_cmp(&a.tps_p50().unwrap_or(0.0)).then(a.pricing.in_per_m().unwrap_or(f64::MAX).total_cmp(&b.pricing.in_per_m().unwrap_or(f64::MAX))));
    let list: Vec<Value> = eps.iter().map(|e| {
        let (tier, words) = data_tier(zdr, &model, e);
        json!({
            "provider": e.provider_name, "route": e.route_name(), "tag": e.tag, "quantization": e.quantization.clone().filter(|q| q != "unknown"),
            "inPerM": e.pricing.in_per_m(), "outPerM": e.pricing.out_per_m(), "cacheReadPerM": e.pricing.cache_read_per_m(),
            "context": e.context_length, "maxOut": e.max_completion_tokens, "uptime30m": e.uptime_last_30m,
            "latencyP50Ms": e.latency_p50_ms().map(|x| x.round()), "tpsP50": e.tps_p50().map(|x| (x * 10.0).round() / 10.0), "status": e.status,
            "data": tier, "dataLabel": words, "detail": e.detail_string(),
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
    let key = hub.work().team_provider().map(|p| s(&p, "apiKey")).unwrap_or_default();
    if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
    let r = tokio::time::timeout(Duration::from_secs(25), rq.send()).await.map_err(|_| "OpenRouter did not answer in time.".to_string())?.map_err(|e| format!("OpenRouter could not be reached: {}", e.to_string().replace(&key, "…")))?;
    let st = r.status().as_u16();
    if !(200..300).contains(&st) { return Err(if st == 404 { "OpenRouter does not know that model.".into() } else { format!("OpenRouter answered {st}.") }); }
    r.json::<Value>().await.map_err(|_| "OpenRouter answered in an unknown shape.".to_string())
}
/// A model id as OpenRouter's path wants it (author/slug, nothing else).
pub fn model_ok(id: &str) -> bool { let mut p = id.split('/'); let (a, b, c) = (p.next(), p.next(), p.next()); c.is_none() && [a, b].iter().all(|x| x.is_some_and(|t| !t.is_empty() && !t.chars().all(|ch| ch == '.') && t.len() <= 100 && t.chars().all(|ch| ch.is_ascii_alphanumeric() || "-._:".contains(ch)))) }

pub async fn models(hub: &dyn AiHub, q: &str) -> Result<Value, String> {
    let raw = match cached("models", MODELS_TTL) { Some(v) => v, None => { let v = get(hub, "/models").await?; keep("models", &v); v } };
    Ok(models_view(&raw, q))
}
pub async fn zdr(hub: &dyn AiHub) -> Value {
    if let Some(v) = cached("zdr", ENDPOINTS_TTL) { return v; }
    match get(hub, "/endpoints/zdr").await { Ok(v) => { let d = v["data"].clone(); keep("zdr", &d); d } Err(_) => Value::Null } // without it every endpoint shows as "retains, may train"
}
pub async fn endpoints(hub: &dyn AiHub, model: &str) -> Result<Value, String> {
    if !model_ok(model) { return Err("A model is its OpenRouter id, author/slug (e.g. deepseek/deepseek-chat-v3.1).".into()); }
    let k = format!("ep:{model}");
    let raw = match cached(&k, ENDPOINTS_TTL) { Some(v) => v, None => { let v = get(hub, &format!("/models/{model}/endpoints")).await?; keep(&k, &v); v } };
    let z = zdr(hub).await;
    Ok(endpoints_view(&raw, &z))
}

/// `GET /api/openrouter/catalog/models?q=` and `GET /api/openrouter/catalog/endpoints?model=<author/slug>`: AI Admin's
/// model and provider picker (the Manage AI permission).
pub async fn route(hub: &Arc<dyn AiHub>, r: &Req, a: &Account) -> Option<Resp> {
    if r.method != hyper::Method::GET || !r.path.starts_with("/api/openrouter/catalog/") { return None; }
    if !crate::openrouter::manages_ai(&**hub, &a.username) { return Some(answer(403, json!({ "error": pgm_identity_internal::siteroles::needs(pgm_identity_internal::siteroles::MANAGE_AI) }))); }
    let got = match r.path.as_str() {
        "/api/openrouter/catalog/models" => models(&**hub, &r.param("q").unwrap_or_default().chars().take(100).collect::<String>()).await,
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
        let v = endpoints_view(&raw, &zdr); let e = v["endpoints"].as_array().unwrap();
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
        let v = models_view(&raw, "DeepSeek ");
        assert_eq!(v["total"], 2); assert_eq!(v["models"][0]["id"], "deepseek/deepseek-chat-v3.1"); assert_eq!(v["models"][0]["inPerM"], json!(0.2)); assert_eq!(v["models"][0]["input"], json!(["text"]));
        assert!(v["models"][1]["inPerM"].is_null(), "a varying price (-1) is no price"); assert_eq!(v["models"][1]["outPerM"], json!(2.0));
        assert_eq!(models_view(&raw, "deepseek r1")["total"], 1); assert_eq!(models_view(&raw, "")["models"][0]["id"], "qwen/qwen3-coder");
    }
    #[test]
    fn only_a_real_model_id_reaches_openrouters_path() {
        assert!(model_ok("deepseek/deepseek-chat-v3.1") && model_ok("openai/gpt-oss-120b:free"));
        for bad in ["", "deepseek", "a/b/c", "../x", "a/ b", "a/b?x=1", "a/b#c"] { assert!(!model_ok(bad), "{bad}"); }
    }
}
