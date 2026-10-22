//! Providers and the inference APIs (/v1: OpenAI- and Anthropic-compatible; API keys): pgm-ai-internal
//! (PGM_AI_Internal) since 5.0.0. What it needs from the hub: modules/traits.rs (AiHub). Here: the hub's entry points.
use std::sync::Arc;

use super::auth::Account;
use super::Routed;
use crate::front::{Hub, Req};
use pgm_ai_internal::hub::AiHub;
pub use pgm_ai_internal::openai::*;

fn d(hub: &Arc<Hub>) -> Arc<dyn AiHub> { hub.clone() }
pub async fn v1(hub: &Arc<Hub>, r: &Req) -> Routed { Routed::Answer(pgm_ai_internal::openai::v1(&d(hub), r).await) }
pub fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> { pgm_ai_internal::openai::route(&d(hub), r, a).map(Routed::Answer) }
