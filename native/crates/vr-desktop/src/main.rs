//! VibeRoller Desktop (Tauri): the VibeRoller web app in its own window, plus what only a native app can do.
//!
//! Two windows, two levels of trust:
//! - "main" shows the hub itself (its page, always the current version: it updates itself as in the browser). The hub's
//!   page may ask this app only for harmless things: its version, a desktop notification, the unread count on the tray,
//!   who is signed in (its session: the app's updates need an account), opening the Tools window, opening a link in the browser, and the AI host switch (status / start / pause / resume /
//!   stop / set up one of the person's own hosts). Nothing that reads, writes or runs anything on this computer.
//! - "tools" is this app's own page (ui/, built into the program; never loaded from the network): the local projects
//!   folder without browser permission prompts, an integrated terminal, a build / run runner, git with Forgejo sign-in,
//!   the local API proxy (127.0.0.1 → the hub's OpenAI / Anthropic API with the person's key), the AI host switch and
//!   the app's updates, and this computer as a linked machine (machine.rs: link it, serve the hub's tool calls while the app is
//!   open, or install the persistent service). So even a page on the hub that someone managed to inject script into cannot
//!   touch this computer, and cannot link it, widen its scope or start its runner: those commands are the Tools window's only.
//! A tray icon (show, Tools, the AI host on / off, free the GPU, the image engine on / off, updates, quit), native notifications, one instance at a time, close to
//! the tray, and updates signed with VibeRoller's release key (update.rs).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod aihost;
mod capture;
mod install;
mod local;
mod machine;
mod gpu;
mod proxy;
mod settings;
mod update;

use serde_json::{json, Value};
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

/// What this app is called (the window, the taskbar count, the tray): VibeRoller, or a PGM product's name when built
/// as that product's app (native/build-products.sh sets VR_APP_NAME and the product's address as the default hub).
const APP_NAME: &str = match option_env!("VR_APP_NAME") { Some(n) => n, None => "VibeRoller" };

struct App { store: settings::Store, terms: Arc<local::Terms>, runs: Arc<local::Runs>, proxy: proxy::Proxy, host_dir: String, machine: Arc<machine::MachineApp>, machine_dir: std::path::PathBuf }
type S<'a> = State<'a, App>;
type R<T> = Result<T, String>;

