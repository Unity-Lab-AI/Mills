# pgm-sandbox-internal (PGM_Sandbox_Internal)

Throw-away container copies of a pre-set-up base image (board T-303; first user: reverse engineering with REA,
T-253 / T-254). One library for the hub, the AI host, native apps and Jcode PGM. Engine-agnostic: one `Runtime` trait,
with `Cli::podman()` (Linux) and `Cli::docker()` (Windows) implementations; a program or a test may bring its own.

```rust
let cfg = Config::from_json(root, &json!({ "image": "localhost/pgm-re-base:1.0.0", "maxConcurrent": 3 }));
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

## Not here (yet)

Windows Docker verification, wiring into the native apps / Jcode PGM, per-user quotas beyond the queue length.
