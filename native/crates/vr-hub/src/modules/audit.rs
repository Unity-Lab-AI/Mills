//! The security log, the hub's side (T-271; the chain itself is pgm-core-internal's `audit`): the file is
//! `<bridge>/security/audit.jsonl` (data, never shipped). Sign-ins, device locks and product sign-ins are recorded where
//! they happen (pgm-identity-internal); the rest is recorded here, from the request and its answer, for every route the
//! front sees (answered here or by the JavaScript hub behind it): what an admin changed, who shared a provider and with
//! what scope, release downloads, devices and machines given a token (delegation), and what changed at a start
//! (a new version gone live, a config switch such as rustHub.alone). Only chosen fields of a request go in, never a
//! password, token or key. Admins read it: GET /api/admin/audit?since=&limit=&action= → {entries, verified}.
use serde_json::{json, Value};

use super::{answer, Routed};
use crate::front::{Hub, Req};
use pgm_core_internal::audit;

/// Config keys whose change between two starts is a config switch worth a line (none of them holds a secret).
const SWITCHES: &[&str] = &["rustHub", "instance", "devAllow", "behindProxy", "allowedOrigins", "publicUrl", "hubClaude", "defaultUsername"];

/// At start: the log opened, and what changed since the last start (the version: a go-live or a rollback; a config switch).
pub fn started(root: &std::path::Path, cfg: &super::Config, alone: bool) {
    audit::init(&root.join("security").join("audit.jsonl"));
    let Some(l) = audit::global() else { return };
    let mut conf = serde_json::Map::new();
    for k in SWITCHES { if let Some(v) = cfg.get(k) { conf.insert((*k).into(), v.clone()); } }
    let now = json!({ "version": crate::VERSION, "alone": alone, "config": conf });
    if let Some(last) = l.last_of("hub.start") {
        let d = &last["detail"];
        if d["version"] != now["version"] { audit::record("", "hub.version", crate::VERSION, json!({ "from": d["version"], "to": now["version"] }), ""); }
        if d["alone"] != now["alone"] { audit::record("", "config.switch", "rustHub.alone", json!({ "from": d["alone"], "to": alone }), ""); }
        for k in SWITCHES { if *k != "rustHub" && d["config"][k] != now["config"][k] { audit::record("", "config.switch", k, json!({ "from": d["config"][k], "to": now["config"][k] }), ""); } }
        if d["config"]["rustHub"] != now["config"]["rustHub"] && d["alone"] == now["alone"] { audit::record("", "config.switch", "rustHub", json!({ "from": d["config"]["rustHub"], "to": now["config"]["rustHub"] }), ""); }
    }
    audit::record("", "hub.start", crate::VERSION, now, "");
}

