//! Site admin and AI Admin (T-379): the hub's own roles and permissions (pgm-identity-internal siteroles: the chat
//! servers' ladder and model), the site-wide settings, and AI Admin's global part (the default AI for everyone, every AI
//! host on the hub, Unity's own-computer tools per account). Every refusal names the permission that is missing.
//! - GET  /api/admin/roles (View accounts or Manage roles) → {categories, roles, members, me: {role, position, owner, perms}}
//!   (the owner, the hub's own account, stands above every position)
//! - POST /api/admin/roles/assign {username, role} (Manage roles; below you, and them below you) → {username, role, admin}
//! - POST /api/admin/roles/save {id?, name, color, position, perms} (Manage roles; custom roles below you) → {role}
//! - POST /api/admin/roles/delete {id} (Manage roles) → {ok, cleared}: its people back to Member
//! - GET / POST /api/admin/site-settings (Manage hub) → {motd, defaultTheme, newAccountRole}
//! - GET  /api/admin/ai (Manage AI or View AI hosts) → {defaultProvider, choices, hubClaude, hosts, canManage, unity}
//! - POST /api/admin/ai/default {defaultProvider} (Manage AI): saved in config.local.json, counts at once
use pgm_identity_internal::siteroles::{self as sr, Role};
use serde_json::{json, Value};
use std::sync::Arc;

use super::auth::Account;
use super::{answer, Routed};
use crate::front::{Hub, Req};

/// None when `u` holds `perm`; else the 403 that names it.
pub fn need(hub: &Hub, u: &str, perm: &str) -> Option<Routed> { if hub.mods.auth.may(u, perm) { None } else { Some(answer(403, json!({ "error": sr::needs(perm) }))) } }
/// None when `actor` may act on `target`'s account (`self_ok`: their own counts); else the 403 that says why.
pub fn may_act(hub: &Hub, actor: &str, target: &str, self_ok: bool) -> Option<Routed> { hub.mods.auth.site_roles().may_act_on(actor, target, self_ok).err().map(|e| answer(403, json!({ "error": e }))) }

/// /api/check's `site` (add-only): the person's role and permissions, and what the site says to everyone.
pub fn check_field(hub: &Hub, a: &Account) -> Value {
    let s = hub.mods.auth.site_roles(); let role = s.role_of(&a.username); let st = sr::settings(&hub.root);
    json!({ "role": role.brief(), "perms": role.effective(), "motd": st["motd"], "defaultTheme": st["defaultTheme"] })
}

/// Gives `user` that role (writing siteRole and the admin flag that goes with it), the JavaScript hub told.
pub async fn give(hub: &Hub, actor: &str, user: &str, role: &Role) -> Result<bool, String> {
    let admin = hub.mods.auth.site_roles().assign(user, Some(role))?;
    println!("[vr-hub] {actor} gave \"{user}\" the role {} (admin {admin}).", role.name);
    super::admin::accounts_changed(hub).await;
    Ok(admin)
}

/// The AI choices for the default AI: "" (the built-in order) and each person whose AI host has a Claude login.
fn ai_choices(hub: &Hub) -> Vec<Value> {
    let mut v = vec![json!({ "value": "", "label": "The hub's built-in order: this hub's Claude, else the first usable AI host" })];
    for o in hub.mods.ai.claude_owners() { v.push(json!({ "value": format!("host:{o}"), "label": format!("{}'s Claude (AI host)", hub.mods.name_of(&o)) })); }
    v
}
/// Saves the default AI in the hub's config.local.json (its permissions kept; "" removes the key) and uses it at once.
fn set_default_ai(hub: &Hub, v: &str) {
    let f = hub.root.join("config.local.json");
    let mode = std::fs::metadata(&f).ok().map(|m| m.permissions());
    let mut c = pgm_core_internal::json::read_json(&f).filter(Value::is_object).unwrap_or_else(|| json!({}));
    if let Some(m) = c.as_object_mut() { if v.is_empty() { m.remove("defaultProvider"); } else { m.insert("defaultProvider".into(), json!(v)); } }
    pgm_core_internal::json::write_json_private(&f, &c);
    if let Some(m) = mode { let _ = std::fs::set_permissions(&f, m); }
    hub.mods.work.set_default_setting(v);
}

