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
| PGM_Dist_Internal | PsyGraphMediaDistributionInternal | `pgm-dist-internal` (`pgm_dist_internal`) | signed releases, self-update, downloads, version stamps |

Dependency order (a library only uses the ones before it): Core → Platform, Net → Identity, Collab, Dist → HCom, AI.
Context uses no other PGM library; AI uses it (the Work system prompt through its lint, the usage ledger for API / Unity answers).
HCom and AI do not use each other: what one needs from the other goes through the program around them (the hub
implements `ChatHost` and `AiHub`). `vr-core` is VibeRoller's old shared crate: it now only re-exports from these, so
code that names `vr_core::…` keeps working. How to use a library elsewhere: each crate's README.md.

Since 5.1.0 the libraries also have a repository of their own, https://git.unityailab.com/UnityAILab/pgm-libs (tagged
`vX.Y.Z`, with the web kit), which the PGM products build from. Until VibeRoller builds from it too, this copy and
that one are kept identical: a change to a crate here goes there as well, with a new tag.

Mirrored from pgm-libs by copy (LAWS.md, library law): `pgm-context-internal` = pgm-libs 5.15.1 (the `src/` identical; the
tree-sitter feature is optional and OFF here, so the repomap uses its text scanner). A change goes to pgm-libs first.