// ---------- for both windows ----------
#[tauri::command]
fn app_info(st: S) -> Value { let mut v = json!({ "version": update::VERSION, "target": update::TARGET, "hub": st.store.get().hub, "native": true, "os": std::env::consts::OS }); if let (Some(o), Some(c)) = (v.as_object_mut(), capture::capability().as_object()) { o.extend(c.clone()); } v }
/// `vr-desktop --smoke`: the build check opens the Tools window, whose page reports back through this command once it
/// has loaded and reached the app (then the app ends: 0 = the page, its script and the IPC all work).
static SMOKE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SMOKE_MAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SMOKE_SEEN: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());
#[tauri::command]
fn notify(app: AppHandle, title: String, body: String) -> R<()> {
    if SMOKE.load(std::sync::atomic::Ordering::SeqCst) {
        println!("SMOKE {title} {body}");
        // done when the Tools page reported and (with --hub) the hub's page in the main window did too
        let mut seen = SMOKE_SEEN.lock().unwrap_or_else(|e| e.into_inner()); seen.insert(title.clone());
        if seen.contains("smoke-ok") && (!SMOKE_MAIN.load(std::sync::atomic::Ordering::SeqCst) || seen.contains("smoke-main")) { app.exit(0); }
        return Ok(());
    }
    use tauri_plugin_notification::NotificationExt;
    app.notification().builder().title(title.chars().take(120).collect::<String>()).body(body.chars().take(400).collect::<String>()).show().map_err(|e| e.to_string())
}
#[tauri::command]
fn set_badge(app: AppHandle, count: u32) {
    let title = if count > 0 { format!("({count}) {APP_NAME}") } else { APP_NAME.into() };
    if let Some(w) = app.get_webview_window("main") { let _ = w.set_title(&title); let _ = w.set_badge_count(if count > 0 { Some(count as i64) } else { None }); }
    if let Some(t) = app.tray_by_id("main") { let _ = t.set_tooltip(Some(if count > 0 { format!("{APP_NAME}: {count} unread") } else { APP_NAME.into() })); }
}
/// The hub's page: who is signed in ('' = signed out). Updates need an account (4.0.0); the session only ever goes to
/// this app's hub, and the releases stay checked against the release key built into this program whoever's session it is.
#[tauri::command]
fn set_session(st: S, token: String) -> R<()> { st.store.set_session(&token) }
#[tauri::command]
fn open_tools(app: AppHandle) -> R<()> { show_tools(&app).map_err(|e| e.to_string()) }
#[tauri::command]
fn open_external(url: String) -> R<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) { return Err("Only web addresses open in the browser.".into()); }
    let r = if cfg!(windows) { { #[allow(unused_mut)] let mut c = std::process::Command::new("rundll32"); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c }.args(["url.dll,FileProtocolHandler", &url]).spawn() } else if cfg!(target_os = "macos") { { #[allow(unused_mut)] let mut c = std::process::Command::new("open"); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c }.arg(&url).spawn() } else { { #[allow(unused_mut)] let mut c = std::process::Command::new("xdg-open"); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c }.arg(&url).spawn() };
    r.map(|_| ()).map_err(|e| e.to_string())
}
/// The person's own Ollama (OLLAMA_HOST, else 127.0.0.1:11434), asked from here so no browser CORS limit applies (T-208).
#[tauri::command]
async fn ollama_local() -> Value {
    match tokio::task::spawn_blocking(pgm_ai_internal::host::runner::probe_ollama_blocking).await.ok().flatten() {
        Some((url, models)) => json!({ "found": true, "url": url, "models": models }),
        None => json!({ "found": false, "models": [] }),
    }
}
#[tauri::command]
fn host_status(st: S) -> Value { aihost::status(&st.host_dir) }
#[tauri::command]
fn host_act(st: S, what: String) -> R<String> { aihost::act(&st.host_dir, &what) }
// the GPU switches (gpu.rs): what holds it, free it, the image engine on / off
#[tauri::command]
async fn gpu_status(st: S<'_>) -> R<Value> { Ok(gpu::status(&st.host_dir).await) }
#[tauri::command]
async fn gpu_act(st: S<'_>, what: String) -> R<String> {
    match what.as_str() { "free" => gpu::free(&st.host_dir).await, "image-on" => gpu::image(&st.host_dir, true).await, "image-off" => gpu::image(&st.host_dir, false).await, _ => Err("unknown GPU action".into()) }
}
#[tauri::command]
fn host_setup(st: S, config: Value) -> R<String> {
    // only a host of this app's own hub
    let hub = st.store.get().hub;
    if config.get("hub").and_then(Value::as_str).map(|h| h.trim_end_matches('/') != hub.trim_end_matches('/')).unwrap_or(true) { return Err("That host belongs to another hub.".into()); }
    let msg = aihost::setup(&st.host_dir, &config)?;
    let _ = aihost::act(&st.host_dir, "start");
    Ok(msg)
}

// ---------- the Tools window only ----------
// This computer as a linked machine (machine.rs). None of these is in the hub capability.
#[tauri::command]
fn machine_status(st: S) -> Value { st.machine.status(&st.machine_dir, &st.store.get().hub) }
/// "Link this computer": shows a code to approve on the web, signed in as yourself; once approved the while-open runner starts.
#[tauri::command]
async fn machine_link(app: AppHandle, folder: String, name: String) -> R<String> {
    tokio::task::spawn_blocking(move || {
        let st = app.state::<App>();
        let hub = st.store.get().hub;
        let (m, dir, app2) = (st.machine.clone(), st.machine_dir.clone(), app.clone());
        let hub2 = hub.clone();
        st.machine.link(&hub, &st.machine_dir, &folder, &name, Arc::new(move || {
            let st = app2.state::<App>();
            let _ = st.store.patch(json!({ "machineOn": true }));
            let _ = m.start(&hub2, &dir);
        }))
    }).await.map_err(|e| e.to_string())?
}
#[tauri::command]
fn machine_act(st: S, what: String) -> R<Value> {
    let hub = st.store.get().hub;
    match what.as_str() {
        "start" => { st.machine.start(&hub, &st.machine_dir)?; let _ = st.store.patch(json!({ "machineOn": true })); }
        "stop" => { st.machine.stop(); let _ = st.store.patch(json!({ "machineOn": false })); }
        _ => return Err("unknown action".into()),
    }
    Ok(st.machine.status(&st.machine_dir, &hub))
}
/// "Keep it running when the app is closed" / remove it again (the bundled vr-host's install subcommand).
#[tauri::command]
async fn machine_service(app: AppHandle, install: bool) -> R<String> {
    tokio::task::spawn_blocking(move || {
        let st = app.state::<App>();
        let r = st.machine.service(&st.machine_dir, install);
        if install && r.is_ok() { let _ = st.store.patch(json!({ "machineOn": false })); }
        r
    }).await.map_err(|e| e.to_string())?
}
#[tauri::command]
fn settings_get(st: S) -> Value { let mut v = serde_json::to_value(st.store.public()).unwrap_or(json!({})); v["hostDir"] = json!(st.host_dir); v }
#[tauri::command]
fn settings_set(st: S, patch: Value) -> R<Value> { st.store.patch(patch)?; Ok(settings_get(st)) }
#[tauri::command]
async fn pick_folder(app: AppHandle) -> R<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |p| { let _ = tx.send(p.and_then(|p| p.into_path().ok()).map(|p| p.display().to_string())); });
    rx.await.map_err(|e| e.to_string())
}
fn root(st: &S) -> String { st.store.get().projects }
#[tauri::command] fn fs_list(st: S, path: String) -> R<Value> { local::fs_list(&root(&st), &path) }
#[tauri::command] fn fs_read(st: S, path: String) -> R<Value> { local::fs_read(&root(&st), &path) }
#[tauri::command] fn fs_write(st: S, path: String, text: String) -> R<()> { local::fs_write(&root(&st), &path, &text) }
#[tauri::command] fn fs_delete(st: S, path: String) -> R<()> { local::fs_delete(&root(&st), &path) }
#[tauri::command] fn fs_mkdir(st: S, path: String) -> R<()> { local::fs_mkdir(&root(&st), &path) }
#[tauri::command] fn term_open(app: AppHandle, st: S, path: String, cols: u16, rows: u16) -> R<u32> { local::term_open(&app, &st.terms, &root(&st), &path, cols, rows) }
#[tauri::command] fn term_write(st: S, id: u32, data: String) -> R<()> { local::term_write(&st.terms, id, &data) }
#[tauri::command] fn term_resize(st: S, id: u32, cols: u16, rows: u16) -> R<()> { local::term_resize(&st.terms, id, cols, rows) }
#[tauri::command] fn term_close(st: S, id: u32) { local::term_close(&st.terms, id) }
#[tauri::command] fn run_start(app: AppHandle, st: S, path: String, cmd: String) -> R<u32> { local::run_start(&app, &st.runs, &root(&st), &path, &cmd) }
#[tauri::command] fn run_stop(st: S, id: u32) { local::run_stop(&st.runs, id) }
#[tauri::command]
async fn git(st: S<'_>, path: String, args: Vec<String>) -> R<Value> { let s = st.store.get(); let r = s.projects.clone(); tokio::task::spawn_blocking(move || local::git(&r, &path, &args, &s.forgejo_url, &s.forgejo_token)).await.map_err(|e| e.to_string())? }
#[tauri::command]
async fn proxy_start(st: S<'_>) -> R<Value> { let s = st.store.get(); st.proxy.start(s.proxy_port, s.hub.clone(), s.proxy_key.clone()).await?; let _ = st.store.patch(json!({ "proxyOn": true })); Ok(proxy_status(st)) }
#[tauri::command]
fn proxy_stop(st: S) -> Value { st.proxy.stop(); let _ = st.store.patch(json!({ "proxyOn": false })); proxy_status(st) }
#[tauri::command]
fn proxy_status(st: S) -> Value { let p = *st.proxy.port.lock().unwrap_or_else(|e| e.into_inner()); json!({ "running": st.proxy.running(), "port": p, "url": if p > 0 { format!("http://127.0.0.1:{p}/v1") } else { String::new() } }) }
#[tauri::command]
async fn update_check(st: S<'_>) -> R<Value> { let s = st.store.get(); let mut v = update::check(&s.hub, &s.hub_session).await?; v["installed"] = json!(install::installed_here()); Ok(v) }
#[tauri::command]
fn install_here() -> R<String> { install::install() }
#[tauri::command]
async fn update_apply(app: AppHandle, st: S<'_>) -> R<bool> { let s = st.store.get(); let r = update::apply(&s.hub, &s.hub_session).await?; if r { app.restart(); } Ok(r) }

/// Unity on the desktop: a see-through, frameless, always-on-top window over the whole screen that never takes a
/// click (everything under it keeps working), with only her in it. The hub's page passes on what Unity was told to do
/// (walk left / right / up / down, sit, wave, dance, come here, stop, hide): {action, amount?}.
#[tauri::command]
async fn companion(app: AppHandle, action: String, amount: Option<f64>, look: Option<String>) -> R<bool> {
    clog(&format!("order {action}"));
    let action: String = action.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(24).collect::<String>().to_lowercase();
    if action == "outfit" { return companion_outfit(&app, look.unwrap_or_default()); }
    if action == "show" { set_companion_on(&app, true); }
    if !companion_on(&app) { clog(&format!("off: {action} dropped")); return Ok(false); }
    if action == "hide" { if let Some(w) = app.get_webview_window("companion") { w.hide().map_err(|e| e.to_string())?; } return Ok(true); }
    let w = match app.get_webview_window("companion") {
        Some(w) => w,
        None => {
            // over the app's own window only: same place and size, kept there when it moves or resizes, above it and
            // nothing else (its child), hidden while it is minimised
            let main = app.get_webview_window("main").ok_or("the app window is not open")?;
            let w = WebviewWindowBuilder::new(&app, "companion", WebviewUrl::App("companion.html".into())).title("Unity")
                .transparent(true).decorations(false).skip_taskbar(true).shadow(false).resizable(false).focused(false).visible(false)
                .parent(&main).map_err(|e| e.to_string())?
                .build().map_err(|e| { clog(&format!("build failed: {e}")); e.to_string() })?;
            clog("window built");
            fit_companion(&app);
            let a2 = app.clone();
            main.on_window_event(move |ev| match ev {
                tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. } => fit_companion(&a2),
                _ => {}
            });
            let _ = w.set_ignore_cursor_events(true);
            w
        }
    };
    fit_companion(&app);
    let _ = w.show();
    let _ = w.set_ignore_cursor_events(true);
    use tauri::Emitter;
    // a window that was just made is still loading: it asks for the last order itself (companion.js), so keep it
    *LAST_ORDER.lock().unwrap_or_else(|e| e.into_inner()) = json!({ "action": action, "amount": amount.unwrap_or(0.0) });
    w.emit_to("companion", "companion", json!({ "action": action, "amount": amount.unwrap_or(0.0) })).map_err(|e| e.to_string())?;
    Ok(true)
}
/// Her trail: %TEMP%/vr-companion.log (what she was told and what happened).
fn clog(m: &str) { use std::io::Write; if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("vr-companion.log")) { let _ = writeln!(f, "{} {m}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)); } }
/// `vr-desktop --companion <move>`: her moves from a terminal or a script (the running app gets them).
fn companion_arg(args: &[String]) -> Option<String> { args.iter().position(|a| a == "--companion").map(|i| args.get(i + 1).cloned().unwrap_or_else(|| "show".into())) }
/// The companion window laid exactly over the app window (its inner area); hidden while the app is minimised.
fn fit_companion(app: &AppHandle) {
    let (Some(main), Some(c)) = (app.get_webview_window("main"), app.get_webview_window("companion")) else { return };
    if main.is_minimized().unwrap_or(false) { let _ = c.hide(); return; }
    if let (Ok(p), Ok(s)) = (main.inner_position(), main.inner_size()) { let _ = c.set_position(p); let _ = c.set_size(s); }
}
/// `--look <text>` with `--companion outfit`.
fn companion_look(args: &[String]) -> Option<String> { args.iter().position(|a| a == "--look").and_then(|i| args.get(i + 1).cloned()) }
/// Her clips: this computer's own (a look she was told to wear, in the app's data folder), else the ones built in.
fn companion_dir(app: &AppHandle) -> std::path::PathBuf { app.path().app_data_dir().unwrap_or_else(|_| std::env::temp_dir().join("viberoller")).join("companion") }
#[tauri::command]
fn companion_clips(app: AppHandle) -> Value {
    let dir = companion_dir(&app); let mut out = serde_json::Map::new();
    for n in ["idle", "walk", "sit", "wave", "dance"] {
        if let Ok(b) = std::fs::read(dir.join(format!("{n}.webp"))) { use base64::Engine; out.insert(n.into(), json!(format!("data:image/webp;base64,{}", base64::engine::general_purpose::STANDARD.encode(b)))); }
    }
    Value::Object(out)
}
/// A new look: companion-look.py (redraw her base, film her moves, cut them out) runs here, hidden, with the image
/// engine's Python; she keeps her look until all the new clips are there, then the window swaps them.
static DRESSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn companion_outfit(app: &AppHandle, look: String) -> R<bool> {
    let look: String = look.chars().filter(|c| !c.is_control()).take(200).collect::<String>().trim().to_string();
    if look.is_empty() { return Err("What should she wear?".into()); }
    if DRESSING.swap(true, std::sync::atomic::Ordering::SeqCst) { return Err("She is still changing; one look at a time.".into()); }
    let dir = companion_dir(app); let _ = std::fs::create_dir_all(&dir);
    let st = app.state::<App>();
    let python = gpu::start_cmd(&st.host_dir).map(|(p, _, _)| p).unwrap_or_else(|| "python".into());
    for (f, body) in [("companion-look.py", include_str!("../companion-look.py")), ("companion-clips.py", include_str!("../companion-clips.py"))] { std::fs::write(dir.join(f), body).map_err(|e| e.to_string())?; }
    let base = dir.join("base.png");
    if !base.exists() { std::fs::write(&base, include_bytes!("../ui/companion/base.png")).map_err(|e| e.to_string())?; }
    clog(&format!("outfit {look} with {python}"));
    let a = app.clone();
    std::thread::spawn(move || {
        let mut c = std::process::Command::new(&python);
        c.arg(dir.join("companion-look.py")).arg("--look").arg(&look).arg("--base").arg(&base).arg("--out").arg(&dir).current_dir(&dir)
            .stdout(std::process::Stdio::null()).stderr(std::fs::File::create(dir.join("look.log")).map(std::process::Stdio::from).unwrap_or(std::process::Stdio::null()));
        #[cfg(windows)] { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
        let ok = c.status().map(|s| s.success()).unwrap_or(false);
        clog(&format!("outfit done: {ok}"));
        DRESSING.store(false, std::sync::atomic::Ordering::SeqCst);
        use tauri::Emitter;
        let _ = a.emit_to("companion", "companion-clips", json!({ "ok": ok }));
    });
    Ok(true)
}
/// Desktop Unity on or off, remembered between starts (companion/on.json); off hides her and drops her orders.
fn companion_on(app: &AppHandle) -> bool { std::fs::read_to_string(companion_dir(app).join("on.json")).map(|t| t.trim() != "false").unwrap_or(true) }
fn set_companion_on(app: &AppHandle, on: bool) { let d = companion_dir(app); let _ = std::fs::create_dir_all(&d); let _ = std::fs::write(d.join("on.json"), if on { "true" } else { "false" }); }
#[tauri::command]
async fn companion_set(app: AppHandle, on: bool) -> R<bool> {
    set_companion_on(&app, on);
    if on { companion(app, "show".into(), None, None).await } else { if let Some(w) = app.get_webview_window("companion") { let _ = w.hide(); } Ok(false) }
}
/// Her state for the Tools tab: on, what she looks like now, a look being made (its step) and the last error.
#[tauri::command]
fn companion_status(app: AppHandle) -> Value {
    let d = companion_dir(&app);
    let st: Value = std::fs::read_to_string(d.join("status.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let err = std::fs::read_to_string(d.join("look.log")).ok().map(|t| t.lines().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")).unwrap_or_default();
    json!({ "on": companion_on(&app), "dressing": DRESSING.load(std::sync::atomic::Ordering::SeqCst), "status": st, "log": err })
}
/// The overlay's own trail (what it loaded, what failed) into vr-companion.log.
#[tauri::command]
fn companion_note(m: String) { clog(&format!("page: {}", m.chars().take(400).collect::<String>())); }
static LAST_ORDER: std::sync::Mutex<Value> = std::sync::Mutex::new(Value::Null);
#[tauri::command]
fn companion_last() -> Value { std::mem::take(&mut *LAST_ORDER.lock().unwrap_or_else(|e| e.into_inner())) }

fn show_main(app: &AppHandle) { if let Some(w) = app.get_webview_window("main") { let _ = w.show(); let _ = w.unminimize(); let _ = w.set_focus(); } }
fn show_tools(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window("tools") { w.show()?; w.set_focus()?; return Ok(()); }
    WebviewWindowBuilder::new(app, "tools", WebviewUrl::App("tools.html".into())).title("VibeRoller Tools").inner_size(1100.0, 760.0).min_inner_size(640.0, 420.0).build()?;
    Ok(())
}

fn main() {
    // `<app> machine run --state <dir>`: the persistent mode (the service starts it), no window
    if let Some(code) = machine::cli(&std::env::args().collect::<Vec<_>>()) { std::process::exit(code); }
    // vr-desktop --gpu status | free | image-on | image-off: the tray's GPU switches from a terminal or a script
    { let args: Vec<String> = std::env::args().collect();
      if let Some(what) = args.iter().position(|a| a == "--gpu").map(|i| args.get(i + 1).cloned().unwrap_or_else(|| "status".into())) {
        let dir = aihost::standard_dir().display().to_string();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio");
        let r = rt.block_on(async { match what.as_str() { "status" => Ok(gpu::status(&dir).await.to_string()), "free" => gpu::free(&dir).await, "image-on" => gpu::image(&dir, true).await, "image-off" => gpu::image(&dir, false).await, _ => Err("vr-desktop --gpu status | free | image-on | image-off".into()) } });
        match r { Ok(s) => { println!("{s}"); std::process::exit(0) } Err(e) => { eprintln!("{e}"); std::process::exit(1) } }
      } }
    if std::env::args().any(|a| a == "--selftest") { println!("vr-desktop {} ({}) loads", update::VERSION, update::TARGET); return; }
    if std::env::args().any(|a| a == "--install") { match install::install() { Ok(m) => { println!("{m}"); return; } Err(e) => { eprintln!("{e}"); std::process::exit(1); } } }
    let smoke = std::env::args().any(|a| a == "--smoke");
    SMOKE.store(smoke, std::sync::atomic::Ordering::SeqCst);
    if smoke { std::thread::spawn(|| { std::thread::sleep(std::time::Duration::from_secs(40)); eprintln!("SMOKE timeout: the Tools page never reported"); std::process::exit(1); }); }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| { clog(&format!("second start {args:?}")); if let Some(act) = companion_arg(&args) { let a = app.clone(); tauri::async_runtime::spawn(async move { if let Err(e) = companion(a, act, None, companion_look(&args)).await { eprintln!("companion: {e}"); } }); } else { show_main(app) } }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![companion, companion_last, companion_clips, companion_set, companion_status, companion_note, app_info, notify, set_badge, set_session, open_tools, open_external, host_status, ollama_local, host_act, gpu_status, gpu_act, host_setup, settings_get, settings_set, pick_folder,
            fs_list, fs_read, fs_write, fs_delete, fs_mkdir, term_open, term_write, term_resize, term_close, run_start, run_stop, git, proxy_start, proxy_stop, proxy_status, update_check, update_apply, install_here, machine_status, machine_link, machine_act, machine_service])
        .setup(|app| {
            let dir = app.path().app_config_dir().unwrap_or_else(|_| std::env::temp_dir().join("viberoller"));
            let store = settings::Store::load(dir.clone());
            // --hub <address>: this run only (the build check points it at a test hub)
            if let Some(h) = std::env::args().skip_while(|a| a != "--hub").nth(1) { store.s.lock().unwrap_or_else(|e| e.into_inner()).hub = h; SMOKE_MAIN.store(SMOKE.load(std::sync::atomic::Ordering::SeqCst), std::sync::atomic::Ordering::SeqCst); }
            let s = store.get();
            // the AI host's folder: the setting, else the standard install of vr-host when it holds a host (Windows
            // %LOCALAPPDATA%VibeRollerAIHost, Linux ~/.local/share/viberoller/ai-host), else this app's own folder
            let host_dir = if !s.host_dir.is_empty() { s.host_dir.clone() } else if aihost::config(&aihost::standard_dir().display().to_string()).exists() { aihost::standard_dir().display().to_string() } else { app.path().app_data_dir().unwrap_or(dir).join("ai-host").display().to_string() };
            let machine_dir = machine::state_dir(&app.path().app_data_dir().unwrap_or_else(|_| std::env::temp_dir().join("viberoller")));
            app.manage(App { store, terms: Arc::default(), runs: Arc::default(), proxy: proxy::Proxy::default(), host_dir, machine: Arc::default(), machine_dir: machine_dir.clone() });
            // the while-open runner comes back by itself when this computer is linked and the person has not paused it
            if s.machine_on && !SMOKE.load(std::sync::atomic::Ordering::SeqCst) { let _ = app.state::<App>().machine.start(&s.hub, &machine_dir); }
            let hub = s.hub.trim_end_matches('/').to_string();
            // the hub's page: only the harmless commands (see the top of this file)
            app.add_capability(tauri::ipc::CapabilityBuilder::new("hub").remote(format!("{hub}/*")).window("main")
                .permission("core:event:default").permission("allow-app-info").permission("allow-notify").permission("allow-set-badge").permission("allow-set-session").permission("allow-open-tools")
                .permission("allow-open-external").permission("allow-host-status").permission("allow-host-act").permission("allow-host-setup").permission("allow-companion").permission("allow-companion-set").permission("allow-companion-status"))?;
            let url: tauri::Url = hub.parse().map_err(|e| format!("bad hub address {hub}: {e}"))?;
            let host = url.host_str().unwrap_or("").to_string();
            let hub_for_perm = hub.clone();
            let main_win = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url)).title(APP_NAME).inner_size(1320.0, 880.0).min_inner_size(420.0, 520.0)
                // screen sharing / microphone / camera: only for the hub's own page (capture.rs); the Tools window has no handler
                .on_permission_request(move |wv, kind| capture::decide(&kind, wv.url().map(|u| u.to_string()).unwrap_or_default().as_str(), &hub_for_perm))
                // links to other sites open in the browser, not in the app
                .on_navigation(move |u| { let same = u.host_str() == Some(host.as_str()) || u.scheme() == "tauri" || u.scheme() == "about" || u.scheme() == "blob" || u.scheme() == "data"; if !same && (u.scheme() == "https" || u.scheme() == "http") { let _ = open_external(u.to_string()); } same })
                .build()?;
            capture::enable_in_webview(&main_win);
            // the tray
            let h = app.handle();
            let show = MenuItem::with_id(h, "show", "Show VibeRoller", true, None::<&str>)?;
            let tools = MenuItem::with_id(h, "tools", "Tools (files, terminal, builds, git)", true, None::<&str>)?;
            let host_on = MenuItem::with_id(h, "host-resume", "AI host: share this PC's AI", true, None::<&str>)?;
            let host_off = MenuItem::with_id(h, "host-pause", "AI host: pause", true, None::<&str>)?;
            let gpu_free = MenuItem::with_id(h, "gpu-free", "Free the GPU (unload the AI models)", true, None::<&str>)?;
            let img_on = MenuItem::with_id(h, "image-on", "Image engine: on", true, None::<&str>)?;
            let img_off = MenuItem::with_id(h, "image-off", "Image engine: off", true, None::<&str>)?;
            let uni_on = MenuItem::with_id(h, "unity-on", "Desktop Unity: on", true, None::<&str>)?;
            let uni_off = MenuItem::with_id(h, "unity-off", "Desktop Unity: off", true, None::<&str>)?;
            let uni_set = MenuItem::with_id(h, "unity-settings", "Desktop Unity: her look and settings…", true, None::<&str>)?;
            let upd = MenuItem::with_id(h, "update", "Check for updates", true, None::<&str>)?;
            let quit = MenuItem::with_id(h, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(h, &[&show, &tools, &PredefinedMenuItem::separator(h)?, &host_on, &host_off, &PredefinedMenuItem::separator(h)?, &gpu_free, &img_on, &img_off, &PredefinedMenuItem::separator(h)?, &uni_on, &uni_off, &uni_set, &PredefinedMenuItem::separator(h)?, &upd, &quit])?;
            TrayIconBuilder::with_id("main").icon(app.default_window_icon().cloned().ok_or("no icon")?).tooltip(APP_NAME).menu(&menu).show_menu_on_left_click(false)
                .on_menu_event(|app, ev| {
                    let st = app.state::<App>();
                    match ev.id.as_ref() {
                        "show" => show_main(app),
                        "tools" => { let _ = show_tools(app); }
                        "host-resume" => { let d = st.host_dir.clone(); let _ = aihost::act(&d, if aihost::status(&d)["running"].as_bool() == Some(true) { "resume" } else { "start" }); }
                        "host-pause" => { let _ = aihost::act(&st.host_dir, "pause"); }
                        "gpu-free" | "image-on" | "image-off" => {
                            let (d, what, a) = (st.host_dir.clone(), ev.id.as_ref().to_string(), app.clone());
                            tauri::async_runtime::spawn(async move {
                                let r = match what.as_str() { "gpu-free" => gpu::free(&d).await, "image-on" => gpu::image(&d, true).await, _ => gpu::image(&d, false).await };
                                let _ = notify(a, APP_NAME.to_string(), r.unwrap_or_else(|e| e));
                            });
                        }
                        "update" => { let _ = show_tools(app); }
                        "unity-on" | "unity-off" => { let (a, on) = (app.clone(), ev.id.as_ref() == "unity-on"); tauri::async_runtime::spawn(async move { let _ = companion_set(a, on).await; }); }
                        "unity-settings" => { let _ = show_tools(app); if let Some(w) = app.get_webview_window("tools") { let _ = w.eval("window.openTab && openTab('unity')"); } }
                        "quit" => app.exit(0),
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, ev| { if let tauri::tray::TrayIconEvent::Click { button: tauri::tray::MouseButton::Left, .. } = ev { show_main(tray.app_handle()); } })
                .build(app)?;
            if SMOKE.load(std::sync::atomic::Ordering::SeqCst) {
                WebviewWindowBuilder::new(app, "tools", WebviewUrl::App("tools.html#smoke".into())).title("VibeRoller Tools").build()?;
                if SMOKE_MAIN.load(std::sync::atomic::Ordering::SeqCst) {
                    // the hub's page: it must see the app (window.__TAURI__) and be allowed what the hub capability allows
                    let h2 = app.handle().clone();
                    std::thread::spawn(move || for _ in 0..15 {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        if let Some(w) = h2.get_webview_window("main") { let _ = w.eval("if (window.__TAURI__ && window.__TAURI__.core && typeof NATIVE !== 'undefined' && NATIVE) window.__TAURI__.core.invoke('app_info').then((i) => window.__TAURI__.core.invoke('notify', { title: 'smoke-main', body: location.host + ' ' + i.version + ' tools-tile ' + !document.getElementById('goTools').hidden + ' share-field ' + (typeof i.screenShare) }))"); }
                    });
                }
            }
            if companion_dir(app.handle()).join("on.json").exists() && companion_on(app.handle()) { let a = app.handle().clone(); tauri::async_runtime::spawn(async move { tokio::time::sleep(std::time::Duration::from_secs(6)).await; let _ = companion(a, "show".into(), None, None).await; }); }
            // the local API proxy comes back on by itself when it was on
            if s.proxy_on { let h2 = app.handle().clone(); tauri::async_runtime::spawn(async move { let st = h2.state::<App>(); let s = st.store.get(); let _ = st.proxy.start(s.proxy_port, s.hub, s.proxy_key).await; }); }
            // updates: at start (once someone is signed in on the hub's page) and every 6 hours, applied when nothing
            // local is running
            if s.auto_update {
                let h3 = app.handle().clone();
                tauri::async_runtime::spawn(async move { loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    let st = h3.state::<App>();
                    let s = st.store.get();
                    if s.hub_session.is_empty() { continue; } // updates need an account: look again in a minute
                    let idle = st.terms.map.lock().unwrap_or_else(|e| e.into_inner()).is_empty() && st.runs.map.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
                    if idle { if let Ok(true) = update::apply(&s.hub, &s.hub_session).await { h3.restart(); } }
                    tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
                } });
            }
            Ok(())
        })
        .on_window_event(|w, ev| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = ev {
                if w.label() == "main" && w.app_handle().state::<App>().store.get().close_to_tray { api.prevent_close(); let _ = w.hide(); }
            }
        })
        .run(tauri::generate_context!())
        .expect("VibeRoller Desktop could not start");
}
