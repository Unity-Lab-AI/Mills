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
| PGM_Dist_Internal | PsyGraphMediaDistributionInternal | `pgm-dist-internal` (`pgm_dist_internal`) | signed releases, self-update, downloads, version stamps |

Dependency order (a library only uses the ones before it): Core → Platform → Net → Identity → AI / HCom / Collab / Dist.
`vr-core` is VibeRoller's old shared crate: it now only re-exports from these, so code that names `vr_core::…` keeps working.