fn sv(b: &Value, k: &str) -> String { match &b[k] { Value::String(s) => s.trim().to_string(), Value::Null => String::new(), v => v.to_string() } }
fn truthy(v: &Value) -> bool { !matches!(v, Value::Null | Value::Bool(false)) && *v != json!(0) && *v != json!("") }
/// The security-relevant action a request is, with its target and the fields of it that may be kept.
fn what(r: &Req) -> Option<(&'static str, String, Value)> {
    use hyper::Method;
    let (post, get) = (r.method == Method::POST, r.method == Method::GET);
    let p = r.path.as_str();
    let b = if post && ["/api/admin/", "/api/hosts/", "/api/providers/", "/api/device", "/api/guilds/server/password"].iter().any(|x| p.starts_with(x)) { r.json() } else { Value::Null };
    let user = || sv(&b, "username").to_lowercase();
    Some(match p {
        "/api/logout" if post => ("signout", String::new(), json!({})),
        "/api/admin/create" if post => ("account.create", user(), json!({ "admin": truthy(&b["admin"]) })),
        "/api/admin/delete" if post => ("account.delete", user(), json!({})),
        "/api/admin/set-admin" if post => ("account.role", user(), json!({ "admin": b["admin"] == json!(true) })),
        "/api/admin/password" if post => ("account.password", user(), json!({ "keepMachines": b["keepMachines"] == json!(true) })),
        "/api/admin/signout" if post => ("account.signout", user(), json!({ "keepMachines": b["keepMachines"] == json!(true) })),
        "/api/admin/unlock" if post => ("device.reset", user(), json!({})),
        "/api/admin/allow-device" if post => ("device.allow", user(), json!({})),
        "/api/admin/restart" if post => ("hub.restart", String::new(), json!({})),
        "/api/admin/unity-tools" if post => ("delegation.unityTools", user(), json!({ "pc": b["pc"], "code": b["code"] })),
        "/api/admin/gateway/products" if post => ("gateway.product", sv(&b, "id"), json!({ "name": sv(&b, "name"), "ai": b["ai"] == json!(true) })),
        "/api/admin/gateway/products/remove" if post => ("gateway.productRemove", sv(&b, "id"), json!({})),
        // a device-code sign-in approved: a tool (or a linked machine) gets a token of the person's own
        "/api/device/approve" if post => ("delegation.device", String::new(), json!({ "replace": truthy(&b["replace"]) })),
        "/api/device/deny" if post => ("device.deny", String::new(), json!({})),
        "/api/devices/revoke" if post => ("device.revoke", sv(&b, "id"), json!({})),
        "/api/hosts/new" if post => ("aihost.new", String::new(), json!({ "nickname": sv(&b, "nickname") })),
        "/api/hosts/remove" if post => ("aihost.remove", sv(&b, "id"), json!({})),
        "/api/hosts/share" if post => ("provider.share", sv(&b, "id"), json!({ "provider": sv(&b, "provider"), "users": b["users"] })),
        "/api/hosts/scope" if post => ("provider.scope", sv(&b, "id"), json!({ "provider": sv(&b, "provider"), "user": sv(&b, "user"), "models": b["models"], "uses": b["uses"] })),
        "/api/providers/add" if post => ("provider.add", sv(&b, "label"), json!({ "kind": sv(&b, "kind"), "shared": truthy(&b["shared"]) })),
        "/api/providers/update" if post => ("provider.update", sv(&b, "id"), if b.get("shared").is_some() { json!({ "shared": truthy(&b["shared"]) }) } else { json!({}) }),
        "/api/providers/remove" if post => ("provider.remove", sv(&b, "id"), json!({})),
        "/api/apikey/rotate" if post => ("apikey.new", String::new(), json!({})),
        "/api/apikey/revoke" if post => ("apikey.revoke", String::new(), json!({})),
        "/api/forgejo/connect" if post => ("forgejo.connect", String::new(), json!({})),
        "/api/forgejo/disconnect" if post => ("forgejo.disconnect", String::new(), json!({})),
        "/api/guilds/server/password" if post => ("guild.password", sv(&b, "server"), json!({})),
        "/api/releases/file" if get => ("release.download", r.param("product").unwrap_or_default(), json!({ "tag": r.param("tag").unwrap_or_default(), "asset": r.param("asset").unwrap_or_default() })),
        "/api/native/file" if get => ("release.download", r.param("app").unwrap_or_default(), json!({ "target": r.param("target").unwrap_or_default(), "name": r.param("name").unwrap_or_default() })),
        _ if get && p.starts_with("/api/downloads/") && p.ends_with(".zip") => ("release.download", p.trim_start_matches("/api/downloads/").trim_end_matches(".zip").to_string(), json!({ "zip": true })),
        _ if r.method == Method::DELETE && p.starts_with("/api/machines/") => ("delegation.machineUnlinked", p.trim_start_matches("/api/machines/").to_string(), json!({})),
        _ => return None,
    })
}
/// Does this request need its caller looked up before it runs (a sign-out, a password change: the session may be gone after)?
pub fn watched(r: &Req) -> bool { r.path.starts_with("/api/admin/") || what(r).is_some() }
/// Who is asking (before the request runs).
pub fn actor(hub: &Hub, r: &Req) -> String { hub.mods.auth.session_user(r).unwrap_or_default() }
/// After the answer: the action into the log when it went through; an admin route refused (403) is noted too.
pub fn note(hub: &Hub, r: &Req, actor: &str, status: u16) {
    let ip = hub.mods.auth.outside_ip(r);
    match what(r) {
        Some((action, target, detail)) if (200..300).contains(&status) => {
            let target = if action == "signout" { actor.to_string() } else { target };
            audit::record(actor, action, &target, detail, &ip);
        }
        _ if status == 403 && r.path.starts_with("/api/admin/") && !actor.is_empty() => audit::record(actor, "admin.refused", &r.path, json!({ "method": r.method.as_str() }), &ip),
        _ => {}
    }
}

/// GET /api/admin/audit?since=<seq>&limit=<n>&action=<family>: admins only.
pub fn route(hub: &Hub, r: &Req, user: Option<&str>) -> Routed {
    let Some(u) = user else { return answer(401, json!({ "error": "Not signed in." })) };
    if !hub.mods.auth.account(u).map(|a| a.admin).unwrap_or(false) { return answer(403, json!({ "error": "Admins only." })); }
    let Some(l) = audit::global() else { return answer(503, json!({ "error": "The security log is not open." })) };
    let since = r.param("since").and_then(|x| x.parse().ok()).unwrap_or(0u64);
    let limit = r.param("limit").and_then(|x| x.parse().ok()).unwrap_or(200usize).clamp(1, 1000);
    let action: String = r.param("action").unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.').take(64).collect();
    answer(200, json!({ "entries": l.read(since, limit, &action), "verified": l.verify().to_json(), "file": "security/audit.jsonl" }))
}
