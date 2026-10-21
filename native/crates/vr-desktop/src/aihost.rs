//! The desktop app's switch for hosting this computer's AI: it drives the native AI host (vr-host, shipped next to this
//! app) in its own folder, through vr-host's own commands, so the host keeps running (and updating itself) whether or
//! not the app is open. Setting it up takes one of the person's hosts from the hub (Settings → My AI hosts → "Run on
//! this PC"): its ai-node.json is written here, private.
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn host_exe() -> PathBuf {
    let me = std::env::current_exe().unwrap_or_default();
    me.with_file_name(if cfg!(windows) { "vr-host.exe" } else { "vr-host" })
}
pub fn config(dir: &str) -> PathBuf { PathBuf::from(dir).join("ai-node.json") }
fn vrhost(dir: &str, args: &[&str]) -> Result<(i32, String), String> {
    let exe = host_exe();
    if !exe.exists() { return Err(format!("The AI host program is not next to this app ({}).", exe.display())); }
    let mut c = std::process::Command::new(&exe);
    c.args(args).arg("--config").arg(config(dir)).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
    let o = c.output().map_err(|e| e.to_string())?;
    Ok((o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).trim().to_string()))
}
pub fn status(dir: &str) -> Value {
    if !config(dir).exists() { return json!({ "configured": false, "running": false, "installed": host_exe().exists() }); }
    match vrhost(dir, &["status", "--json"]) { Ok((_, out)) => serde_json::from_str(&out).unwrap_or_else(|_| json!({ "configured": true, "error": out })), Err(e) => json!({ "configured": true, "error": e }) }
}
/// "start" | "stop" | "pause" | "resume" | "restart" | "update" | "boot-user" / "boot-service" / "boot-sign-in" / "boot-off"
pub fn act(dir: &str, what: &str) -> Result<String, String> {
    let args: Vec<&str> = match what {
        "start" | "pause" | "resume" | "update" | "restart" => vec![what],
        "stop" => vec!["stop", "--now"],
        w if w.starts_with("boot-") => vec!["boot", &w[5..]],
        _ => return Err("unknown action".into()),
    };
    vrhost(dir, &args).map(|(_, out)| out)
}
/// Writes the host's settings from the hub (one of this person's own hosts), keeping the folder private.
pub fn setup(dir: &str, cfg: &Value) -> Result<String, String> {
    let o = cfg.as_object().ok_or("not a host configuration")?;
    for k in ["hub", "id", "token"] { if o.get(k).and_then(Value::as_str).map(str::is_empty).unwrap_or(true) { return Err(format!("the configuration has no {k}")); } }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut s = vr_core::settings::HostSettings::new(config(dir));
    s.raw = o.clone();
    s.check().map_err(|e| e.to_string())?;
    s.save().map_err(|e| e.to_string())?;
    Ok(format!("This computer is set up as the AI host \"{}\".", o.get("nickname").and_then(Value::as_str).unwrap_or("")))
}
