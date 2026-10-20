//! The local API proxy: http://127.0.0.1:<port>/v1/… → the hub's /v1/… (OpenAI and Anthropic formats, bridge/openai.js)
//! with this person's API key added, so tools on this computer (an IDE, an SDK, a script) use VibeRoller's AI with no
//! key of their own. Only this computer can reach it (127.0.0.1), only /v1 passes, answers stream through as they come,
//! and requests a web page could make (an Origin header, or not JSON) are refused, so no website can use it.
use bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::sync::Mutex;

type Body = BoxBody<Bytes, std::io::Error>;
fn text(status: StatusCode, msg: &str) -> Response<Body> {
    let b = serde_json::json!({ "error": { "message": msg, "type": "proxy_error" } }).to_string();
    Response::builder().status(status).header("content-type", "application/json").body(Full::new(Bytes::from(b)).map_err(|e| match e {}).boxed()).expect("response")
}

#[derive(Default)]
pub struct Proxy { stop: Mutex<Option<tokio::sync::oneshot::Sender<()>>>, pub port: Mutex<u16> }
impl Proxy {
    pub fn running(&self) -> bool { self.stop.lock().unwrap().is_some() }
    pub fn stop(&self) { if let Some(s) = self.stop.lock().unwrap().take() { let _ = s.send(()); } *self.port.lock().unwrap() = 0; }
    pub async fn start(&self, port: u16, hub: String, key: String) -> Result<(), String> {
        self.stop();
        if key.is_empty() { return Err("Set your VibeRoller API key first (the hub: Settings → API access).".into()); }
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| format!("Could not listen on 127.0.0.1:{port}: {e}"))?;
        let http = vr_core::hub::http_client(std::time::Duration::from_secs(15)).map_err(|e| e.to_string())?;
        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        *self.stop.lock().unwrap() = Some(tx);
        *self.port.lock().unwrap() = port;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    a = listener.accept() => {
                        let Ok((stream, _)) = a else { continue };
                        let (http, hub, key) = (http.clone(), hub.clone(), key.clone());
                        tokio::spawn(async move {
                            let svc = hyper::service::service_fn(move |req| { let (http, hub, key) = (http.clone(), hub.clone(), key.clone()); async move { Ok::<_, std::convert::Infallible>(forward(req, &http, &hub, &key).await) } });
                            let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), svc).await;
                        });
                    }
                }
            }
        });
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // a stand-in hub: answers every request with what it was sent (the path and the Authorization header)
    async fn fake_hub() -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move { loop { let (mut s, _) = l.accept().await.unwrap(); tokio::spawn(async move {
            let mut buf = vec![0u8; 8192]; let n = s.read(&mut buf).await.unwrap_or(0); let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
            let auth = req.lines().find(|l| l.to_ascii_lowercase().starts_with("authorization:")).map(|l| l[14..].trim().to_string()).unwrap_or_default();
            let body = serde_json::json!({ "path": path, "auth": auth }).to_string();
            let _ = s.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
        }); } });
        port
    }
    #[tokio::test]
    async fn forwards_v1_with_the_key_and_refuses_web_pages() {
        let hub = fake_hub().await;
        let p = super::Proxy::default();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let port = l.local_addr().unwrap().port(); drop(l);
        assert!(p.start(port, format!("http://127.0.0.1:{hub}"), String::new()).await.is_err()); // no key, no proxy
        p.start(port, format!("http://127.0.0.1:{hub}"), "mills_testkey".into()).await.unwrap();
        let c = reqwest::Client::new();
        let ok: serde_json::Value = c.get(format!("http://127.0.0.1:{port}/v1/models")).send().await.unwrap().json().await.unwrap();
        assert_eq!(ok["path"], "/v1/models"); assert_eq!(ok["auth"], "Bearer mills_testkey");
        assert_eq!(c.get(format!("http://127.0.0.1:{port}/api/check")).send().await.unwrap().status(), 404);
        assert_eq!(c.get(format!("http://127.0.0.1:{port}/v1/models")).header("origin", "https://evil.example").send().await.unwrap().status(), 403);
        assert_eq!(c.post(format!("http://127.0.0.1:{port}/v1/messages")).header("content-type", "text/plain").body("{}").send().await.unwrap().status(), 415);
        p.stop();
    }
}
async fn forward(req: Request<Incoming>, http: &reqwest::Client, hub: &str, key: &str) -> Response<Body> {
    let path = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
    if !path.starts_with("/v1/") && path != "/v1" { return text(StatusCode::NOT_FOUND, "Only /v1 (the OpenAI- and Anthropic-compatible API) passes here."); }
    if req.headers().contains_key("origin") { return text(StatusCode::FORBIDDEN, "Web pages may not use this proxy."); }
    let method = req.method().clone();
    if method == hyper::Method::POST && !req.headers().get("content-type").and_then(|v| v.to_str().ok()).map(|v| v.starts_with("application/json")).unwrap_or(false) {
        return text(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Send JSON (Content-Type: application/json).");
    }
    let pass: Vec<(String, String)> = req.headers().iter().filter(|(k, _)| matches!(k.as_str(), "content-type" | "accept" | "anthropic-version" | "anthropic-beta")).filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string()))).collect();
    let body = match req.into_body().collect().await { Ok(b) => b.to_bytes(), Err(_) => return text(StatusCode::BAD_REQUEST, "Could not read the request.") };
    let mut rb = http.request(reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET), format!("{}{}", hub.trim_end_matches('/'), path)).bearer_auth(key).timeout(std::time::Duration::from_secs(600));
    for (k, v) in pass { rb = rb.header(k, v); }
    if !body.is_empty() { rb = rb.body(body); }
    match rb.send().await {
        Ok(r) => {
            let mut b = Response::builder().status(r.status().as_u16());
            for (k, v) in r.headers() { if matches!(k.as_str(), "content-type" | "cache-control" | "request-id") { b = b.header(k.as_str(), v.as_bytes()); } }
            let stream = r.bytes_stream().map_ok(Frame::data).map_err(std::io::Error::other);
            b.body(BodyExt::boxed(StreamBody::new(stream))).unwrap_or_else(|_| text(StatusCode::BAD_GATEWAY, "bad answer"))
        }
        Err(e) => text(StatusCode::BAD_GATEWAY, &format!("The hub could not be reached: {e}")),
    }
}
