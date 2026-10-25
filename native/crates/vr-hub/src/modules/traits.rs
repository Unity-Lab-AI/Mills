//! What the PGM libraries need from the hub, said by the hub: [`ChatHost`] (pgm-hcom-internal's chat) and [`AiHub`]
//! (pgm-ai-internal's AI hosts, Work runs, the API, Unity). Each call is what the module did itself before 5.0.0.
use bytes::Bytes;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::auth::Account;
use super::Config;
use crate::front::Hub;
use pgm_ai_internal::hub::{AiHub, Fut};
use pgm_hcom_internal::chat::ChatHost;

impl ChatHost for Hub {
    fn chat(&self) -> &Arc<super::chat::Chat> { &self.mods.chat }
    fn files(&self) -> &super::files::Files { &self.mods.files }
    fn accounts(&self) -> Vec<Account> { self.mods.auth.accounts() }
    fn account(&self, u: &str) -> Option<Account> { self.mods.auth.account(u) }
    fn name_of(&self, u: &str) -> String { self.mods.name_of(u) }
    fn root(&self) -> &Path { &self.root }
    fn cfg(&self) -> &Config { &self.cfg }
    fn shares_for(&self, u: &str) -> Vec<Value> { self.mods.work.shares_for(u) }
    fn usage_status(&self, a: &Account) -> Value { self.mods.work.usage_status(self, a) }
    fn notes_version(&self) -> u64 { self.mods.notes.version() }
    fn ai_hosts_status(&self) -> Vec<Value> { self.mods.ai.status() }
    fn agent_called(&self, target: String, caller: String, conv: String) { if let Some(h) = self.me() { pgm_ai_internal::unity_routes::run_agent(&h, target, caller, conv); } }
    fn working(&self, u: &str) -> bool { self.mods.work.busy(u) }
}

impl AiHub for Hub {
    fn cfg(&self) -> &Config { &self.cfg }
    fn root(&self) -> &Path { &self.root }
    fn mills_dir(&self) -> PathBuf { self.mods.mills_dir() }
    fn public_url(&self) -> String { self.mods.public_url(&self.cfg) }
    fn ai(&self) -> &Arc<super::ai_hosts::AiHosts> { &self.mods.ai }
    fn work(&self) -> &Arc<super::work::Work> { &self.mods.work }
    fn api(&self) -> &super::openai::Api { &self.mods.api }
    fn unity(&self) -> &super::unity::Unity { &self.mods.unity }
    fn unity_room(&self) -> &super::unity_routes::RoomState { &self.mods.unity_room }
    fn accounts(&self) -> Vec<Account> { self.mods.auth.accounts() }
    fn account(&self, u: &str) -> Option<Account> { self.mods.auth.account(u) }
    fn is_admin(&self, u: &str) -> bool { self.mods.auth.is_admin(u) }
    fn name_of(&self, u: &str) -> String { self.mods.name_of(u) }
    fn work_message(&self, a: &Account, message: &str, where_: Value) -> Value { self.mods.files.work_message(a, message, where_) }
    fn board_tool(&self, op: &str, account: &str, args: &Value) -> Value {
        let out = self.mods.board_op(op, &self.mods.actor(account, true), args);
        if out.status == 200 { json!({ "ok": true, "result": out.body }) } else { json!({ "ok": false, "error": out.body["error"] }) }
    }
    fn notes_tool(&self, name: &str, args: &Value, actor: &str) -> Value { self.mods.notes.tool(name, args, actor) }
    fn team_tool(&self, name: &str, args: &Value, a: &Account) -> Value { pgm_hcom_internal::chat::team_tool(self, name, args, a) }
    fn chat_has_key(&self, u: &str) -> bool { self.mods.chat.has_key(u) }
    fn chat_agent_on(&self, users: &[(String, String, bool)]) -> bool { self.mods.chat.agent_on(users) }
    fn chat_room_changed(&self) { self.mods.chat.room_changed() }
    fn set_agent_busy(&self, v: Option<(String, String)>) { self.mods.chat.set_agent_busy(v) }
    fn agent_blocks(&self, target: &str, conv: &str) -> String { pgm_hcom_internal::chat::agent_blocks(self, target, conv) }
    fn agent_say(&self, text: &str, to: &str, as_: &str, via: &str, conv: &str) -> i64 { pgm_hcom_internal::chat::agent_say(self, text, to, as_, via, conv) }
    fn internal<'a>(&'a self, path: &'a str, body: String) -> Fut<'a, Option<(u16, Bytes)>> { Box::pin(crate::front::internal(self, path, body)) }
    fn push_ai_state(&self) -> Fut<'_, ()> { Box::pin(super::push_ai_state(self)) }
}
