//! **pgm-workflow-internal** = PGM_Workflow_Internal (PsyGraphMediaWorkflowInternal): Hyper Workflows as data.
//! A workflow is boxes (nodes) with sockets (ports) joined by wires (edges). This library holds everything about the
//! graph that needs no screen, no hub and no network (spec: VibeRoller's team note plans/hyper-workflows.md):
//! - [`model`]: the document types (spec section 13); unknown fields are kept and written back
//! - [`doc`]: parse and write, `schema` migrate-on-read, a newer schema refused with a clear message
//! - [`types`]: port types and the type rules (3.1)
//! - [`kinds`]: node-kind manifests (11.1), the [`kinds::KindRegistry`] trait and the built-in kinds' port signatures
//! - [`validate`]: every rule of 3.3, as a list of errors and warnings that name the node / port / edge
//! - [`diff`]: two versions compared node by node (added, removed, changed config, rewired)
//! - [`version`]: version numbers, channels, `name@3` references, publishing (9.1)
//! - [`shapes`]: laws, saved prompts and groups (6.1)
//! - [`expr`]: `{{name}}` templates and a small safe condition language, our own, no code runs
//! - [`store`]: [`store::load_dir`] lists workflow files, moving a bad one aside as `*.bad-<time>` and going on
//!
//! The engine that runs a graph is not here (T-200): it asks this library "is this valid, what are its ports".
#![forbid(unsafe_code)]

pub mod diff;
pub mod doc;
pub mod expr;
pub mod kinds;
pub mod model;
pub mod shapes;
pub mod store;
pub mod types;
pub mod validate;
pub mod version;

pub use diff::{diff, DocDiff};
pub use kinds::{DefaultRegistry, KindManifest, KindRegistry};
pub use model::{Document, Edge, Node, PortDecl, Section, Subgraph};
pub use pgm_core_internal::{Error, Result};
pub use types::PortType;
pub use validate::{validate, Issue, Level, Options, Report, ScopeInfo};

/// The document schema this library writes and fully understands.
pub const SCHEMA: u32 = 1;
/// A document over this many bytes is refused (spec 13: 1 MB by default).
pub const MAX_BYTES: usize = 1_000_000;
/// A graph over this many nodes (each level counted alone) is refused (spec 13: 2000 by default).
pub const MAX_NODES: usize = 2000;
/// Sub-graphs nest at most this deep by default (spec 3.3 rule 7).
pub const MAX_DEPTH: usize = 8;
