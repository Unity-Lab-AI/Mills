# The PGM libraries: short names, full names, crates

Every library here is reusable in any project (nothing in it knows VibeRoller's hub); the VibeRoller programs (vr-hub,
vr-host, vr-desktop) are built on them. Plan and rules: the team note plans/pgm-libraries.md.

| Short name | Full name | Crate (`use` name) | Holds |
|---|---|---|---|
| PGM_Core_Internal | PsyGraphMediaCoreInternal | `pgm-core-internal` (`pgm_core_internal`) | errors, base64, time, hashing / HMAC / AES-GCM / HKDF / ids, JSON files (atomic, private), file protection, text helpers |
| PGM_Platform_Internal | PsyGraphMediaPlatformInternal | `pgm-platform-internal` (`pgm_platform_internal`) | processes, standard folders, installing a program in place, Windows service, systemd units, start at boot |
| PGM_Net_Internal | PsyGraphMediaNetworkInternal | `pgm-net-internal` (`pgm_net_internal`) | HTTP server kit, reverse proxy, long polls, HTTP clients, the encrypted host link, relays, sync, rate limits |
| PGM_Identity_Internal | PsyGraphMediaIdentityInternal | `pgm-identity-internal` (`pgm_identity_internal`) | accounts, passwords, sessions, device locks, sign-in limits, admins, per-account keys |
| PGM_AI_Internal | PsyGraphMediaAIInternal | `pgm-ai-internal` (`pgm_ai_internal`) | AI hosts + sharing, providers, inference APIs, Work runs, MCP tools, usage, Unity, the host runner |
| PGM_HCom_Internal | PsyGraphMediaHumanCommunicationInternal | `pgm-hcom-internal` (`pgm_hcom_internal`) | chat + end-to-end encryption, servers / channels / roles, calls, attachments, voice clips, images / video, profiles, markdown |
| PGM_Collab_Internal | PsyGraphMediaCollaborationInternal | `pgm-collab-internal` (`pgm_collab_internal`) | board, notes, projects / worktrees, Forgejo |
| PGM_Context_Internal | PsyGraphMediaContextInternal | `pgm-context-internal` (`pgm_context_internal`) | fewer tokens, more cache hits: token counting per provider, the usage / cache ledger, cache-friendly prompt assembly + its volatile-text lint, tool-output shaping, repo map, replay cache, prefix-cache routing (tree-sitter feature off here) |
| PGM_Workflow_Internal | PsyGraphMediaWorkflowInternal | `pgm-workflow-internal` (`pgm_workflow_internal`) | Hyper Workflows as data: graph model + JSON document (unknown fields kept, migrate on read), port types, validation (3.3), diff, versions, laws / prompts shapes, node-kind manifests + registry, `{{template}}` / condition expressions, the document store (T-199, plans/hyper-workflows.md) |
| PGM_WorkflowCommand_Internal | PsyGraphMediaWorkflowCommandInternal | `pgm-workflow-command-internal` (`pgm_workflow_command_internal`) | the one `/hyperwork <name> [options] <prompt>` line parser for the page, Hyper Prod Man and jcode, with the nearest-names helper; serde only (T-207; mirrored by copy into Jcode PGM's pgm/libs) |
| PGM_WorkflowEngine_Internal | PsyGraphMediaWorkflowEngineInternal | `pgm-workflow-engine-internal` (`pgm_workflow_engine_internal`) | runs a workflow: tokens on wires, parallel lines + joins, switch, bounded loops, sub-graphs, for-each, sub-workflows, retries / timeouts / failure paths, approvals, run limits, cancel, the run log, resume / answer / replay from it; `WorkflowHost` + `NodeKind` traits, a fake host (T-200); the built-in kinds prompt, chat, summarise, jev, tool (+ custom HTTP), script (transform language), agent, orchestrate (T-201) |
| PGM_Orchestrator_Internal | PsyGraphMediaOrchestratorInternal | `pgm-orchestrator-internal` (`pgm_orchestrator_internal`) | the policy of /pgm-orchestrator: worker profiles, task kinds, worker selection with fallbacks, health, spend; WorkflowEngine's `orchestrate` picks its workers with it |
| PGM_Plan_Internal | PsyGraphMediaPlanInternal | `pgm-plan-internal` (`pgm_plan_internal`) | planner mode, one library for the hub and Jcode PGM: the draft check, the parents / sub-tasks table, approve all / some / change / reject, the writer in the hub's order behind a `PlanHub` trait, the planner helpers (parse, existing, roles, owner, glossary) and helper-set detection |
| PGM_Sandbox_Internal | PsyGraphMediaSandboxInternal | `pgm-sandbox-internal` (`pgm_sandbox_internal`) | throw-away container copies of a base image (reverse engineering, T-303): a cap on how many run, a FIFO queue, owner-only control, an exchange folder for data in / out, idle reporting, adoption after a restart; one `Runtime` trait with Podman / Docker command-line implementations |
| PGM_Sync_Internal | PsyGraphMediaSyncInternal | `pgm-sync-internal` (`pgm_sync_internal`) | two full copies of a program's data kept the same both ways (T-324, plans/dev-hub.md § 2): change records + each copy's add-only journal, the hub-to-hub exchange on this machine with a shared secret (cursor per peer, catch up, no echo), newest wins per record with the loser kept, a kind's own merge, ids two copies both handed out moved apart, shape checks (malformed refused), unknown kinds held; the program's data plugs in through `Kind` adapters |
| PGM_Dist_Internal | PsyGraphMediaDistributionInternal | `pgm-dist-internal` (`pgm_dist_internal`) | signed releases, self-update, downloads, version stamps |

Dependency order (a library only uses the ones before it): Core → Platform, Net → Identity, Collab, Dist → HCom, AI.
Sandbox uses only Core (a program brings the engine: Podman or Docker CLI, or its own `Runtime`).
Sync uses Core and Net (its HTTP client); it knows no program's data: the hub's board, notes, chat and accounts are adapters in vr-hub (modules/sync.rs).
Workflow uses Core only (no hub, no screen, no network); WorkflowEngine uses Workflow and Orchestrator (and tokio).
Context uses no other PGM library; AI uses it (the Work system prompt through its lint, the usage ledger for API / Unity answers).
HCom and AI do not use each other: what one needs from the other goes through the program around them (the hub
implements `ChatHost` and `AiHub`). `vr-core` is VibeRoller's old shared crate: it now only re-exports from these, so
code that names `vr_core::…` keeps working. How to use a library elsewhere: each crate's README.md.

Since 5.1.0 the libraries also have a repository of their own, https://git.unityailab.com/UnityAILab/pgm-libs (tagged
`vX.Y.Z`, with the web kit), which the PGM products build from. Until VibeRoller builds from it too, this copy and
that one are kept identical: a change to a crate here goes there as well, with a new tag.

Mirrored from pgm-libs by copy (LAWS.md, library law): `pgm-context-internal` = pgm-libs 5.15.1 (the `src/` identical; the
tree-sitter feature is optional and OFF here, so the repomap uses its text scanner), `pgm-orchestrator-internal` =
pgm-libs 5.20.0 (the crate identical), `pgm-plan-internal` = pgm-libs 5.23.0 (identical; used by `pgm-ai-internal`
through `plan.rs` / `planner.rs`, and mirrored the same way in Jcode PGM's `pgm/libs`). A change goes to pgm-libs first.
The two workflow crates start here (T-199, T-200) and go to pgm-libs with the Hyper Workflows release.
