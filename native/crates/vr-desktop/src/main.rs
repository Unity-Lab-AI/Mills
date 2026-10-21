//! VibeRoller Desktop (Tauri): the VibeRoller web app in its own window, plus what only a native app can do.
//!
//! Two windows, two levels of trust:
//! - "main" shows the hub itself (its page, always the current version: it updates itself as in the browser). The hub's
//!   page may ask this app only for harmless things: its version, a desktop notification, the unread count on the tray,
//!   opening the Tools window, opening a link in the browser, and the AI host switch (status / start / pause / resume /
//!   stop / set up one of the person's own hosts). Nothing that reads, writes or runs anything on this computer.
//! - "tools" is this app's own page (ui/, built into the program; never loaded from the network): the local projects
//!   folder without browser permission prompts, an integrated terminal, a build / run runner, git with Forgejo sign-in,
//!   the local API proxy (127.0.0.1 → the hub's OpenAI / Anthropic API with the person's key), the AI host switch and
//!   the app's updates. So even a page on the hub that someone managed to inject script into cannot touch this computer.
//! A tray icon (show, Tools, the AI host on / off, updates, quit), native notifications, one instance at a time, close to
//! the tray, and updates signed with VibeRoller's release key (update.rs).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod aihost;
mod install;
mod local;
mod proxy;
mod settings;
mod update;

use serde_json::{json, Value};
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

struct App { store: settings::Store, terms: Arc<local::Terms>, runs: Arc<local::Runs>, proxy: proxy::Proxy, host_dir: String }
type S<'a> = State<'a, App>;
type R<T> = Result<T, String>;

