# pgm-ai-internal: PGM_AI_Internal (PsyGraphMediaAIInternal)

Everything AI: the server side (AI hosts, providers, inference, coding runs, the assistant) and the host program's side.

- `ai_hosts`: machines that lend their AI by connecting out: the registry, the hub side of the host link (hello, jobs,
  replies, streams, events, updates), sharing, engines, the PC host, caps
- `usage`: usage windows (5 hours, 7 days) per source
- `openai`: providers (keys, endpoints, logins) and the OpenAI- and Anthropic-compatible `/v1` (plain + streaming,
  format conversion), per-person API keys
- `work`: AI coding runs (Claude Code here, relayed to a host's login, or on an older host), the run log + attach,
  MCP tool relays, shared sessions
- `unity`, `unity_brain`, `unity_routes`, `unity_time`, `unity_web`: the assistant (tools, brain, confirmations,
  voice / speech / pictures, the chat's AI member and its queue)
- `host`: the AI host program (`runner::run`, `relay`, `work`, `mcp`, `folder`, `settings` = ai-node.json)
- `local_proxy`: 127.0.0.1 → a hub's `/v1` with a person's key (no web page can use it)

## What it needs from the program around it

`hub::AiHub`: the config and folders (`cfg`, `root`, `mills_dir`, `public_url`), the state this library made (`ai`,
`work`, `api`, `unity`, `unity_room`), accounts (`accounts`, `account`, `is_admin`, `name_of`), the tools an AI may
call (`work_message`, `board_tool`, `notes_tool`, `team_tool`), the chat (`chat_has_key`, `chat_agent_on`,
`chat_room_changed`, `set_agent_busy`, `agent_blocks`, `agent_say`) and two async calls (`internal`, `push_ai_state`).
Return an empty value for what you do not have. Entry points take `&Arc<dyn AiHub>` (they start background work) and
answer with `hub::Resp`.

## Use it in another project

```rust
use std::sync::Arc;
use pgm_ai_internal::{hub::AiHub, openai};
impl AiHub for Server { /* the list above */ }
let hub: Arc<dyn AiHub> = server.clone();
let answer = openai::v1(&hub, &request).await; // an OpenAI- / Anthropic-compatible API on your server
```

The host program: `pgm_ai_internal::host::runner::run(...)` is the whole job loop; build with `VR_VERSION` /
`VR_COMMIT` / `VR_RELEASE_PUBKEY` set as for vr-host (build.rs passes the target).
