//! Unity's section and the chat's Unity: pgm-ai-internal (PGM_AI_Internal) since 5.0.0. What it needs from the hub:
//! modules/traits.rs (AiHub). Here: the hub's entry points.
use serde_json::Value;
use std::sync::Arc;

use super::auth::Account;
use super::Routed;
use crate::front::{Hub, Req};
use pgm_ai_internal::hub::AiHub;
pub use pgm_ai_internal::unity_routes::*;

fn d(hub: &Arc<Hub>) -> Arc<dyn AiHub> { hub.clone() }
pub async fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> { pgm_ai_internal::unity_routes::route(&d(hub), r, a).await.map(Routed::Answer) }
pub async fn internal(hub: &Arc<Hub>, r: &Req) -> Routed { Routed::Answer(pgm_ai_internal::unity_routes::internal(&d(hub), r).await) }
pub fn signed_in(hub: &Arc<Hub>, user: &str, client: Value, ip: String) { pgm_ai_internal::unity_routes::signed_in(&d(hub), user, client, ip) }
