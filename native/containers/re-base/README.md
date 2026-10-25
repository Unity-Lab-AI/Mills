# pgm-re-base 1.0.0: the reverse-engineering base image (T-253)

One pre-set-up container image with REA, Ghidra, a JDK and the runtime RE tools, all working. The container manager
(T-303) clones it for every RE job. Image: `localhost/pgm-re-base:1.0.0`, also `:latest`. Size: about 2.25 GB
(as reported by podman; a build also keeps its cached stage layers until pruned).

## What is inside

| Part | Version | Where | Pinned and checked by |
|---|---|---|---|
| Base | Debian 13 "trixie-slim" (13.7) | | image digest in the Containerfile; apt from a dated snapshot.debian.org (20261020T213255Z), signed by Debian's keyring |
| Node.js | 24.22.1 | /opt/node | SHA-256 from nodejs.org SHASUMS256.txt |
| REA (`rea-agents`, github.com/morluto/rea, MIT) | 6.8.0 | /opt/rea | npm lockfile with sha512 integrity (rea-agents: `sha512-9QDeRnGq...ZPyZ9ww==`); `npm ci --ignore-scripts` |
| Ghidra | 12.1.4 (20260921) | /opt/ghidra | SHA-256 from the GitHub release notes |
| Temurin JDK | 21.0.13+9 | /opt/jdk | SHA-256 from Adoptium |
| Frida / frida-tools | 17.24.3 / 14.11.0 | venv /opt/frida-venv, commands in /usr/local/bin | pip `--require-hashes`, all wheels hashed |
| Android platform-tools (adb) | 37.0.1 | /opt/android-platform-tools | Google's SHA-1 (repository2-3.xml) plus a SHA-256 of that file |
| radare2 | 6.2.4 | /usr | SHA-256 from the release checksums.txt (not in Debian 13) |
| LLDB 19, GDB 16, strace, ltrace, binutils, file, strings, jq, Python 3.13 + venv, unzip, 7z, xxd, procps | Debian snapshot | | snapshot date |

REA's workflow skill (`reverse-engineer-anything`: SKILL.md plus `references/`, the files `rea setup` would write) is
at `/opt/rea/skill/reverse-engineer-anything/` (also in `$REA_SKILL_DIR`). The manager loads it into a run only
when a container is up. `rea setup` was never run: no agent config exists in the image.

`GHIDRA_INSTALL_DIR=/opt/ghidra`, `JAVA_HOME=/opt/jdk` and `REA_MCP_INPUT_SCHEMA_PROFILE=compact` are set. REA's MCP
entry for a run: command `/opt/node/bin/node`, args `/opt/rea/node_modules/rea-agents/scripts/rea.mjs mcp` (or
simply `rea mcp`), started with `exec` in the container.

Folders: `/work/in` (put the samples here), `/work/out` (results), `/work/tmp` (scratch), owned by user `re`
(uid 1000, the default user). Default command `sleep infinity`; the entrypoint prints the versions, then runs the
command (`PGM_RE_QUIET=1` hides the banner). Samples for the tests are in `/opt/re-fixtures/`.
OCI labels: `org.opencontainers.image.version` and `tools.pgm.re-base.*` for every component version.

Nothing is downloaded at build time without a checksum, and nothing at container start. amd64 only for now.

## Build

Linux (podman, or `ENGINE=docker`):

    ./build.sh            # build
    ./build.sh --test     # build, then smoke test

Windows (Docker Desktop or Podman Desktop, Linux containers): the same Containerfile.

    build.cmd
    build.cmd --test      # runs /opt/re-smoke.sh inside a throwaway container, no shell needed

Build time: about 5 minutes with a warm connection (about 1 GB of downloads). To change a pin, edit the `ARG` lines in
the Containerfile (and the lockfile or `frida-requirements.txt`), bump `VERSION`, rebuild.

If a rootless podman cannot start containers because its storage sits under a path with group-only permissions
(seen on the server: storage under /srv/apps/VibeRoller/home, "OCI permission denied"), point it at storage on a
world-traversable path: `ENGINE_ARGS="--root /var/tmp/x/root --runroot /var/tmp/x/run" ./build.sh`.

## Smoke test

`./smoke.sh [image]` starts a container with `--network none`, runs the checks inside and removes it:
`rea --version`; REA MCP `initialize` + `tools/list` (counts the tools); a JavaScript/Electron static analysis of a
tiny sample app; Ghidra `analyzeHeadless` import + analysis of a tiny compiled C binary, plus
`rea doctor --provider ghidra`; frida, lldb, adb, gdb, radare2, java, strace, ltrace and the rest report their versions;
user, folders and skill checks.

## How the manager uses it

One container per job, cloned from the image (`podman run -d --name <job> --network none -v <in>:/work/in:ro
-v <out>:/work/out localhost/pgm-re-base:1.0.0`), at most three at once, the rest queued. Work goes in through
`/work/in` and results come out of `/work/out`; tools run through `podman exec`. Add network (or `--cap-add`
/ `--device` for ADB, ptrace and Frida use) only when a job asks. When the person or the agent is done, the container
is removed with `podman rm -f`: nothing persists.

## Security

REA is not a sandbox: its analysis and runtime tools run with the permissions of whoever starts them. The container
is the isolation. Run untrusted binaries only inside it, as the non-root `re` user, without network unless needed,
without host mounts other than `/work/in` and `/work/out`. Never run `rea setup` for someone (it edits agent
configs), and fetch REA only from the official sources (github.com/morluto/rea, rea.tools, npm `rea-agents`);
look-alike repos are malware lures.
