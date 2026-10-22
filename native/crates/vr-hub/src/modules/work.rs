//! Work runs: pgm-ai-internal (PGM_AI_Internal) since 5.0.0: Claude Code runs (here, relayed to a host's login, or on
//! an older host), their log + attach, the MCP tool relays, shared sessions, projects, past sessions. What it needs from
//! the hub: modules/traits.rs (AiHub). Here: the hub's entry points.
use serde_json::Value;
use std::sync::Arc;

use super::auth::Account;
use super::Routed;
use crate::front::{Hub, Req};
use pgm_ai_internal::hub::AiHub;
pub use pgm_ai_internal::work::*;

fn d(hub: &Arc<Hub>) -> Arc<dyn AiHub> { hub.clone() }
pub async fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> { pgm_ai_internal::work::route(&d(hub), r, a).await.map(Routed::Answer) }
pub async fn tool(hub: &Arc<Hub>, which: &str, account: &str, b: &Value) -> Value { pgm_ai_internal::work::tool(&d(hub), which, account, b).await }
pub async fn llm(hub: &Arc<Hub>, r: &Req) -> Routed { Routed::Answer(pgm_ai_internal::work::llm(&d(hub), r).await) }
pub fn start(hub: &Arc<Hub>) { pgm_ai_internal::work::start(&d(hub)) }