// ---------- for both windows ----------
#[tauri::command]
fn app_info(st: S) -> Value { json!({ "version": update::VERSION, "target": update::TARGET, "hub": st.store.get().hub, "native": true, "os": std::env::consts::OS }) }
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
        let mut seen = SMOKE_SEEN.lock().unwrap(); seen.insert(title.clone());
        if seen.contains("smoke-ok") && (!SMOKE_MAIN.load(std::sync::atomic::Ordering::SeqCst) || seen.contains("smoke-main")) { app.exit(0); }
        return Ok(());
    }
    use tauri_plugin_notification::NotificationExt;
    app.notification().builder().title(title.chars().take(120).collect::<String>()).body(body.chars().take(400).collect::<String>()).show().map_err(|e| e.to_string())
}
#[tauri::command]
fn set_badge(app: AppHandle, count: u32) {
    let title = if count > 0 { format!("({count}) VibeRoller") } else { "VibeRoller".into() };
    if let Some(w) = app.get_webview_window("main") { let _ = w.set_title(&title); let _ = w.set_badge_count(if count > 0 { Some(count as i64) } else { None }); }
    if let Some(t) = app.tray_by_id("main") { let _ = t.set_tooltip(Some(if count > 0 { format!("VibeRoller: {count} unread") } else { "VibeRoller".into() })); }
}
#[tauri::command]
fn open_tools(app: AppHandle) -> R<()> { show_tools(&app).map_err(|e| e.to_string()) }
#[tauri::command]
fn open_external(url: String) -> R<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) { return Err("Only web addresses open in the browser.".into()); }
    let r = if cfg!(windows) { std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", &url]).spawn() } else if cfg!(target_os = "macos") { std::process::Command::new("open").arg(&url).spawn() } else { std::process::Command::new("xdg-open").arg(&url).spawn() };
    r.map(|_| ()).map_err(|e| e.to_string())
}
#[tauri::command]
fn host_status(st: S) -> Value { aihost::status(&st.host_dir) }
#[tauri::command]
fn host_act(st: S, what: String) -> R<String> { aihost::act(&st.host_dir, &what) }
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
fn proxy_status(st: S) -> Value { let p = *st.proxy.port.lock().unwrap(); json!({ "running": st.proxy.running(), "port": p, "url": if p > 0 { format!("http://127.0.0.1:{p}/v1") } else { String::new() } }) }
#[tauri::command]
async fn update_check(st: S<'_>) -> R<Value> { let mut v = update::check(&st.store.get().hub).await?; v["installed"] = json!(install::installed_here()); Ok(v) }
#[tauri::command]
fn install_here() -> R<String> { install::install() }
#[tauri::command]
async fn update_apply(app: AppHandle, st: S<'_>) -> R<bool> { let r = update::apply(&st.store.get().hub).await?; if r { app.restart(); } Ok(r) }

fn show_main(app: &AppHandle) { if let Some(w) = app.get_webview_window("main") { let _ = w.show(); let _ = w.unminimize(); let _ = w.set_focus(); } }
fn show_tools(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window("tools") { w.show()?; w.set_focus()?; return Ok(()); }
    WebviewWindowBuilder::new(app, "tools", WebviewUrl::App("tools.html".into())).title("VibeRoller Tools").inner_size(1100.0, 760.0).min_inner_size(640.0, 420.0).build()?;
    Ok(())
}

fn main() {
    if std::env::args().any(|a| a == "--selftest") { println!("vr-desktop {} ({}) loads", update::VERSION, update::TARGET); return; }
    if std::env::args().any(|a| a == "--install") { match install::install() { Ok(m) => { println!("{m}"); return; } Err(e) => { eprintln!("{e}"); std::process::exit(1); } } }
    let smoke = std::env::args().any(|a| a == "--smoke");
    SMOKE.store(smoke, std::sync::atomic::Ordering::SeqCst);
    if smoke { std::thread::spawn(|| { std::thread::sleep(std::time::Duration::from_secs(40)); eprintln!("SMOKE timeout: the Tools page never reported"); std::process::exit(1); }); }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![app_info, notify, set_badge, open_tools, open_external, host_status, host_act, host_setup, settings_get, settings_set, pick_folder,
            fs_list, fs_read, fs_write, fs_delete, fs_mkdir, term_open, term_write, term_resize, term_close, run_start, run_stop, git, proxy_start, proxy_stop, proxy_status, update_check, update_apply, install_here])
        .setup(|app| {
            let dir = app.path().app_config_dir().unwrap_or_else(|_| std::env::temp_dir().join("viberoller"));
            let store = settings::Store::load(dir.clone());
            // --hub <address>: this run only (the build check points it at a test hub)
            if let Some(h) = std::env::args().skip_while(|a| a != "--hub").nth(1) { store.s.lock().unwrap().hub = h; SMOKE_MAIN.store(SMOKE.load(std::sync::atomic::Ordering::SeqCst), std::sync::atomic::Ordering::SeqCst); }
            let s = store.get();
            let host_dir = if s.host_dir.is_empty() { app.path().app_data_dir().unwrap_or(dir).join("ai-host").display().to_string() } else { s.host_dir.clone() };
            app.manage(App { store, terms: Arc::default(), runs: Arc::default(), proxy: proxy::Proxy::default(), host_dir });
            let hub = s.hub.trim_end_matches('/').to_string();
            // the hub's page: only the harmless commands (see the top of this file)
            app.add_capability(tauri::ipc::CapabilityBuilder::new("hub").remote(format!("{hub}/*")).window("main")
                .permission("core:event:default").permission("allow-app-info").permission("allow-notify").permission("allow-set-badge").permission("allow-open-tools")
                .permission("allow-open-external").permission("allow-host-status").permission("allow-host-act").permission("allow-host-setup"))?;
            let url: tauri::Url = hub.parse().map_err(|e| format!("bad hub address {hub}: {e}"))?;
            let host = url.host_str().unwrap_or("").to_string();
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url)).title("VibeRoller").inner_size(1320.0, 880.0).min_inner_size(420.0, 520.0)
                // links to other sites open in the browser, not in the app
                .on_navigation(move |u| { let same = u.host_str() == Some(host.as_str()) || u.scheme() == "tauri" || u.scheme() == "about" || u.scheme() == "blob" || u.scheme() == "data"; if !same && (u.scheme() == "https" || u.scheme() == "http") { let _ = open_external(u.to_string()); } same })
                .build()?;
            // the tray
            let h = app.handle();
            let show = MenuItem::with_id(h, "show", "Show VibeRoller", true, None::<&str>)?;
            let tools = MenuItem::with_id(h, "tools", "Tools (files, terminal, builds, git)", true, None::<&str>)?;
            let host_on = MenuItem::with_id(h, "host-resume", "AI host: share this PC's AI", true, None::<&str>)?;
            let host_off = MenuItem::with_id(h, "host-pause", "AI host: pause", true, None::<&str>)?;
            let upd = MenuItem::with_id(h, "update", "Check for updates", true, None::<&str>)?;
            let quit = MenuItem::with_id(h, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(h, &[&show, &tools, &PredefinedMenuItem::separator(h)?, &host_on, &host_off, &PredefinedMenuItem::separator(h)?, &upd, &quit])?;
            TrayIconBuilder::with_id("main").icon(app.default_window_icon().cloned().ok_or("no icon")?).tooltip("VibeRoller").menu(&menu).show_menu_on_left_click(false)
                .on_menu_event(|app, ev| {
                    let st = app.state::<App>();
                    match ev.id.as_ref() {
                        "show" => show_main(app),
                        "tools" => { let _ = show_tools(app); }
                        "host-resume" => { let d = st.host_dir.clone(); let _ = aihost::act(&d, if aihost::status(&d)["running"].as_bool() == Some(true) { "resume" } else { "start" }); }
                        "host-pause" => { let _ = aihost::act(&st.host_dir, "pause"); }
                        "update" => { let _ = show_tools(app); }
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
                        if let Some(w) = h2.get_webview_window("main") { let _ = w.eval("if (window.__TAURI__ && window.__TAURI__.core && typeof NATIVE !== 'undefined' && NATIVE) window.__TAURI__.core.invoke('app_info').then((i) => window.__TAURI__.core.invoke('notify', { title: 'smoke-main', body: location.host + ' ' + i.version + ' tools-tile ' + !document.getElementById('goTools').hidden }))"); }
                    });
                }
            }
            // the local API proxy comes back on by itself when it was on
            if s.proxy_on { let h2 = app.handle().clone(); tauri::async_runtime::spawn(async move { let st = h2.state::<App>(); let s = st.store.get(); let _ = st.proxy.start(s.proxy_port, s.hub, s.proxy_key).await; }); }
            // updates: at start and every 6 hours, applied when nothing local is running
            if s.auto_update {
                let h3 = app.handle().clone();
                tauri::async_runtime::spawn(async move { loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    let st = h3.state::<App>();
                    let idle = st.terms.map.lock().unwrap().is_empty() && st.runs.map.lock().unwrap().is_empty();
                    if idle { if let Ok(true) = update::apply(&st.store.get().hub).await { h3.restart(); } }
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
