//! Site roles (T-379): who may do what on the hub itself (accounts, the hub, its AI), on the same ladder and model as the
//! chat servers' roles (pgm-hcom-internal perms.rs): Member 10, Moderator 20, Super Moderator 30, Admin 40, Super Admin
//! 50, Co-owner 60, the same names and colours; each role a set of permissions, ADMINISTRATOR = every one of them.
//! - an account's role: its config.json `siteRole` (the default account's: config.local.json, like its admin flag) when it
//!   names a known role; else the hub's default account is Co-owner; else an account with the admin flag is Super Admin
//!   (Sponge, 2026-11-16: the existing admins keep everything); else Member
//! - the admin flag follows the role it is given (the frozen JavaScript hub and every `admin` check keep working): on when
//!   the role holds ADMINISTRATOR or every permission of the builtin Admin
//! - ranking as in the chat: you act on an account only when your role is strictly above its role (or the same role, when
//!   it holds ADMINISTRATOR: a Super Admin acts on another Super Admin) (the default account,
//!   the owner, is above everyone and nobody acts on it); you give, make, change and delete only roles strictly below your
//!   own, and grant only permissions you hold
//! - custom roles in `<root>/site-roles.json` (`c-<random>`, a name, a colour, a position 1..59 between the builtins');
//!   the site-wide settings (message of the day, default theme, new accounts' role) in `<root>/site-settings.json`
use pgm_core_internal::json::{read_json, write_json, write_json_private};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const ADMINISTRATOR: &str = "ADMINISTRATOR";
pub const VIEW_ACCOUNTS: &str = "VIEW_ACCOUNTS";
pub const MANAGE_ACCOUNTS: &str = "MANAGE_ACCOUNTS";
pub const MANAGE_ROLES: &str = "MANAGE_ROLES";
pub const MANAGE_HUB: &str = "MANAGE_HUB";
pub const VIEW_HEALTH: &str = "VIEW_HEALTH";
pub const VIEW_SECURITY_LOG: &str = "VIEW_SECURITY_LOG";
pub const MANAGE_AI: &str = "MANAGE_AI";
pub const VIEW_AI_HOSTS: &str = "VIEW_AI_HOSTS";

pub const CATEGORIES: &[(&str, &[(&str, &str)])] = &[
    ("Accounts", &[(VIEW_ACCOUNTS, "See every account, its status and role"), (MANAGE_ACCOUNTS, "Create, stop and delete accounts; reset passwords, sign people out, unlock devices"), (MANAGE_ROLES, "Give people roles below their own; make, change and delete custom roles below their own")]),
    ("Hub", &[(MANAGE_HUB, "Restart the hub, clear the chat history, PGM Gateway products and the site-wide settings"), (VIEW_HEALTH, "See the hub's health and a run's trace"), (VIEW_SECURITY_LOG, "Read the security log")]),
    ("AI", &[(MANAGE_AI, "AI Admin's global part: the team's OpenRouter (key, models, budgets, personal keys), the default AI for everyone, Unity's own-computer tools per account"), (VIEW_AI_HOSTS, "See every AI host on the hub: owner, state, who uses it")]),
    ("Advanced", &[(ADMINISTRATOR, "Every permission. Give with care.")]),
];
/// Every permission id, in the catalogue's order.
pub fn perm_ids() -> Vec<&'static str> { CATEGORIES.iter().flat_map(|(_, p)| p.iter().map(|x| x.0)).collect() }
/// [[category, [[id, description], …]], …] (what the page lists)
pub fn categories_json() -> Value { json!(CATEGORIES.iter().map(|(n, p)| json!([n, p.iter().map(|(i, d)| json!([i, d])).collect::<Vec<_>>()])).collect::<Vec<_>>()) }
/// A permission in words: MANAGE_AI → "Manage AI", VIEW_SECURITY_LOG → "View security log".
pub fn perm_name(id: &str) -> String {
    id.split('_').enumerate().map(|(i, w)| if w == "AI" { w.to_string() } else if i == 0 { let l = w.to_lowercase(); let mut c = l.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() } else { w.to_lowercase() }).collect::<Vec<_>>().join(" ")
}
/// The refusal for a missing permission, in words.
pub fn needs(perm: &str) -> String { format!("Needs the {} permission.", perm_name(perm)) }
/// A list of permissions cleaned: catalogue ids only (any case), each once, in the catalogue's order.
pub fn clean_perms(v: &Value) -> Vec<String> {
    let given: Vec<String> = v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_uppercase())).collect()).unwrap_or_default();
    perm_ids().into_iter().filter(|p| given.iter().any(|g| g == p)).map(String::from).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Role { pub id: String, pub name: String, pub color: String, pub position: i64, pub perms: Vec<String>, pub builtin: bool }
