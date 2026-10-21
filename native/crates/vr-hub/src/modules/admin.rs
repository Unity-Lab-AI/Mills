//! The admin's accounts (moved from bridge/server.js): the list, making an account (with a first password), deleting
//! one (kept in profiles-deleted/, never destroyed), asking for a restart; and everyone's own Forgejo token ("My
//! Forgejo": checked against Forgejo, kept on the host in bridge/forgejo-users.json, never sent back).
use serde_json::{json, Value};
use std::sync::Arc;

use super::auth::Account;
use super::{answer, Routed};
use crate::front::{Hub, Req};

fn summary(hub: &Hub, a: &Account) -> Value {
    let au = &hub.mods.auth; let lock = au.lock_of(&a.username); let u = &a.username;
    let busy = hub.mods.work.busy(u) || hub.mods.unity.runs.lock().unwrap().contains_key(u);
    let (devices, allow_next) = lock.as_ref().map(|l| (super::auth::lock_devices(l).len(), l["allowNext"] == json!(true))).unwrap_or((0, false));
    json!({ "username": u, "display": a.display, "admin": a.admin, "locked": lock.is_some(), "lockedAt": lock.map(|l| l["at"].clone()).unwrap_or(Value::Null), "devices": devices, "allowNext": allow_next, "signedIn": au.signed_in(u), "busy": busy,
        "model": hub.mods.work.effective_model(a), "chatKey": hub.mods.chat.has_key(u), "hasPassword": a.dir.join("auth.json").exists(), "unityPc": hub.mods.unity.group_on(a, "pc"), "unityCode": hub.mods.unity.group_on(a, "code") })
}
fn forgejo_url(hub: &Hub) -> String { std::fs::read_to_string(hub.root.join("forgejo.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|c| c["url"].as_str().map(String::from)).or_else(|| hub.cfg.get("forgejoUrl").and_then(Value::as_str).map(String::from)).unwrap_or_else(|| "https://git.unityailab.com".into()).trim_end_matches('/').to_string() }
fn users_file(hub: &Hub) -> std::path::PathBuf { hub.root.join("forgejo-users.json") }
fn users(hub: &Hub) -> Value { std::fs::read_to_string(users_file(hub)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(|v| v["users"].is_object()).unwrap_or(json!({ "users": {} })) }
fn me(hub: &Hub, u: &str) -> Value {
    let cfg: Value = std::fs::read_to_string(hub.root.join("forgejo.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let r = users(hub)["users"][u].clone();
    json!({ "url": forgejo_url(hub), "connected": r.is_object(), "login": r["login"].as_str().unwrap_or(""), "savedAt": r["savedAt"].as_str().unwrap_or(""),
        "mirror": if cfg["repo"].is_string() { json!({ "repo": cfg["repo"], "board": cfg["board"].as_str().unwrap_or("team"), "poll": cfg["token"].as_str().map(|t| !t.is_empty()).unwrap_or(false) }) } else { Value::Null } })
}

pub async fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> {
    let post = r.method == hyper::Method::POST; let get = r.method == hyper::Method::GET;
    let p = r.path.as_str();
    match p {
        "/api/forgejo/me" if get => return Some(answer(200, me(hub, &a.username))),
        "/api/forgejo/disconnect" if post => { let mut v = users(hub); if v["users"].as_object_mut().map(|m| m.remove(&a.username).is_some()).unwrap_or(false) { super::unity::write_json(&users_file(hub), &v); vr_core::settings::private(&users_file(hub)); } return Some(answer(200, me(hub, &a.username))); }
        "/api/forgejo/connect" if post => {
            let token = r.json()["token"].as_str().unwrap_or("").trim().to_string();
            let url = forgejo_url(hub);
            if !(8..=200).contains(&token.len()) || !token.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) { return Some(answer(400, json!({ "error": format!("Paste a Forgejo access token (Settings → Applications on {url}).") }))); }
            let http = vr_core::hub::http_client(std::time::Duration::from_secs(10)).expect("http");
            let res = tokio::time::timeout(std::time::Duration::from_secs(15), http.get(format!("{url}/api/v1/user")).header("authorization", format!("token {token}")).header("accept", "application/json").send()).await;
            let login = match res {
                Ok(Ok(resp)) if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 => return Some(answer(400, json!({ "error": "Forgejo did not accept that token." }))),
                Ok(Ok(resp)) if !resp.status().is_success() => return Some(answer(400, json!({ "error": format!("Forgejo answered {}.", resp.status().as_u16()) }))),
                Ok(Ok(resp)) => resp.json::<Value>().await.ok().and_then(|j| j["login"].as_str().map(String::from)),
                Ok(Err(e)) => return Some(answer(400, json!({ "error": e.to_string() }))),
                Err(_) => return Some(answer(400, json!({ "error": "Forgejo did not answer in time." }))),
            };
            let Some(login) = login.filter(|l| !l.is_empty()) else { return Some(answer(400, json!({ "error": "Forgejo did not say whose token that is." }))) };
            let mut v = users(hub); v["users"][&a.username] = json!({ "token": token, "login": login, "savedAt": super::auth::iso_now() });
            super::unity::write_json(&users_file(hub), &v); vr_core::settings::private(&users_file(hub));
            return Some(answer(200, me(hub, &a.username)));
        }
        _ => {}
    }
    if !p.starts_with("/api/admin/") || !matches!(p, "/api/admin/accounts" | "/api/admin/create" | "/api/admin/delete" | "/api/admin/restart" | "/api/admin/stop") { return None; }
    if !a.admin { return Some(answer(403, json!({ "error": "Admins only." }))); }
    let au = &hub.mods.auth; let b = r.json();
    let target = || b["username"].as_str().and_then(|u| au.account(u));
    Some(match p {
        "/api/admin/accounts" if get => answer(200, json!({ "accounts": au.accounts().iter().map(|x| summary(hub, x)).collect::<Vec<_>>(), "me": a.username })),
        "/api/admin/create" if post => {
            let u = b["username"].as_str().unwrap_or("").trim().to_lowercase();
            if !super::auth::valid_username(&u) { return Some(answer(400, json!({ "error": "Usernames use lowercase letters, digits and dashes (max 32)." }))); }
            let dir = hub.root.join("profiles").join(&u);
            if au.account(&u).is_some() || dir.exists() { return Some(answer(400, json!({ "error": "That username is taken." }))); }
            let _ = std::fs::create_dir_all(&dir);
            let display = { let d: String = b["display"].as_str().unwrap_or("").trim().chars().take(40).collect(); if d.is_empty() { let mut x = u.clone(); if let Some(f) = x.get_mut(0..1) { f.make_ascii_uppercase(); } x } else { d } };
            let mut cfg = json!({ "user": display }); if super::unity::truthy(&b["admin"]) { cfg["admin"] = json!(true); }
            super::unity::write_json(&dir.join("config.json"), &cfg);
            let pw = super::auth::new_password();
            super::unity::write_json(&dir.join("auth.json"), &super::auth::hash_password(&pw)); vr_core::settings::private(&dir.join("auth.json"));
            println!("[vr-hub] {} created account \"{u}\".", a.username);
            accounts_changed(hub).await;
            answer(200, json!({ "username": u, "password": pw }))
        }
        "/api/admin/stop" if post => { let Some(t) = target() else { return Some(answer(400, json!({ "error": "No such account." }))) }; hub.mods.work.stop(hub, &t.username); hub.mods.api.stop(&t.username); hub.mods.unity.stop(&t.username); answer(200, json!({ "ok": true })) }
        "/api/admin/delete" if post => {
            let Some(t) = target() else { return Some(answer(400, json!({ "error": "No such account." }))) };
            if t.username == a.username { return Some(answer(400, json!({ "error": "You cannot delete your own account." }))); }
            if t.dir == hub.root { return Some(answer(400, json!({ "error": format!("The \"{}\" account lives in bridge/ itself and cannot be deleted here.", t.username) }))); }
            let (w, c) = (&hub.mods.work, &hub.mods.chat); let u = &t.username;
            w.stop(hub, u); hub.mods.unity.stop(u); au.drop_sessions(u); au.unlock(u); w.share_stop(u); w.forget(u); super::chat::calls_leave(hub, u);
            let us: Vec<(String, String, bool)> = au.accounts().into_iter().map(|x| (x.username, x.display, x.admin)).collect();
            c.guilds.drop_user(u, &super::guilds::Ctx { users: &us, seq: c.seq(), msg_conv: &|n| c.msg_conv(n), delete_recent: &|_, _, _| 0 });
            for e in ["webp", "png", "jpg"] { let _ = std::fs::remove_file(hub.mods.mills_dir().join("Avatars").join(format!("{u}.{e}"))); }
            let gone = hub.root.join("profiles-deleted"); let _ = std::fs::create_dir_all(&gone);
            if let Err(e) = std::fs::rename(&t.dir, gone.join(format!("{u}-{}", vr_core::now_ms()))) { return Some(answer(500, json!({ "error": format!("Could not move the account away: {e}") }))); }
            c.drop_key(u); c.keys_changed();
            println!("[vr-hub] {} deleted account \"{u}\" (moved to bridge/profiles-deleted/).", a.username);
            accounts_changed(hub).await;
            answer(200, json!({ "ok": true }))
        }
        "/api/admin/restart" if post => { let _ = std::fs::write(hub.root.join(".restart"), format!("{}\n", super::auth::iso_now())); answer(200, json!({ "ok": true, "supervised": std::env::var("MILLS_SUPERVISED").is_ok() })) }
        _ => answer(404, json!({ "error": "Unknown admin endpoint" })),
    })
}
/// The JavaScript hub (still answering a few routes) reads the accounts again.
async fn accounts_changed(hub: &Hub) { crate::front::internal(hub, "/internal/front/accounts-changed", "{}".into()).await; }
