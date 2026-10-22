//! Unity's brain (a local model's tool loop or Claude): pgm-ai-internal (PGM_AI_Internal) since 5.0.0.
use std::sync::Arc;

use crate::front::Hub;
use pgm_ai_internal::hub::AiHub;
#[allow(unused_imports)]
pub use pgm_ai_internal::unity_brain::*;

pub fn start(hub: &Arc<Hub>) { let h: Arc<dyn AiHub> = hub.clone(); pgm_ai_internal::unity_brain::start(&h) }