impl Role {
    /// Does this role hold that permission (ADMINISTRATOR holds them all)?
    pub fn holds(&self, p: &str) -> bool { self.perms.iter().any(|x| x == ADMINISTRATOR || x == p) }
    /// What it holds, ADMINISTRATOR spelled out as every permission.
    pub fn effective(&self) -> Vec<String> { if self.holds(ADMINISTRATOR) { perm_ids().into_iter().map(String::from).collect() } else { self.perms.clone() } }
    /// The admin flag that goes with this role: ADMINISTRATOR, or every permission of the builtin Admin.
    pub fn admin_flag(&self) -> bool { self.holds(ADMINISTRATOR) || admin_perms().iter().all(|p| self.holds(p)) }
    pub fn json(&self) -> Value { json!({ "id": self.id, "name": self.name, "color": self.color, "position": self.position, "perms": self.perms, "builtin": self.builtin }) }
    pub fn brief(&self) -> Value { json!({ "id": self.id, "name": self.name, "color": self.color, "position": self.position }) }
}
/// The builtin Admin's permissions: every one but ADMINISTRATOR.
fn admin_perms() -> Vec<&'static str> { perm_ids().into_iter().filter(|p| *p != ADMINISTRATOR).collect() }
/// The ladder (fixed, not editable): the chat's names, colours and positions.
pub fn builtins() -> Vec<Role> {
    let r = |id: &str, name: &str, color: &str, position: i64, perms: Vec<&str>| Role { id: id.into(), name: name.into(), color: color.into(), position, perms: perms.into_iter().map(String::from).collect(), builtin: true };
    vec![
        r("member", "Member", "#2ecc71", 10, vec![]),
        r("moderator", "Moderator", "#3498db", 20, vec![VIEW_ACCOUNTS]),
        r("supermod", "Super Moderator", "#1abc9c", 30, vec![VIEW_ACCOUNTS, VIEW_HEALTH, VIEW_SECURITY_LOG, VIEW_AI_HOSTS]),
        r("admin", "Admin", "#e67e22", 40, admin_perms()),
        r("superadmin", "Super Admin", "#e74c3c", 50, vec![ADMINISTRATOR]),
        r("coowner", "Co-owner", "#f1c40f", 60, vec![ADMINISTRATOR]),
    ]
}
fn builtin(id: &str) -> Role { builtins().into_iter().find(|r| r.id == id).expect("builtin role") }
/// The hub's own account (the owner) ranks above every role.
pub const OWNER_RANK: i64 = i64::MAX;
/// Whether someone of rank `actor` may give / save / delete a role at `position`: strictly below only.
pub fn below(actor: i64, position: i64) -> bool { actor > position }
/// Whether someone holding `mine` (effective) may grant `perms`.
pub fn grantable(mine: &[String], perms: &[String]) -> bool { perms.iter().all(|p| mine.contains(p)) }

