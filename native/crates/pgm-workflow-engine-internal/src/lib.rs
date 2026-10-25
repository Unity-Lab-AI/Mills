//! **pgm-workflow-engine-internal** = PGM_WorkflowEngine_Internal (PsyGraphMediaWorkflowEngineInternal): the Hyper
//! Workflows engine (T-200; spec: VibeRoller's team note plans/hyper-workflows.md, sections 4, 8 and 14).
//!
//! A workflow is a [`pgm_workflow_internal::Document`]; a run is one prompt sent through it. The engine is plain code,
//! no agent loop: it follows the graph, calls an AI only when a node says so, and goes on until an `end`, a limit, a
//! cancel or an unhandled failure.
//! - [`Engine`]: [`Engine::start`] a run, [`Engine::resume`] it from its log, [`Engine::answer`] a question it waits on,
//!   [`Engine::replay`] it with no outside calls; [`Engine::preflight`] says what would stop a document running here.
//! - [`WorkflowHost`]: everything outside the engine (AI, tools, agents, people, the log sink, the clock).
//! - [`NodeKind`]: what one kind of node does; the flow-control kinds are the engine's own.
//! - [`fake`]: a fake host and fake kinds for tests.
//!
//! Model and engine are two crates (open question 1): the model needs no async runtime, so an editor or a viewer can
//! use it alone.
#![forbid(unsafe_code)]

pub mod cancel;
mod engine;
pub mod fake;
pub mod host;
pub mod kinds;
pub mod log;
mod plan;
mod run;
pub mod service;

pub use cancel::Cancel;
pub use engine::{Engine, RunInput, RunLimits, RunOptions, RunReport};
pub use host::{walk, Ask, BoxFut, HttpAnswer, HttpRequest, ModelAnswer, ModelRequest, NoHost, NodeCall, NodeError, NodeKind, NodeOutput, ToolCtx, WorkflowHost};
pub use log::Prior;
pub use pgm_workflow_internal as model;
