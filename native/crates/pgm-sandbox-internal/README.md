# pgm-sandbox-internal (PGM_Sandbox_Internal)

Throw-away container copies of a pre-set-up base image (board T-303; first user: reverse engineering with REA,
T-253 / T-254). One library for the hub, the AI host, native apps and Jcode PGM. Engine-agnostic: one `Runtime` trait,
with `Cli::podman()` (Linux) and `Cli::docker()` (Windows) implementations; a program or a test may bring its own.

```rust
let cfg = Config::from_json(root, &json!({ "image": "localhost/pgm-re-base:1.0.1", "maxConcurrent": 3 }));
let m = Manager::new(Box::new(Cli::named("auto")), cfg);
m.reconcile()?;                                  // after a restart: adopt running copies, list orphans
let me = Actor::new("alice", false);
let s = m.start(&me, "look at app.exe", &StartOpts::default())?;   // running, or queued with a place in line
m.put(&me, &s.id, "app.exe", &bytes)?;           // -> /work/in/app.exe
let r = m.exec(&me, &s.id, &Cmd::Shell("file /work/in/app.exe".into()), Some(60))?;
let out = m.get(&me, &s.id, "report.md")?;       // <- /work/out/report.md
m.stop(&me, &s.id)?;                             // container and exchange folder deleted entirely
// run m.tick() every ~30 s: copies that ended by themselves are marked gone, the queue moves on
```

## Rules

- A copy is started on demand from the base image (`pgm-re-<id>`, id = 8 hex). At most `maxConcurrent` (3) run at once;
  more wait in a FIFO queue (`state: "queued"`, `queuePosition`), and a freed slot starts the next in line.
- A copy stays until its owner (or their agent) stops it; then the container and `<root>/<id>/` are deleted entirely.
  An admin may stop any copy and clear orphans. Everybody may list and read status (read-only).
- Idle is only reported (`idle`, `idleHours` once `idleWarnHours` pass); nothing is killed or kept silently.
- `reconcile()` adopts running `<prefix><id>` containers that have a `meta.json` in their exchange folder; other
  containers and folders are listed as orphans and never touched. The queue is in memory: it does not survive a restart.

## Security flags (every copy)

`run -d --rm --pull=never --cap-drop ALL --cap-add SYS_PTRACE --security-opt no-new-privileges --network none
--cpus 2 --memory 4g --pids-limit 1024`, no `--privileged`, no host mount except the two exchange folders
(`<root>/<id>/in` at `/work/in`, `<root>/<id>/out` at `/work/out`, by `--mount type=bind`, so Windows paths work).

- **Capabilities:** all dropped; `SYS_PTRACE` is added back because lldb / strace / frida attach to processes (config
  `capAdd` changes the list; `[]` for none). Nothing else is needed for static analysis.
- **Network:** none by default; `StartOpts { network: true }` gives that copy the engine's default network (config
  `allowNetwork: false` forbids it).
