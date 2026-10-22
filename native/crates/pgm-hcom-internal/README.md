# pgm-hcom-internal: PGM_HCom_Internal (PsyGraphMediaHumanCommunicationInternal)

Everything people-to-people.

- `e2e`: the chat's end-to-end encryption (ECDH P-256, a per-message key wrapped for every reader, v3 / v4 messages,
  files, voice clips), exactly as the web page does it
- `guilds`: servers, channels and groups: membership, roles, invites, moderation (timeouts, bans, slowmode, the
  new-member wait), pins, password locks, the audit log, group caps
- `perms`: Discord's permission model and order (server → @everyone → roles → member; ranks)
- `files`: attachments (ciphertext only), people's uploads and the Files library
- `chat`: messages (only ciphertext), receipts, typing, presence, the room's long poll, calls (the set-up relay), the
  chat's AI member, the team tools; `start(hub)` runs its background work

## What it needs from the program around it

`chat::ChatHost` (implement it once, on your server's state):

| Method | Gives |
|---|---|
| `chat()`, `files()` | the chat store and the Files library this library made |
| `accounts()`, `account(u)`, `name_of(u)` | who exists (pgm-identity-internal's `Account`) and their display names |
| `root()`, `cfg()` | the data folder and the config |
| `shares_for(u)`, `usage_status(a)`, `notes_version()`, `ai_hosts_status()` | what the room's long poll also reports (return empty values if you have none) |
| `agent_called(target, caller, conv)` | someone called the AI member: start it (or do nothing) |

## Use it in another project

```rust
use pgm_hcom_internal::chat::{self, Chat, ChatHost};
struct Server { chat: std::sync::Arc<Chat>, files: pgm_hcom_internal::files::Files, /* … */ }
impl ChatHost for Server { /* the table above */ }
chat::start(server.clone()); // Arc<dyn ChatHost>
```