pub async fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> {
    let p = r.path.as_str();
    if !matches!(p, "/api/admin/roles" | "/api/admin/roles/assign" | "/api/admin/roles/save" | "/api/admin/roles/delete" | "/api/admin/site-settings" | "/api/admin/ai" | "/api/admin/ai/default") { return None; }
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let au = &hub.mods.auth; let s = au.site_roles(); let me = a.username.as_str(); let b = r.json();
    let users = || au.accounts().into_iter().map(|x| x.username).collect::<Vec<_>>();
    Some(match p {
        "/api/admin/roles" if get => {
            if !au.may(me, sr::VIEW_ACCOUNTS) && !au.may(me, sr::MANAGE_ROLES) { return Some(answer(403, json!({ "error": "Needs the View accounts or the Manage roles permission." }))); }
            let mine = s.role_of(me);
            answer(200, json!({ "categories": sr::categories_json(), "roles": s.all().iter().map(Role::json).collect::<Vec<_>>(), "members": s.members(&users()),
                "me": { "role": mine.id, "position": mine.position, "owner": s.is_owner(me), "perms": mine.effective() } }))
        }
        "/api/admin/roles/assign" if post => {
            if let Some(x) = need(hub, me, sr::MANAGE_ROLES) { return Some(x); }
            let Some(t) = b["username"].as_str().and_then(|u| au.account(u)) else { return Some(answer(400, json!({ "error": "No such account." }))) };
            let Some(role) = b["role"].as_str().and_then(|id| s.get(id)) else { return Some(answer(400, json!({ "error": "No such role." }))) };
            if let Some(x) = may_act(hub, me, &t.username, false) { return Some(x); }
            if let Err(e) = s.may_handle(me, &role) { return Some(answer(403, json!({ "error": e }))); }
            match give(hub, me, &t.username, &role).await { Ok(admin) => answer(200, json!({ "username": t.username, "role": role.id, "admin": admin })), Err(e) => answer(400, json!({ "error": e })) }
        }
        "/api/admin/roles/save" if post => {
            if let Some(x) = need(hub, me, sr::MANAGE_ROLES) { return Some(x); }
            let id = b["id"].as_str().filter(|i| !i.is_empty());
            let old = match id { Some(i) => match s.get(i) { Some(o) if o.builtin => return Some(answer(400, json!({ "error": "The builtin roles cannot be changed." }))), Some(o) => Some(o), None => return Some(answer(400, json!({ "error": "No such role." }))) }, None => None };
            let role = match sr::custom_from(id, &b) { Ok(x) => x, Err(e) => return Some(answer(400, json!({ "error": e }))) };
            for x in old.iter().chain(std::iter::once(&role)) { if let Err(e) = s.may_handle(me, x) { return Some(answer(403, json!({ "error": e }))); } }
            match s.save(&role) { Ok(x) => { println!("[vr-hub] {me} saved the role {} ({}).", x.name, x.id); super::admin::accounts_changed(hub).await; answer(200, json!({ "role": x.json() })) } Err(e) => answer(400, json!({ "error": e })) }
        }
        "/api/admin/roles/delete" if post => {
            if let Some(x) = need(hub, me, sr::MANAGE_ROLES) { return Some(x); }
            let Some(role) = b["id"].as_str().and_then(|i| s.get(i)) else { return Some(answer(400, json!({ "error": "No such role." }))) };
            if role.builtin { return Some(answer(400, json!({ "error": "The builtin roles cannot be deleted." }))); }
            if let Err(e) = s.may_handle(me, &role) { return Some(answer(403, json!({ "error": e }))); }
            match s.delete(&role.id, &users()) { Ok(cleared) => { println!("[vr-hub] {me} deleted the role {} ({} back to Member).", role.name, cleared.len()); super::admin::accounts_changed(hub).await; answer(200, json!({ "ok": true, "cleared": cleared })) } Err(e) => answer(400, json!({ "error": e })) }
        }
        "/api/admin/site-settings" if get || post => {
            if let Some(x) = need(hub, me, sr::MANAGE_HUB) { return Some(x); }
            if get { return Some(answer(200, sr::settings(&hub.root))); }
            let v = match sr::settings_from(&sr::settings(&hub.root), &b) { Ok(v) => v, Err(e) => return Some(answer(400, json!({ "error": e }))) };
            if b.get("newAccountRole").is_some_and(|x| !x.is_null()) {
                let Some(role) = v["newAccountRole"].as_str().and_then(|i| s.get(i)) else { return Some(answer(400, json!({ "error": "newAccountRole names no role." }))) };
                if let Err(e) = s.may_handle(me, &role) { return Some(answer(403, json!({ "error": e }))); }
            }
            sr::save_settings(&hub.root, &v);
            answer(200, v)
        }
        "/api/admin/ai" if get => {
            let manage = au.may(me, sr::MANAGE_AI);
            if !manage && !au.may(me, sr::VIEW_AI_HOSTS) { return Some(answer(403, json!({ "error": "Needs the Manage AI or the View AI hosts permission." }))); }
            // Unity's own-computer tools per account (what /api/admin/accounts says as unityPc / unityCode): Manage AI only
            let unity: Vec<Value> = if manage { au.accounts().iter().map(|x| json!({ "username": x.username, "display": x.display, "pc": hub.mods.unity.group_on(x, "pc"), "code": hub.mods.unity.group_on(x, "code") })).collect() } else { vec![] };
            answer(200, json!({ "defaultProvider": hub.mods.work.default_setting(), "choices": ai_choices(hub), "hubClaude": hub.cfg.get("hubClaude") != Some(&json!(false)), "hosts": hub.mods.ai.overview(), "canManage": manage, "unity": unity }))
        }
        "/api/admin/ai/default" if post => {
            if let Some(x) = need(hub, me, sr::MANAGE_AI) { return Some(x); }
            let v = b["defaultProvider"].as_str().unwrap_or("").trim().to_string();
            if !ai_choices(hub).iter().any(|c| c["value"] == json!(v)) { return Some(answer(400, json!({ "error": "That is not one of the choices: \"\" (the built-in order) or \"host:<owner>\" of someone whose AI host has a Claude login." }))); }
            set_default_ai(hub, &v);
            println!("[vr-hub] {me} set the default AI to {}.", if v.is_empty() { "the built-in order" } else { &v });
            answer(200, json!({ "defaultProvider": v }))
        }
        _ => answer(405, json!({ "error": "Method not allowed." })),
    })
}
