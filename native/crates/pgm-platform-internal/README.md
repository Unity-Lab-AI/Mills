# pgm-platform-internal: PGM_Platform_Internal (PsyGraphMediaPlatformInternal)

The machine, the same on Windows and Linux. Names, texts and paths come in as parameters.

- `proc`: `which` (PATH, PATHEXT on Windows), `command` / `std_command` (no window, killed on drop), `run` (success +
  output), `pid_alive`, `kill_pid`, `spawn_detached` (background, output to a log), Windows `is_admin` / `run_elevated`
- `folders`: `home`, `exe`
- `service`: a program as a service. `ServiceSpec` names it; `status`, `boot_mode`, `boot_modes`, `boot_label`,
  `set_boot("service" | "sign-in" | "user" | "off")`, `start`, `stop`, `remove_windows_service`
  (Windows service with restart-on-failure and start / stop for signed-in users; Windows Run key; systemd system or
  user unit)
- `winsvc` (Windows): `dispatch(name, body, fallback)`: run inside the Service Control Manager

## Use it in another project

```rust
use pgm_platform_internal::service::{set_boot, ServiceSpec};
let spec = ServiceSpec { name: "my-agent".into(), display: "My Agent".into(), description: "Does things".into(),
    exe: "/opt/my-agent/my-agent".into(), args: vec!["run".into()], dir: "/opt/my-agent".into(),
    log: "/opt/my-agent/agent.log".into(), log_name: "agent.log".into(), ..Default::default() };
for line in set_boot(&spec, "user") { println!("{line}"); }
```