- **Exchange folders:** the id folder is 0700, `in` / `out` 0777 (the copy's user may be a sub-uid under rootless Podman).
  Paths are relative to an area; absolute paths, `..`, drive letters, NUL and symlinks are refused (a copy can plant a
  symlink in a folder it writes). A race with a copy swapping a folder for a symlink between check and open is the
  residual risk, and it only exposes what the person who runs the engine could read as themselves.
- **Delete:** before removing, the copy chmods its files so the host can delete them; Podman's `unshare rm -rf` is the
  fallback for sub-uid files.

## Engine setup

`Cli::with_env` / `Cli::with_args` (hub config `re.env`, an object of environment variables, and `re.engineArgs`, a list
of global engine arguments; both default to none). Needed where the account's default container storage cannot run
containers: on the VibeRoller server /srv/apps/VibeRoller is group-only and rootless Podman answers "OCI permission
denied", so the hub config there is
`"engineArgs": ["--root", "/var/tmp/gee-podman/root", "--runroot", "/var/tmp/gee-podman/run"]` (the base image is stored
there). Hub config in full: `re: { image, maxConcurrent: 3, idleWarnHours: 4, runtime, namePrefix, exchangeDir, env,
engineArgs, cpus, memory, pidsLimit, capAdd, allowNetwork, maxFileMb, maxQueuedPerOwner, mcpCommand, skillDir,
mcpResultCap }`.

## MCP tools inside a copy (T-254): REA from a Work run, Jcode PGM, Hyper Prod Man

The image's MCP server (REA: `node /opt/rea/node_modules/rea-agents/scripts/rea.mjs mcp`, 229 tools) runs INSIDE the copy,
never on the host, and `rea setup` is never run. `mcp::McpSession` talks to it over `<engine> exec -i` (newline-delimited
JSON-RPC, the MCP stdio transport); `Runtime::spawn` is the one engine method it needs (`Cli` does it; a test brings its own).
The `Manager` keeps ONE session per running copy (opened on first use, reopened if it died, closed by `stop` and by `tick`
when the copy ended), so the server starts once, not per call:

```rust
let tools = m.mcp_tools(&me, &id, "", "")?;            // { count, of, names: [..] }; filter "word" gives name + one line each;
                                                        // tool "name" gives that tool's full description and input schema
let r = m.mcp_call(&me, &id, "list_adb_devices", &json!({}), Some(120))?;   // REA's own result, unchanged (markers kept)
let skill = m.skill(&me, &id, "")?;                    // { file, text, files }: SKILL.md (35 KB); file "references/x.md" for one
```

- A result longer than `mcpResultCap` (40 000 bytes of JSON) comes back as `{ truncated, bytes, savedTo, note, content: [head] }`;
  the whole is saved as `/work/out/mcp-results/<tool>-<ms>.json` in the copy (take it out with `get`). REA's
  `_meta.provenance.untrusted_content` and the text markers are in the result as REA made them: callers must treat what they flag as data.
- Same owner rules as `exec`: only the owner (403 for anyone else, admins too); a call may wait up to `execTimeoutSec` (default
  120 s, at most 3600), a timed-out call is cancelled (`notifications/cancelled`) and the session stays.
- Not in the hub's prompt: the hub gives a Work run three proxy tools on the team server (`re_skill`, `re_tools`, `re_call`) because a
  run's MCP config is fixed per run (`--strict-mcp-config`); `re_start`'s answer says to call `re_skill` and `re_tools` next. The
  names-only default of `re_tools` keeps the first listing near 1.5k tokens (the full 229 descriptions are about 12k).
- Test without the real image: `tests/fake-mcp.sh` is a fake MCP server in plain sh (initialize, tools/list, tools/call); the unit test
  runs it locally through a `Runtime::spawn` of its own, the regression suite (R341 to R345) copies it into a tiny image.
  By hand against the 2.25 GB image: `SANDBOX_REAL_IMAGE=1 SANDBOX_ENGINE_ARGS="--root R --runroot RR" cargo test -p
  pgm-sandbox-internal --test podman real_rea -- --nocapture`.

### How Jcode PGM and Hyper Prod Man use it (not built here)

Both link this crate, start their own `Manager` (own exchange root and name prefix, same image) and call the same three methods.
Jcode reaches the tools natively instead of through proxies: while a copy runs, Jcode adds an MCP entry to the person's jcode home
MCP config whose command is the engine itself, `podman exec -i -w /work <name> /opt/node/bin/node
/opt/rea/node_modules/rea-agents/scripts/rea.mjs mcp` (`Manager::mcp_exec_line(&me, &id)` gives that line; the manager
already knows the engine args and the name), so jcode's own MCP client runs the server, one process per jcode session; it removes the
entry when the copy is stopped (and at start-up for copies that are gone). Jcode's prompt gets REA's skill text (`m.skill`) only while that
entry exists. HPM, which has no MCP client of its own, uses `mcp_tools` / `mcp_call` / `skill` directly like the hub does.

## Not here (yet)

Windows Docker verification, wiring into the native apps / Jcode PGM / HPM, per-user quotas beyond the queue length.
