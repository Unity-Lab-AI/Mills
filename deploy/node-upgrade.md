# Moving the team's Node.js (T-304)

Why: the 2026-10-21 Node security release (CVE-2026-105482 DNS NUL truncation, CVE-2026-105483 / -106069 / -106072 TLS
host-name checks, CVE-2026-111497 Brotli out-of-bounds write in `node:zlib`, all fixed in 22.23.4, 24.22.1, 26.12.1; the
server ran 22.23.3). Details: https://nodejs.org/en/blog/vulnerability/october-2026-security-releases

## The line

| Line | Release | Status (nodejs.org schedule) | End of life |
|---|---|---|---|
| 22 "Jod" | v22.23.4 | Maintenance LTS | 2027-04-30 |
| **24 "Krypton"** | **v24.22.1** | Maintenance LTS since 2026-10-20 (newest LTS today) | 2028-04-30 |
| 26 | v26.12.1 | Current; **becomes LTS on 2026-10-28**, Maintenance 2027-10-20 | 2029-04-30 |

The suite (normal mode, `--rust-hub`, the JS fallback) passes on 24.22.1; 26.12.1 was run in normal mode too (results in
the T-304 commit note). Node 22 keeps working: nothing in the code needs a newer Node than 22 (no engines check, the
launchers only test that `node` exists), so AI hosts on 22 stay supported.

Downloaded and verified (linux-x64, SHASUMS256.txt from nodejs.org, and its GPG signature against the nodejs/release-keys
keyring: good signature by the release key 5BE8A3F6C8A5C01D 106C0AD820B1A390 B168D356):

| File | sha256 (in groups of 16: join them, compare with SHASUMS256.txt) |
|---|---|
| node-v24.22.1-linux-x64.tar.xz | 6c172a9ab6718f7b 801fb5a7bd486af2 ee5e32758086a04b 84fcce2aa77095e6 |
| node-v24.22.1-win-x64.zip | d2046d28c2c3dcd2 11ac5145c0bf30e2 047ef9dfb306693c 8c6e2eccd8b95fa5 |
| node-v26.12.1-linux-x64.tar.xz | 6170b2d474e1c626 f43cff3adc118df9 fe3b573258a4fc3d 850c8dbc6b079bbd |
| node-v26.12.1-win-x64.zip | b3efd5c70cba3308 ed46e7e699a9b8aa c075bbc6b027fcb3 9c84c31acc25e2b4 |

Unpacked under `/srv/apps/VibeRoller/node/`: `node-v24.22.1-linux-x64`, `node-v26.12.1-linux-x64`, next to
`node-v22.23.3-linux-x64` (`current` still points at the old one until the go-live).

## Where Node is chosen

- `/srv/apps/VibeRoller/node/current` (a symlink): the hub service (`deploy/viberoller-hub.service`: `PATH` and `ExecStart`),
  the auto-deploy service (`deploy/viberoller-autodeploy.service`: `node --check`), and `native/build.sh` all use it.
- `deploy/setup-server.sh`: for a NEW server only; takes a system Node from 22 up, else downloads the newest of the line
  `VR_NODE_LINE` (default 24) and now verifies the tarball against the published SHASUMS256.txt.
- `run.js` starts the hub, the supervisor's JavaScript hub and every MCP helper with `process.execPath`: they all run on
  whatever Node `run.js` itself runs on.
- AI hosts and the PC launchers (`start*.sh`, `start*.bat`, the service installers) use whatever `node` is on the PATH.

## Go-live steps (at a quiet moment, with Sponge)

1. Merge and go live as usual first (this branch has no code change that needs the new Node); check the hub is healthy.
2. Quiet moment: nobody mid-reply (`hub_status` / Work runs). Note the old target: `readlink /srv/apps/VibeRoller/node/current`.
3. Flip: `ln -sfn node-v24.22.1-linux-x64 /srv/apps/VibeRoller/node/current` (relative target, as the old link).
   Keep `node-v22.23.3-linux-x64` where it is: it is the rollback.
4. A graceful restart is NOT enough: `run.js` re-spawns the hub with its own `process.execPath`, so it stays on the old
   Node until `run.js` itself restarts. Restart the service: `systemctl --user restart viberoller-hub` (a few seconds
   down; the AI hosts and browsers reconnect by themselves).
5. Check: `curl -s http://127.0.0.1:7777/api/status`, `/proc/<pid>/exe` of the hub (or the log's first line) shows
   24.22.1, then `claude` Work run, chat, a Rust hub path (`/api/status` answered by the Rust front if rustHub is on).
6. The auto-deploy picks the new Node up by itself on its next run (its `PATH` is `current/bin`). Its rollback
   (`git reset` to the last good commit + graceful restart) is about CODE and keeps working under any Node. It does not
   change the Node: if Node itself is the trouble, roll back by hand (step 7). A deploy that straddles the flip is safe:
   the verify step only needs `/api/status` to answer.
7. Rollback: `ln -sfn node-v22.23.3-linux-x64 /srv/apps/VibeRoller/node/current; systemctl --user restart viberoller-hub`.
   Delete the old directory only after a week of calm.
8. Later Node updates are the same: unpack the new directory, flip, restart the service.

## AI hosts (PCs)

They run the node the owner installed and keep working on 22 or newer; nothing forces a change. To move a host: install
the same line from nodejs.org (the installer on Windows replaces the old one), then restart the service (the heart /
knife, or `install-ai-host-service`'s service restart). A host's own restart on a code change does not change its Node.

## Windows

Windows hosts and the Desktop launchers use the Node found on the PATH (`where node`). Install **node-v24.22.1-x64.msi**
from nodejs.org (or unzip `node-v24.22.1-win-x64.zip`, checksum above, and put its folder first on the PATH), then
restart the VibeRollerAIHost service (`sc stop VibeRollerAIHost` / `sc start VibeRollerAIHost`, or the knife and the
heart). The service installer stores the path of the Node it was installed with: after a move of the Node folder, run
`install-ai-host-service.cmd` again.
