//! The chat: pgm-hcom-internal (PGM_HCom_Internal) since 5.0.0: end-to-end encrypted messages, receipts, typing,
//! presence, the room's long poll, calls, servers / channels / groups, the team tools, Unity's side of the chat. The same
//! files (Chat/…). What it needs from the hub: modules/traits.rs (ChatHost). Here: the hub's entry points.
use serde_json::Value;
use std::sync::Arc;

use super::auth::Account;
use super::Routed;
use crate::front::{Hub, Req};
pub use pgm_hcom_internal::chat::*;

/// The chat's routes (signed in as `a`); None = not one of them.
pub async fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> { pgm_hcom_internal::chat::route(&**hub, r, a).await.map(Routed::Answer) }
/// The chat's background work.
pub fn start(hub: &Arc<Hub>) { pgm_hcom_internal::chat::start(hub.clone()) }
/// The JavaScript hub asking (/__front/chat/…).
pub fn front(hub: &Arc<Hub>, what: &str, b: &Value) -> Value { pgm_hcom_internal::chat::front(&**hub, what, b) }
