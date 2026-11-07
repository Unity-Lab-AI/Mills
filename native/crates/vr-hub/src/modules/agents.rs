//! Agent to agent (T-296 / T-297; team note plans/agent-talk.md): pgm-ai-internal's agents module (the envelope, threads,
//! delivery into Work sessions, the person's controls, limits, the audit). Here: the hub's entry point for /api/agents/*.
//! The frozen JavaScript hub has none of it (it answers 404).
use std::sync::Arc;

use super::auth::Account;
use super::Routed;
use crate::front::{Hub, Req};
use pgm_ai_internal::hub::AiHub;

/// /api/agents/* signed in as `a`; None = not one of them.
pub async fn route(hub: &Arc<Hub>, r: &Req, a: &Account) -> Option<Routed> {
    if !r.path.starts_with("/api/agents/") { return None; }
    let d: Arc<dyn AiHub> = hub.clone();
    pgm_ai_internal::agents::route(&d, r, a).await.map(Routed::Answer)
}
