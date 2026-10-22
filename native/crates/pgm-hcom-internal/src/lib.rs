//! **pgm-hcom-internal** = PGM_HCom_Internal (PsyGraphMediaHumanCommunicationInternal): everything people-to-people.
//! - [`e2e`]: the chat's end-to-end encryption (ECDH P-256 keys, per-message keys wrapped for every reader, v3 / v4
//!   messages, files, voice clips, the uploader's own seal), exactly as the web page does it
//! - [`guilds`]: servers, channels and group conversations: membership, roles, invites, moderation (timeouts, bans,
//!   slowmode, the new-member wait), pins, password locks, the audit log, group caps
//! - [`perms`]: Discord's permission model and order (server → @everyone → roles → member; ranks)
//! - [`files`]: attachments (ciphertext only), people's uploads (in their linked Linux account or here) and the Files
//!   library (when, where, last used)
//! The chat store, receipts, typing, presence, calls and profiles join as they move (board T-141).

pub mod e2e;
pub mod files;
pub mod guilds;
pub mod perms;
