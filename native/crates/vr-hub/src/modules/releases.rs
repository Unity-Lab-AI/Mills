//! The team's product releases (T-250, part of T-196): every product, every version, for signed-in accounts only (every
//! account of this hub is the team; a signed-out request is 401 before it gets here, a device token counts as its person).
//!   GET /api/releases                                  products → releases (version, date, notes, assets: name, size, platform)
//!   GET /api/releases/file?product=&tag=&asset=        one asset, fetched from Forgejo with the hub's own token and streamed
//! The token (env VR_FORGEJO_TOKEN, else forgejo.json "releaseToken", else its "token") stays in the hub: it is only ever
//! sent to the Forgejo address, never to the page, never logged. Which repos: config "releases": [{product, repo, label}]
//! (default: the four PGM products). The list is cached for a few minutes. AI hosts keep updating through /node/update.
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{answer, Routed};
use crate::front::{Hub, Req};

const TTL: Duration = Duration::from_secs(300);
pub struct Releases { cache: Mutex<Option<(Instant, Value)>> }
impl Releases { pub fn new() -> Releases { Releases { cache: Mutex::new(None) } } }

fn forgejo_json(hub: &Hub) -> Value { std::fs::read_to_string(hub.root.join("forgejo.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null) }
fn base(hub: &Hub) -> String { forgejo_json(hub)["url"].as_str().map(String::from).or_else(|| hub.cfg.get("forgejoUrl").and_then(Value::as_str).map(String::from)).unwrap_or_else(|| "https://git.unityailab.com".into()).trim_end_matches('/').to_string() }
fn token(hub: &Hub) -> String {
    if let Ok(t) = std::env::var("VR_FORGEJO_TOKEN") { if !t.trim().is_empty() { return t.trim().to_string(); } }
    let f = forgejo_json(hub);
    f["releaseToken"].as_str().or(f["token"].as_str()).unwrap_or("").trim().to_string()
}
fn safe_repo(r: &str) -> bool { let mut p = r.split('/'); let (a, b, c) = (p.next(), p.next(), p.next()); c.is_none() && [a, b].iter().all(|x| x.map(|s| !s.is_empty() && s != "." && s != ".." && s.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_.".contains(ch))).unwrap_or(false)) }
/// [(product, repo, label)] from config "releases", else the four products.
fn products(hub: &Hub) -> Vec<(String, String, String)> {
    let from_cfg: Vec<(String, String, String)> = hub.cfg.get("releases").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| {
        let (p, r) = (x["product"].as_str()?, x["repo"].as_str()?);
        if p.is_empty() || !safe_repo(r) { return None; }
        Some((p.to_string(), r.to_string(), x["label"].as_str().unwrap_or(p).to_string()))
    }).collect()).unwrap_or_default();
    if !from_cfg.is_empty() { return from_cfg; }
    [("pgm-shadowchat", "UnityAILab/pgm-shadowchat", "ShaDow Chat"), ("pgm-hyperprodman", "UnityAILab/pgm-hyperprodman", "Hyper Prod Man"), ("pgm-prodsuit", "UnityAILab/pgm-prodsuit", "Prod Suit"), ("jcode-pgm", "UnityAILab/Jcode-PGM-releases", "Jcode PGM")]
        .iter().map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())).collect()
}
/// "Windows", "Linux", "macOS" or "" from a file's name (darwin first: it contains "win").
pub fn platform(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    if n.contains("darwin") || n.contains("macos") || n.contains("apple") || n.ends_with(".dmg") || n.contains("osx") { "macOS" }
    else if n.contains("windows") || n.contains("win64") || n.contains("win32") || n.contains("-win") || n.contains("_win") || n.ends_with(".exe") || n.ends_with(".msi") { "Windows" }
    else if n.contains("linux") || n.ends_with(".deb") || n.ends_with(".rpm") || n.to_ascii_lowercase().ends_with(".appimage") { "Linux" }
    else { "" }
}
fn enc(s: &str) -> String { s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect() }
fn auth(hub: &Hub, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder { let t = token(hub); if t.is_empty() { rb } else { rb.header("authorization", format!("token {t}")) } }

async fn fetch_one(hub: &Hub, http: &reqwest::Client, base: &str, product: &str, repo: &str, label: &str) -> Value {
    let url = format!("{base}/api/v1/repos/{repo}/releases?limit=50");
    let res = tokio::time::timeout(Duration::from_secs(15), auth(hub, http.get(&url).header("accept", "application/json")).send()).await;
    let list = match res {
        Ok(Ok(r)) if r.status().is_success() => r.json::<Value>().await.ok(),
        Ok(Ok(r)) => return json!({ "product": product, "label": label, "repo": repo, "releases": [], "error": format!("Forgejo answered {}.", r.status().as_u16()) }),
        _ => return json!({ "product": product, "label": label, "repo": repo, "releases": [], "error": "Forgejo did not answer." }),
    };
    let releases: Vec<Value> = list.as_ref().and_then(Value::as_array).map(|a| a.iter().filter(|r| r["draft"] != json!(true)).map(|r| {
        let assets: Vec<Value> = r["assets"].as_array().map(|a| a.iter().filter_map(|x| { let n = x["name"].as_str()?; Some(json!({ "name": n, "size": x["size"].as_u64().unwrap_or(0), "platform": platform(n) })) }).collect()).unwrap_or_default();
        json!({ "version": r["tag_name"].as_str().unwrap_or(""), "name": r["name"].as_str().unwrap_or(""), "date": r["published_at"].as_str().or(r["created_at"].as_str()).unwrap_or(""), "prerelease": r["prerelease"] == json!(true), "notes": r["body"].as_str().unwrap_or("").chars().take(4000).collect::<String>(), "assets": assets })
    }).collect()).unwrap_or_default();
    json!({ "product": product, "label": label, "repo": repo, "releases": releases })
}
async fn catalogue(hub: &Hub) -> Value {
    if let Some((at, v)) = hub.mods.releases.cache.lock().unwrap_or_else(|e| e.into_inner()).as_ref() { if at.elapsed() < TTL { return v.clone(); } }
    let http = vr_core::hub::http_client(Duration::from_secs(10)).expect("http");
    let b = base(hub);
    let mut out = vec![];
    for (p, r, l) in products(hub) { out.push(fetch_one(hub, &http, &b, &p, &r, &l).await); }
    let v = json!({ "products": out, "at": super::auth::iso_now() });
    // a catalogue where every product failed is not kept (the next look asks again)
    if out.iter().any(|p| p["error"].is_null()) { *hub.mods.releases.cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), v.clone())); }
    v
}

