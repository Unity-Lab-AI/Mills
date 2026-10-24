# The proving ground

A persistent, separately isolated copy of the hub for driving real end-to-end scenarios (`bridge/test/proving.js`).
Test accounts `spongetest` (admin) and `geetest`, their own data, a chip-away project (`proving-ground`, cloned from the
private Forgejo repo `Sponge/proving-ground`: a tiny `shelf` module, `node test/shelf.test.js`) and real models through
the Claude relay of the Work run that drives it.

Everything lives in `/srv/apps/sharedwork/_proving` (outside every repo): `hub/` (code copy, config, accounts, data,
`projects/`, `worktrees/`, `hosts/`), `hub.log`, `hub.pid`, `fake-openai.log`. The hub runs from THIS checkout's code,
copied at every `start`.

## Commands

    node bridge/test/proving.js start [--rust-hub]   hub on 7795 (Rust hub in front: back port 7796), detached
    node bridge/test/proving.js status | stop
    node bridge/test/proving.js reset [--all]        stop + wipe the data; keeps hub/accounts.txt unless --all
    node bridge/test/proving.js login <account>      prints a session token (Authorization: Bearer <token>)
    node bridge/test/proving.js work <account> <message> [--model claude-haiku-5-5] [--project proving-ground]
    node bridge/test/proving.js hosts <n>            n AI hosts on a fake OpenAI endpoint (7798); hosts 0 stops them

`start` refuses without `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` in the environment (Work turns run the real
`claude` on PATH), and is idempotent. `--rust-hub` uses `native/target/regress/release/vr-hub` (built by
`regress.js --rust-hub`) or `VR_HUB_BIN`. Passwords are random, in `hub/accounts.txt` (0600). The run's own git login
(GIT_CONFIG_* / FORGEJO_*) is inherited, so Work turns can clone and push to git.unityailab.com.

`work` selects the project and model, posts the message to `/api/chat` as that account and prints every event as NDJSON
until the run ends; exit code 0 = the run ended (`bridge_done` code 0) without `bridge_error` / an error result.
The sandbox's tool list is the live one plus `node`, `git checkout|switch|branch|show|rev-parse|config` and a few
read-only shell commands (no `ssh`, no `fj`).

## Never touches

The live hub (port 7777, /srv/apps/VibeRoller/app), its data, accounts, hosts, board or Forgejo mirror; no tunnel, no
gist, no `aiNodes` from the live config. Branches a scenario pushes to `Sponge/proving-ground` are real: delete the
ones you no longer need.

## Driving a scenario

    T=$(node bridge/test/proving.js login spongetest)
    curl -s -H "Authorization: Bearer $T" http://127.0.0.1:7795/api/check
    node bridge/test/proving.js hosts 2          # routing: both show up as providers h:<host>/local
    node bridge/test/proving.js work geetest "..." --model claude-haiku-5-5

Any hub endpoint works the way `bridge/test/regress.js` calls it (JSON with `Authorization: Bearer <token>`).