/// A custom role from a request ({name, color, position, perms}), checked; `id` = the role it replaces (else a new id).
pub fn custom_from(id: Option<&str>, b: &Value) -> Result<Role, String> {
    let name: String = b["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() || name.chars().count() > 32 { return Err("Give the role a name (at most 32 characters).".into()); }
    let color = b["color"].as_str().unwrap_or("").trim().to_lowercase();
    if color.len() != 7 || !color.starts_with('#') || !color[1..].bytes().all(|c| c.is_ascii_hexdigit()) { return Err("The colour is #rrggbb.".into()); }
    let position = b["position"].as_i64().unwrap_or(0);
    if !(1..=59).contains(&position) || builtins().iter().any(|r| r.position == position) { return Err("The position is a whole number from 1 to 59, not one of the builtin roles' (10, 20, 30, 40, 50).".into()); }
    if b["perms"].as_array().is_some_and(|a| a.iter().any(|p| !p.as_str().is_some_and(|s| perm_ids().contains(&s.trim().to_uppercase().as_str())))) { return Err("perms lists permissions of the catalogue only.".into()); }
    let id = match id { Some(i) => i.to_string(), None => format!("c-{}", pgm_core_internal::crypto::random_hex(4)) };
    Ok(Role { id, name, color, position, perms: clean_perms(&b["perms"]), builtin: false })
}

/// The site's settings as kept, with their defaults.
pub fn settings(root: &Path) -> Value {
    let v = read_json(&root.join("site-settings.json")).filter(Value::is_object).unwrap_or(json!({}));
    let s = |k: &str, d: &str| v[k].as_str().map(String::from).unwrap_or_else(|| d.to_string());
    json!({ "motd": s("motd", ""), "defaultTheme": s("defaultTheme", ""), "newAccountRole": s("newAccountRole", "member") })
}
/// New settings from a request over the old ones (fields left out stay): checked, not yet saved. The role named is
/// checked by the caller (it must exist and be below them).
pub fn settings_from(old: &Value, b: &Value) -> Result<Value, String> {
    let mut v = old.clone();
    if let Some(m) = b.get("motd").filter(|x| !x.is_null()) { let t = m.as_str().ok_or("motd is text.")?.trim().to_string(); if t.chars().count() > 500 { return Err("The message of the day is at most 500 characters.".into()); } v["motd"] = json!(t); }
    if let Some(t) = b.get("defaultTheme").filter(|x| !x.is_null()) { let t = t.as_str().ok_or("defaultTheme is text.")?.trim().to_string(); if t.len() > 32 || !t.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_') { return Err("defaultTheme is a theme id (lowercase letters, digits, dashes, underscores) or \"\".".into()); } v["defaultTheme"] = json!(t); }
    if let Some(r) = b.get("newAccountRole").filter(|x| !x.is_null()) { let r = r.as_str().ok_or("newAccountRole is a role id.")?.trim().to_string(); v["newAccountRole"] = json!(if r.is_empty() { "member".to_string() } else { r }); }
    Ok(v)
}
pub fn save_settings(root: &Path, v: &Value) { write_json(&root.join("site-settings.json"), v) }

/// One hub's site roles: its custom roles and who has which.
pub struct SiteRoles { root: PathBuf, default_user: String }
impl SiteRoles {
    pub fn new(root: &Path, default_user: &str) -> SiteRoles { SiteRoles { root: root.to_path_buf(), default_user: default_user.to_string() } }
    fn file(&self) -> PathBuf { self.root.join("site-roles.json") }
    /// The custom roles as kept (a broken entry is left out).
    pub fn customs(&self) -> Vec<Role> {
        read_json(&self.file()).and_then(|v| v["roles"].as_array().cloned()).unwrap_or_default().iter()
            .filter_map(|r| r["id"].as_str().filter(|i| i.starts_with("c-")).and_then(|i| custom_from(Some(i), r).ok())).collect()
    }
    /// Every role, highest first.
    pub fn all(&self) -> Vec<Role> { let mut v = builtins(); v.extend(self.customs()); v.sort_by(|a, b| b.position.cmp(&a.position)); v }
    pub fn get(&self, id: &str) -> Option<Role> { self.all().into_iter().find(|r| r.id == id) }
    pub fn is_owner(&self, user: &str) -> bool { !user.is_empty() && user == self.default_user }
    /// Where an account's siteRole and admin flag are written: a profile's config.json; the default account's config.local.json.
    fn config_file(&self, user: &str) -> Option<PathBuf> {
        if self.is_owner(user) { return Some(self.root.join("config.local.json")); }
        if !crate::valid_username(user) { return None; }
        let d = self.root.join("profiles").join(user);
        d.is_dir().then(|| d.join("config.json"))
    }
    /// The role an account's config names (what is stored, known or not).
    pub fn assigned(&self, user: &str) -> Option<String> {
        let mut files = vec![self.config_file(user)?];
        if self.is_owner(user) { files.push(self.root.join("config.json")); }
        files.iter().find_map(|f| read_json(f).and_then(|c| c["siteRole"].as_str().map(String::from)))
    }
    /// An account's role: what it was given (a known role), else the owner Co-owner, an admin Super Admin, anyone Member.
    pub fn role_of(&self, user: &str) -> Role {
        if let Some(r) = self.assigned(user).and_then(|id| self.get(&id)) { return r; }
        if self.is_owner(user) { return builtin("coowner"); }
        if crate::admin_in(&self.root, &self.default_user, user) { return builtin("superadmin"); }
        builtin("member")
    }
    /// How high an account stands (the owner above every role).
    pub fn rank_of(&self, user: &str) -> i64 { if self.is_owner(user) { OWNER_RANK } else { self.role_of(user).position } }
    pub fn perms_of(&self, user: &str) -> Vec<String> { if user.is_empty() { vec![] } else { self.role_of(user).effective() } }
    pub fn has(&self, user: &str, perm: &str) -> bool { !user.is_empty() && self.role_of(user).holds(perm) }
    /// May `actor` act on `target`'s account (stop, password, sign out, delete, role)? Yourself: `self_ok`.
    pub fn may_act_on(&self, actor: &str, target: &str, self_ok: bool) -> Result<(), String> {
        if actor == target { return if self_ok { Ok(()) } else { Err("Not on your own account: another admin can.".into()) }; }
        if self.is_owner(target) { return Err(format!("Nobody acts on the hub's own account ({target}).")); }
        if below(self.rank_of(actor), self.rank_of(target)) { return Ok(()); }
        // a role holding ADMINISTRATOR (Super Admin, Co-owner) also acts on its own rank: a locked-out Super Admin is
        // never stuck waiting on the owner
        if self.has(actor, ADMINISTRATOR) && self.rank_of(actor) == self.rank_of(target) { return Ok(()); }
        Err(format!("{target} is {} and you are {}: you act only on accounts below your own role.", self.role_of(target).name, self.role_of(actor).name))
    }
    /// May `actor` give / save / delete `role` (strictly below their own, only permissions they hold)?
    pub fn may_handle(&self, actor: &str, role: &Role) -> Result<(), String> {
        if !below(self.rank_of(actor), role.position) { return Err(format!("{} is not below your own role ({}).", role.name, self.role_of(actor).name)); }
        if !grantable(&self.perms_of(actor), &role.effective()) { return Err(format!("{} holds permissions you do not hold.", role.name)); }
        Ok(())
    }
    /// Who has which role (every account named, its effective role id).
    pub fn members(&self, users: &[String]) -> serde_json::Map<String, Value> { users.iter().map(|u| (u.clone(), json!(self.role_of(u).id))).collect() }
    /// Writes an account's role (None = cleared: back to Member) and the admin flag that goes with it, keeping the file's
    /// permissions (the default account's config.local.json stays private). The admin flag as written.
    pub fn assign(&self, user: &str, role: Option<&Role>) -> Result<bool, String> {
        let f = self.config_file(user).ok_or("No such account.")?;
        let mode = std::fs::metadata(&f).ok().map(|m| m.permissions());
        let mut c = read_json(&f).filter(Value::is_object).unwrap_or_else(|| json!({}));
        let admin = role.map(Role::admin_flag).unwrap_or(false);
        if let Some(m) = c.as_object_mut() {
            match role { Some(r) => { m.insert("siteRole".into(), json!(r.id)); } None => { m.remove("siteRole"); } }
            if admin { m.insert("admin".into(), json!(true)); } else { m.remove("admin"); }
        }
        if self.is_owner(user) { write_json_private(&f, &c); } else { write_json(&f, &c); }
        if let Some(m) = mode { let _ = std::fs::set_permissions(&f, m); }
        Ok(admin)
    }
    /// Saves a custom role (new or replacing the one with its id).
    pub fn save(&self, role: &Role) -> Result<Role, String> {
        if role.builtin || !role.id.starts_with("c-") { return Err("The builtin roles cannot be changed.".into()); }
        let mut list = self.customs();
        match list.iter_mut().find(|r| r.id == role.id) { Some(r) => *r = role.clone(), None => list.push(role.clone()) }
        write_json(&self.file(), &json!({ "roles": list.iter().map(Role::json).collect::<Vec<_>>() }));
        Ok(role.clone())
    }
    /// Deletes a custom role; the accounts that had it (of `users`) are cleared (Member, admin flag off). Who they were.
    pub fn delete(&self, id: &str, users: &[String]) -> Result<Vec<String>, String> {
        let mut list = self.customs();
        let n = list.len(); list.retain(|r| r.id != id);
        if list.len() == n { return Err("No such custom role.".into()); }
        write_json(&self.file(), &json!({ "roles": list.iter().map(Role::json).collect::<Vec<_>>() }));
        let had: Vec<String> = users.iter().filter(|u| self.assigned(u).as_deref() == Some(id)).cloned().collect();
        for u in &had { let _ = self.assign(u, None); }
        Ok(had)
    }
}
/// Does `user` hold `perm` on the hub at `root` (for the libraries that only know the hub's folder)?
pub fn has_perm(root: &Path, default_user: &str, user: &str, perm: &str) -> bool { SiteRoles::new(root, default_user).has(user, perm) }

#[cfg(test)]
mod tests {
    use super::*;
    fn hub() -> (PathBuf, SiteRoles) {
        let d = std::env::temp_dir().join(format!("siteroles-{}", pgm_core_internal::crypto::random_hex(6)));
        for (u, admin) in [("ann", true), ("bob", false), ("cid", false)] { std::fs::create_dir_all(d.join("profiles").join(u)).unwrap(); write_json(&d.join("profiles").join(u).join("config.json"), &if admin { json!({ "user": u, "admin": true }) } else { json!({ "user": u }) }); }
        write_json(&d.join("config.json"), &json!({ "admin": true }));
        let s = SiteRoles::new(&d, "owner"); (d, s)
    }
    #[test]
    fn the_ladder_and_the_catalogue() {
        let b = builtins();
        assert_eq!(b.iter().map(|r| (r.id.as_str(), r.position)).collect::<Vec<_>>(), vec![("member", 10), ("moderator", 20), ("supermod", 30), ("admin", 40), ("superadmin", 50), ("coowner", 60)]);
        assert!(!builtin("admin").holds(ADMINISTRATOR) && builtin("admin").admin_flag() && builtin("superadmin").admin_flag() && !builtin("supermod").admin_flag());
        assert_eq!(builtin("superadmin").effective().len(), perm_ids().len());
        assert_eq!(perm_name(MANAGE_AI), "Manage AI"); assert_eq!(perm_name(VIEW_AI_HOSTS), "View AI hosts"); assert_eq!(needs(VIEW_SECURITY_LOG), "Needs the View security log permission.");
        assert_eq!(clean_perms(&json!(["manage_ai", "NOPE", "VIEW_ACCOUNTS", "MANAGE_AI"])), vec!["VIEW_ACCOUNTS", "MANAGE_AI"]);
    }
    #[test]
    fn who_is_what_without_a_role() {
        let (d, s) = hub();
        assert_eq!(s.role_of("owner").id, "coowner"); assert_eq!(s.rank_of("owner"), OWNER_RANK);
        assert_eq!(s.role_of("ann").id, "superadmin"); assert_eq!(s.role_of("bob").id, "member");
        assert!(s.has("ann", MANAGE_AI) && !s.has("bob", VIEW_ACCOUNTS) && !s.has("", VIEW_ACCOUNTS));
        let _ = std::fs::remove_dir_all(d);
    }
    #[test]
    fn ranks_and_assigning() {
        let (d, s) = hub();
        assert!(s.may_act_on("ann", "bob", false).is_ok() && s.may_act_on("bob", "ann", false).is_err() && s.may_act_on("ann", "owner", false).is_err() && s.may_act_on("owner", "ann", false).is_ok());
        assert!(s.may_act_on("ann", "ann", false).is_err() && s.may_act_on("ann", "ann", true).is_ok());
        // a Super Admin acts on another Super Admin; an Admin never on another Admin
        s.assign("cid", Some(&builtin("superadmin"))).unwrap();
        assert!(s.may_act_on("ann", "cid", false).is_ok() && s.may_act_on("cid", "ann", false).is_ok() && s.may_act_on("cid", "owner", false).is_err());
        s.assign("cid", Some(&builtin("admin"))).unwrap(); s.assign("bob", Some(&builtin("admin"))).unwrap();
        assert!(s.may_act_on("bob", "cid", false).is_err());
        s.assign("bob", None).unwrap(); s.assign("cid", None).unwrap();
        assert!(s.may_handle("ann", &builtin("admin")).is_ok() && s.may_handle("ann", &builtin("superadmin")).is_err() && s.may_handle("owner", &builtin("coowner")).is_ok());
        assert_eq!(s.assign("bob", Some(&builtin("admin"))), Ok(true));
        assert_eq!(s.role_of("bob").id, "admin"); assert!(crate::admin_in(&d, "owner", "bob"));
        assert!(s.may_handle("bob", &builtin("moderator")).is_ok() && s.may_handle("bob", &builtin("admin")).is_err());
        assert_eq!(s.assign("bob", Some(&builtin("moderator"))), Ok(false));
        assert!(!crate::admin_in(&d, "owner", "bob") && s.has("bob", VIEW_ACCOUNTS) && !s.has("bob", MANAGE_ACCOUNTS));
        let _ = std::fs::remove_dir_all(d);
    }
    #[test]
    fn custom_roles() {
        let (d, s) = hub();
        assert!(custom_from(None, &json!({ "name": "x", "color": "#00ff00", "position": 40 })).is_err());
        assert!(custom_from(None, &json!({ "name": "x", "color": "green", "position": 35 })).is_err());
        assert!(custom_from(None, &json!({ "name": "x", "color": "#00ff00", "position": 35, "perms": ["FLY"] })).is_err());
        let r = custom_from(None, &json!({ "name": "AI keeper", "color": "#00FF00", "position": 35, "perms": ["MANAGE_AI"] })).unwrap();
        assert!(r.id.starts_with("c-") && r.color == "#00ff00" && !r.admin_flag());
        s.save(&r).unwrap();
        assert_eq!(s.get(&r.id).unwrap().perms, vec!["MANAGE_AI"]);
        s.assign("cid", Some(&r)).unwrap();
        assert!(s.has("cid", MANAGE_AI) && !s.has("cid", VIEW_ACCOUNTS));
        // an Admin (every permission but ADMINISTRATOR) cannot grant ADMINISTRATOR; a Super Admin can
        s.assign("bob", Some(&builtin("admin"))).unwrap();
        let big = custom_from(None, &json!({ "name": "big", "color": "#000000", "position": 35, "perms": ["ADMINISTRATOR"] })).unwrap();
        assert!(s.may_handle("bob", &big).is_err() && s.may_handle("ann", &big).is_ok());
        assert_eq!(s.delete(&r.id, &["bob".into(), "cid".into()]).unwrap(), vec!["cid".to_string()]);
        assert_eq!(s.role_of("cid").id, "member"); assert!(s.assigned("cid").is_none());
        let _ = std::fs::remove_dir_all(d);
    }
    #[test]
    fn settings_are_checked() {
        let old = json!({ "motd": "", "defaultTheme": "", "newAccountRole": "member" });
        assert_eq!(settings_from(&old, &json!({ "motd": " hi ", "defaultTheme": "dark-blue" })).unwrap(), json!({ "motd": "hi", "defaultTheme": "dark-blue", "newAccountRole": "member" }));
        assert!(settings_from(&old, &json!({ "motd": "x".repeat(501) })).is_err() && settings_from(&old, &json!({ "defaultTheme": "Bad Theme" })).is_err());
    }
}
