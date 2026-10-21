//! Unity's look at the world (from bridge/assistant.js): where someone is, weather, news (keyless public services),
//! web search, reading a public page (never this machine's own network), YouTube.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130 Safari/537.36";
pub struct Web { http: reqwest::Client, manual: reqwest::Client, cache: Mutex<HashMap<String, (u64, Value)>>, search: String, youtube: String, private_ok: bool }
/// Where the person is, as far as anyone knows (lat / lon may be unknown).
#[derive(Clone, Default)]
pub struct Place { pub lat: Option<f64>, pub lon: Option<f64>, pub place: String, pub country: String, pub locale: String }
fn s(v: &Value, k: &str) -> String { match &v[k] { Value::String(x) => x.clone(), Value::Null => String::new(), o => o.to_string() } }
fn enc(x: &str) -> String { vr_core::hub::urlencode(x) }

pub fn unhtml(t: &str) -> String {
    let t = regex::Regex::new(r"(?is)<script.*?</script>|<style.*?</style>|<noscript.*?</noscript>").unwrap().replace_all(t, " ");
    let t = regex::Regex::new(r"<[^>]+>").unwrap().replace_all(&t, " ");
    let t = t.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#039;", "'").replace("&#39;", "'");
    let t = regex::Regex::new(r"&#(\d+);").unwrap().replace_all(&t, |c: &regex::Captures| c[1].parse::<u32>().ok().and_then(char::from_u32).map(String::from).unwrap_or_default());
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}
impl Web {
    pub fn new(a: &Value) -> Web {
        let _ = vr_core::hub::http_client(Duration::from_secs(10)); // (the TLS provider)
        let b = || reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).user_agent("VibeRoller/1.0 (team app)");
        Web { http: b().build().expect("http"), manual: b().redirect(reqwest::redirect::Policy::none()).build().expect("http"), cache: Mutex::default(),
            search: a["searchUrl"].as_str().unwrap_or("https://www.bing.com/search").into(), youtube: a["youtubeUrl"].as_str().unwrap_or("https://www.youtube.com/results").into(), private_ok: a["allowPrivateFetch"] == json!(true) }
    }
    /// JSON (or an RSS text) from a URL, kept for `ttl` ms.
    async fn get(&self, url: &str, ttl: u64) -> Result<Value, String> {
        if let Some((t, v)) = self.cache.lock().unwrap().get(url) { if vr_core::now_ms() - t < ttl { return Ok(v.clone()); } }
        let r = tokio::time::timeout(Duration::from_secs(10), self.http.get(url).send()).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.to_string())?;
        if !r.status().is_success() { return Err(format!("{} answered {}", reqwest::Url::parse(url).map(|u| u.host_str().unwrap_or("").to_string()).unwrap_or_default(), r.status().as_u16())); }
        let text = r.text().await.map_err(|e| e.to_string())?;
        let v = if url.contains("/rss") { json!(text) } else { serde_json::from_str(&text).map_err(|e| e.to_string())? };
        self.cache.lock().unwrap().insert(url.to_string(), (vr_core::now_ms(), v.clone()));
        Ok(v)
    }
    /// The rough place of an address someone connects from.
    pub async fn ip_place(&self, ip: &str) -> Option<Value> {
        if ip.is_empty() || regex::Regex::new(r"^(127\.|::1|10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)").unwrap().is_match(ip) { return None; }
        let r = self.get(&format!("https://ipwho.is/{}", enc(ip)), 6 * 3_600_000).await.ok()?;
        if r["success"] != json!(true) { return None; }
        let place = [s(&r, "city"), s(&r, "region"), s(&r, "country")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(", ");
        Some(json!({ "lat": r["latitude"], "lon": r["longitude"], "place": place, "country": r["country_code"], "tz": r["timezone"]["id"] }))
    }
    pub async fn place_name(&self, lat: f64, lon: f64) -> Option<(String, String)> {
        let r = self.get(&format!("https://nominatim.openstreetmap.org/reverse?format=json&zoom=10&lat={lat:.3}&lon={lon:.3}"), 24 * 3_600_000).await.ok()?;
        let a = &r["address"]; let city = [s(a, "city"), s(a, "town"), s(a, "village"), s(a, "county")].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
        Some(([city, s(a, "state"), s(a, "country")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(", "), s(a, "country_code").to_uppercase()))
    }
    pub async fn locate(&self, ci: &mut Place, city: &str) -> Result<(f64, f64, String, String), String> {
        if !city.is_empty() {
            let r = self.get(&format!("https://geocoding-api.open-meteo.com/v1/search?count=1&name={}", enc(city)), 24 * 3_600_000).await?;
            let g = &r["results"][0]; if g.is_null() { return Err(format!("No place called {city}")); }
            return Ok((g["latitude"].as_f64().unwrap_or(0.0), g["longitude"].as_f64().unwrap_or(0.0), [s(g, "name"), s(g, "admin1"), s(g, "country")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(", "), s(g, "country_code")));
        }
        let (Some(lat), Some(lon)) = (ci.lat, ci.lon) else { return Err("No location for them yet; ask which city.".into()) };
        if ci.place.is_empty() { if let Some((p, c)) = self.place_name(lat, lon).await { ci.place = p; ci.country = c; } }
        Ok((lat, lon, ci.place.clone(), ci.country.clone()))
    }
    pub async fn weather(&self, ci: &mut Place, city: &str, days: i64) -> Result<Value, String> {
        let (lat, lon, place, country) = self.locate(ci, city).await?;
        let us = country.eq_ignore_ascii_case("US") || ci.locale.to_uppercase().ends_with("-US");
        let q = format!("latitude={lat}&longitude={lon}&timezone=auto&current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max&forecast_days={}{}", days.clamp(1, 7), if us { "&temperature_unit=fahrenheit&wind_speed_unit=mph" } else { "" });
        let r = self.get(&format!("https://api.open-meteo.com/v1/forecast?{q}"), 600_000).await?;
        let (c, d, u) = (&r["current"], &r["daily"], if us { "°F" } else { "°C" });
        let wmo = |code: &Value| -> String { let n = code.as_i64().unwrap_or(-1); match n { 0 => "clear", 1 => "mostly clear", 2 => "partly cloudy", 3 => "overcast", 45 => "fog", 48 => "freezing fog", 51 => "light drizzle", 53 => "drizzle", 55 => "heavy drizzle", 61 => "light rain", 63 => "rain", 65 => "heavy rain", 66 => "freezing rain", 67 => "heavy freezing rain", 71 => "light snow", 73 => "snow", 75 => "heavy snow", 77 => "snow grains", 80 | 81 => "rain showers", 82 => "violent rain showers", 85 => "snow showers", 86 => "heavy snow showers", 95 => "thunderstorm", 96 => "thunderstorm with hail", 99 => "thunderstorm with heavy hail", _ => "" }.to_string() };
        let rd = |v: &Value| v.as_f64().map(|x| x.round() as i64).map(|x| x.to_string()).unwrap_or_else(|| "?".into());
        let now_w = { let w = wmo(&c["weather_code"]); if w.is_empty() { format!("weather code {}", c["weather_code"]) } else { w } };
        let days_v: Vec<String> = d["time"].as_array().cloned().unwrap_or_default().iter().enumerate().map(|(i, t)| format!("{}: {}, {}–{}{u}, rain {}%", t.as_str().unwrap_or(""), wmo(&d["weather_code"][i]), rd(&d["temperature_2m_min"][i]), rd(&d["temperature_2m_max"][i]), if d["precipitation_probability_max"][i].is_null() { "?".into() } else { d["precipitation_probability_max"][i].to_string() })).collect();
        Ok(json!({ "ok": true, "place": place, "now": format!("{now_w}, {}{u} (feels {}{u}), humidity {}%, wind {} {}", rd(&c["temperature_2m"]), rd(&c["apparent_temperature"]), c["relative_humidity_2m"], rd(&c["wind_speed_10m"]), if us { "mph" } else { "km/h" }), "days": days_v }))
    }
    pub async fn news(&self, ci: &mut Place, topic: &str, local: bool) -> Result<Value, String> {
        let loc = if ci.locale.is_empty() { "en-US".to_string() } else { ci.locale.clone() };
        let lang = loc.split('-').next().unwrap_or("en").to_string();
        let gl = if !ci.country.is_empty() { ci.country.to_uppercase() } else { loc.split('-').nth(1).unwrap_or("US").to_uppercase() };
        let base = format!("hl={lang}-{gl}&gl={gl}&ceid={gl}:{lang}");
        let mut q = topic.to_string();
        if local && q.is_empty() { if let Ok((_, _, p, _)) = self.locate(ci, "").await { q = p.split(',').next().unwrap_or("").to_string(); } }
        let url = if q.is_empty() { format!("https://news.google.com/rss?{base}") } else { format!("https://news.google.com/rss/search?q={}&{base}", enc(&q)) };
        let xml = self.get(&url, 900_000).await?; let xml = xml.as_str().unwrap_or("").to_string();
        let tag = |body: &str, t: &str| -> String { regex::Regex::new(&format!(r"(?s)<{t}>(.*?)</{t}>")).unwrap().captures(body).map(|c| c[1].to_string()).unwrap_or_default().replace("<![CDATA[", "").replace("]]>", "").replace("&amp;", "&").replace("&#39;", "'").replace("&quot;", "\"").trim().to_string() };
        let items: Vec<Value> = regex::Regex::new(r"(?s)<item>(.*?)</item>").unwrap().captures_iter(&xml).take(8).map(|c| json!({ "title": tag(&c[1], "title"), "when": tag(&c[1], "pubDate"), "source": tag(&c[1], "source") })).collect();
        Ok(json!({ "ok": true, "about": if q.is_empty() { "top stories".to_string() } else { q }, "items": items }))
    }
    pub async fn search(&self, q: &str) -> Result<Value, String> {
        let r = tokio::time::timeout(Duration::from_secs(12), self.http.get(format!("{}?q={}&setlang=en", self.search, enc(q))).header("user-agent", UA).header("accept-language", "en-US,en;q=0.8").send()).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.to_string())?;
        if !r.status().is_success() { return Err(format!("search answered {}", r.status().as_u16())); }
        let html = r.text().await.map_err(|e| e.to_string())?;
        let a_re = regex::Regex::new(r#"(?s)<h2[^>]*>\s*<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap(); let p_re = regex::Regex::new(r"(?s)<p[^>]*>(.*?)</p>").unwrap();
        let mut results = vec![];
        for m in regex::Regex::new(r#"(?s)<li class="b_algo".*?</li>"#).unwrap().find_iter(&html) {
            let Some(a) = a_re.captures(m.as_str()) else { continue };
            let snip = p_re.captures(m.as_str()).map(|c| c[1].to_string()).unwrap_or_default();
            results.push(json!({ "title": unhtml(&a[2]), "url": real_link(&a[1]), "snippet": unhtml(&snip).chars().take(300).collect::<String>() }));
            if results.len() >= 8 { break; }
        }
        Ok(json!({ "ok": true, "query": q, "results": results }))
    }
    /// Only http(s), only public addresses (unless the hub config allows private ones: the regression runner's).
    /// The address, and the IP it was checked against (the request goes to exactly that one: no second lookup that a
    /// name could answer differently).
    async fn public_url(&self, raw: &str) -> Result<(reqwest::Url, Option<std::net::IpAddr>), String> {
        let u = reqwest::Url::parse(raw).map_err(|_| "Not a web address.".to_string())?;
        if u.scheme() != "http" && u.scheme() != "https" { return Err("Only http and https pages.".into()); }
        if self.private_ok { return Ok((u, None)); }
        if let Some(p) = u.port() { if ![80, 443, 8080].contains(&p) { return Err("That port is not allowed.".into()); } }
        let host = u.host_str().unwrap_or("").trim_matches(|c| c == '[' || c == ']').to_string();
        let ips: Vec<std::net::IpAddr> = match host.parse::<std::net::IpAddr>() { Ok(ip) => vec![ip], Err(_) => tokio::net::lookup_host((host.as_str(), 80)).await.map(|i| i.map(|a| a.ip()).collect()).unwrap_or_default() };
        let bad = |ip: &std::net::IpAddr| match ip { std::net::IpAddr::V4(v) => { let o = v.octets(); v.is_loopback() || v.is_private() || v.is_link_local() || o[0] == 0 || (o[0] == 100 && (64..=127).contains(&o[1])) } std::net::IpAddr::V6(v) => { let s0 = v.segments()[0]; v.is_loopback() || v.is_unspecified() || (s0 & 0xfe00) == 0xfc00 || (s0 & 0xffc0) == 0xfe80 || v.to_ipv4_mapped().map(|m| m.is_loopback() || m.is_private() || m.is_link_local()).unwrap_or(false) } };
        if ips.is_empty() || ips.iter().any(bad) || host.eq_ignore_ascii_case("localhost") { return Err("That address is on a private network; only public pages.".into()); }
        Ok((u, ips.first().copied()))
    }
    pub async fn read_page(&self, raw: &str) -> Result<Value, String> {
        let (mut u, mut ip) = self.public_url(raw).await?;
        for _ in 0..4 { // every hop of a redirect is checked
            let client = match (ip, u.host_str()) {
                (Some(ip), Some(h)) => reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).resolve(h, std::net::SocketAddr::new(ip, u.port_or_known_default().unwrap_or(443))).build().map_err(|e| e.to_string())?,
                _ => self.manual.clone(),
            };
            let mut r = tokio::time::timeout(Duration::from_secs(15), client.get(u.clone()).header("user-agent", UA).send()).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.to_string())?;
            let st = r.status().as_u16();
            if (300..400).contains(&st) { if let Some(l) = r.headers().get("location").and_then(|v| v.to_str().ok()) { let next = u.join(l).map_err(|e| e.to_string())?; (u, ip) = self.public_url(next.as_str()).await?; continue; } }
            if !r.status().is_success() { return Err(format!("The page answered {st}")); }
            let ty = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            if !["text", "html", "json", "xml"].iter().any(|k| ty.contains(k)) { return Err(format!("Not a text page ({ty}).")); }
            // at most 2 MB, within the time limit (an endless page cannot fill this hub's memory)
            let mut buf: Vec<u8> = vec![];
            let read = async { while let Ok(Some(c)) = r.chunk().await { buf.extend_from_slice(&c); if buf.len() >= 2_000_000 { break; } } };
            let _ = tokio::time::timeout(Duration::from_secs(20), read).await;
            let body: String = String::from_utf8_lossy(&buf).chars().take(2_000_000).collect();
            let title = regex::Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap().captures(&body).map(|c| unhtml(&c[1])).unwrap_or_default();
            let main = regex::Regex::new(r"(?is)<article.*?</article>|<main.*?</main>").unwrap().find(&body).map(|m| m.as_str().to_string());
            return Ok(json!({ "ok": true, "url": u.as_str(), "title": title, "text": unhtml(main.as_deref().unwrap_or(&body)).chars().take(6000).collect::<String>() }));
        }
        Err("Too many redirects.".into())
    }
    /// The first video for a search: (id, title).
    pub async fn youtube(&self, q: &str) -> Result<(String, String), String> {
        let r = tokio::time::timeout(Duration::from_secs(12), self.http.get(format!("{}?search_query={}", self.youtube, enc(q))).header("user-agent", UA).send()).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.to_string())?;
        let html = r.text().await.map_err(|e| e.to_string())?;
        if let Some(c) = regex::Regex::new(r#"(?s)"videoRenderer":\{"videoId":"([\w-]{11})".*?"title":\{"runs":\[\{"text":"((?:[^"\\]|\\.)*)""#).unwrap().captures(&html) {
            return Ok((c[1].to_string(), serde_json::from_str::<String>(&format!("\"{}\"", &c[2])).unwrap_or_else(|_| q.to_string())));
        }
        regex::Regex::new(r#""videoId":"([\w-]{11})""#).unwrap().captures(&html).map(|c| (c[1].to_string(), q.to_string())).ok_or_else(|| format!("No video found for {q}"))
    }
}
/// Bing wraps results as /ck/a?…&u=a1<base64url of the address>.
fn real_link(href: &str) -> String {
    let h = href.replace("&amp;", "&");
    if let Some(c) = regex::Regex::new(r"[?&]u=a1([^&]+)").unwrap().captures(&h) { use base64::Engine; if let Ok(b) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(c[1].trim_end_matches('=')) { if let Ok(t) = String::from_utf8(b) { return t; } } }
    h
}