/// Signed in (the caller has checked): the catalogue and the files.
pub async fn route(hub: &Hub, r: &Req) -> Option<Routed> {
    if r.method != hyper::Method::GET { return None; }
    match r.path.as_str() {
        "/api/releases" => Some(answer(200, catalogue(hub).await)),
        "/api/releases/file" => Some(file(hub, r).await),
        _ => None,
    }
}
async fn file(hub: &Hub, r: &Req) -> Routed {
    let (product, tag, asset) = (r.param("product").unwrap_or_default(), r.param("tag").unwrap_or_default(), r.param("asset").unwrap_or_default());
    // only what the catalogue lists can be asked for (so the path sent to Forgejo is never the caller's own invention)
    let cat = catalogue(hub).await;
    let found = cat["products"].as_array().and_then(|a| a.iter().find(|p| p["product"] == json!(product)))
        .and_then(|p| p["releases"].as_array().and_then(|rs| rs.iter().find(|x| x["version"] == json!(tag))).map(|rel| (p["repo"].as_str().unwrap_or("").to_string(), rel["assets"].as_array().map(|a| a.iter().any(|x| x["name"] == json!(asset))).unwrap_or(false))));
    let Some((repo, true)) = found else { return answer(404, json!({ "error": "No such file in that release." })) };
    let url = format!("{}/{repo}/releases/download/{}/{}", base(hub), enc(&tag), enc(&asset));
    let http = vr_core::hub::http_client(Duration::from_secs(10)).expect("http");
    let resp = match tokio::time::timeout(Duration::from_secs(30), auth(hub, http.get(&url)).send()).await {
        Ok(Ok(x)) if x.status().is_success() => x,
        Ok(Ok(x)) => return answer(502, json!({ "error": format!("Forgejo answered {}.", x.status().as_u16()) })),
        _ => return answer(502, json!({ "error": "Forgejo did not answer." })),
    };
    let len = resp.content_length();
    let name: String = asset.chars().filter(|c| !c.is_control() && !"\"\\/;".contains(*c)).collect();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<hyper::body::Frame<bytes::Bytes>, Box<dyn std::error::Error + Send + Sync>>>(4);
    tokio::spawn(async move {
        let mut resp = resp;
        loop {
            match resp.chunk().await {
                Ok(Some(c)) => { if tx.send(Ok(hyper::body::Frame::data(c))).await.is_err() { return; } }
                Ok(None) => return,
                Err(e) => { let _ = tx.send(Err(Box::new(e))).await; return; }
            }
        }
    });
    use http_body_util::BodyExt;
    let body = http_body_util::StreamBody::new(futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) })).boxed();
    let mut b = hyper::Response::builder().status(200).header("content-type", "application/octet-stream").header("content-disposition", format!("attachment; filename=\"{name}\"")).header("cache-control", "no-store").header("x-content-type-options", "nosniff");
    if let Some(l) = len { b = b.header("content-length", l); }
    Routed::Answer(b.body(body).expect("response"))
}
