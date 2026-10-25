//! What the AI parts need from the program around them ([`AiHub`]; VibeRoller's hub implements it), and the answer
//! helpers they share.
use bytes::Bytes;
use pgm_core_internal::config::Config;
use pgm_identity_internal::Account;
use pgm_net_internal::http::{json as resp_json, Body};
use serde_json::Value;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use crate::ai_hosts::AiHosts;
use crate::openai::Api;
use crate::unity::Unity;
use crate::unity_routes::RoomState;
use crate::work::Work;

/// A future the trait hands back (async calls through a trait object).
pub type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the AI parts need from the program around them.
pub trait AiHub: Send + Sync {
    // ---- the program
    fn cfg(&self) -> &Config;
    /// Its own folder (its config, ai-host.off, .restart, providers.json …) and the folder above it (Archive/, Api/ …).
    fn root(&self) -> &Path;
    fn mills_dir(&self) -> PathBuf;
    /// The address people reach it at (an https address, else this machine).
    fn public_url(&self) -> String;
    // ---- the AI parts it keeps
    fn ai(&self) -> &Arc<AiHosts>;
    fn work(&self) -> &Arc<Work>;
    fn api(&self) -> &Api;
    fn unity(&self) -> &Unity;
    fn unity_room(&self) -> &RoomState;
    // ---- people
    fn accounts(&self) -> Vec<Account>;
    fn account(&self, u: &str) -> Option<Account>;
    fn is_admin(&self, u: &str) -> bool;
    /// How a person is named (their nickname, else their display name).
    fn name_of(&self, u: &str) -> String;
    // ---- the rest of the program a run's tools reach
    /// A message with its attached files turned into paths a run may read (`{ text, dirs }`).
    fn work_message(&self, a: &Account, message: &str, where_: Value) -> Value;
    /// A board tool (`op`) for this account as an agent: `{ ok, result }` / `{ ok: false, error }`.
    fn board_tool(&self, op: &str, account: &str, args: &Value) -> Value;
    /// A notes tool, saved as `actor`.
    fn notes_tool(&self, name: &str, args: &Value, actor: &str) -> Value;
    /// A team (chat) tool for this account.
    fn team_tool(&self, name: &str, args: &Value, a: &Account) -> Value;
    /// An RE container tool (`re_start` to `re_stop`, T-303): `{ ok, result }` / `{ ok: false, error }`. A program without the container manager says so.
    fn re_tool<'a>(&'a self, _name: &'a str, _args: &'a Value, _a: &'a Account) -> Fut<'a, Value> { Box::pin(async { serde_json::json!({ "ok": false, "error": "The RE containers are not available on this hub." }) }) }
    // ---- the chat's AI member
    fn chat_has_key(&self, u: &str) -> bool;
    fn chat_agent_on(&self, users: &[(String, String, bool)]) -> bool;
    fn chat_room_changed(&self);
    /// She is composing (target, conv) or not.
    fn set_agent_busy(&self, v: Option<(String, String)>);
    /// What she was sent in that conversation since she last answered there.
    fn agent_blocks(&self, target: &str, conv: &str) -> String;
    /// Posts as her (or `as_`, with `via` = whose run): the message number.
    fn agent_say(&self, text: &str, to: &str, as_: &str, via: &str, conv: &str) -> i64;
    // ---- the program behind this one (if any)
    /// A word to a backend program (its internal API): (status, body), or None.
    fn internal<'a>(&'a self, path: &'a str, body: String) -> Fut<'a, Option<(u16, Bytes)>>;
    /// Tell whoever keeps a copy of the AI hosts' state that it changed.
    fn push_ai_state(&self) -> Fut<'_, ()>;
}

/// An answer.
pub type Resp = hyper::Response<Body>;
/// A JSON answer.
pub fn answer(status: u16, v: Value) -> Resp { resp_json(status, &v) }
/// A body streamed from a channel.
pub fn stream_body(rx: tokio::sync::mpsc::Receiver<Result<Bytes, String>>) -> Body {
    use http_body_util::BodyExt;
    let s = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x.map(hyper::body::Frame::data).map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e)), rx)) });
    http_body_util::StreamBody::new(s).boxed()
}
