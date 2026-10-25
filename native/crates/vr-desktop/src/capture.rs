//! Screen sharing, microphone and camera inside the app's webview (T-243, part of T-194).
//!
//! The page (web/calls.js) shares the screen with the browser's own `getDisplayMedia`; this module makes that work in
//! the webview and tells the page whether it can (`app_info.screenShare`). Only the HUB'S page in the "main" window is
//! ever allowed capture: the Tools window (the app's own page) has no permission handler at all, so a capture request
//! there is never granted, and the main window refuses it whenever its current page is not on the hub's own origin.
//!
//! Platform matrix (what the webview needs; real capture cannot be tested headless, see R165 notes):
//! - Windows, WebView2 (Evergreen runtime 100 or newer, on Windows 10 / 11): `getDisplayMedia` opens Chromium's own
//!   picker (screen / window / tab, with system audio for a screen) and needs no permission event; the microphone and
//!   camera ask through WebView2's PermissionRequested event, which the handler below answers for the hub's page. The
//!   handler also answers `DisplayCapture` should a runtime ever raise it. screenShare = true, audio = true.
//! - Linux, WebKitGTK 2.38 or newer (the "4.1" / "6.0" packages) with PipeWire and xdg-desktop-portal plus a portal
//!   backend for the desktop (gnome / kde / wlr / gtk): `getDisplayMedia` goes through the portal's ScreenCast picker
//!   (GStreamer pipewiresrc), on Wayland and (with a portal backend that supports it) on X11. WebKitGTK ships with
//!   `enable-media-stream` OFF, so this module turns it (and mediasource / webrtc) on for the main window only. The
//!   permission-request signal is answered by the handler below (display / microphone / camera). Audio of the shared
//!   screen is not offered by the portal: audio = false for the screen; the microphone works (WebKitGTK 2.38+).
//!   Without a running PipeWire (its socket in XDG_RUNTIME_DIR) screenShare = false with the reason.
//! - macOS: the app is not built or shipped for it; screenShare = false.
//! The page reads `app_info()`: `{ screenShare: bool, screenShareWhy: string, screenShareAudio: bool }` (older pages
//! ignore the fields; older apps lack them, which the page reads as "unknown, try getDisplayMedia").

use serde_json::{json, Value};
use tauri::webview::{PermissionKind, PermissionResponse};

/// The origin (scheme, host, port) of an address, lower-case; None when it is not an http(s) address.
fn origin(url: &str) -> Option<(String, String, u16)> {
    let u: tauri::Url = url.parse().ok()?;
    let scheme = u.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" { return None; }
    Some((scheme, u.host_str()?.to_ascii_lowercase(), u.port_or_known_default()?))
}

/// True when `page` is on exactly the hub's origin (same scheme, host and port: a look-alike host, another port or
/// another scheme is a different origin).
pub fn is_hub_origin(page: &str, hub: &str) -> bool {
    match (origin(page), origin(hub)) { (Some(a), Some(b)) => a == b, _ => false }
}

/// The answer to a permission request from the page now at `page`: capture (display, microphone, camera) only for the
/// hub's own page, refused for any other page; everything else is left to the webview's default (not granted).
pub fn decide(kind: &PermissionKind, page: &str, hub: &str) -> PermissionResponse {
    match kind {
        PermissionKind::DisplayCapture | PermissionKind::Microphone | PermissionKind::Camera =>
            if is_hub_origin(page, hub) { PermissionResponse::Allow } else { PermissionResponse::Deny },
        _ => PermissionResponse::Default,
    }
}

/// What the page may read: can this platform share the screen, and why not.
pub fn capability() -> Value {
    let (ok, why, audio) = platform();
    json!({ "screenShare": ok, "screenShareWhy": why, "screenShareAudio": audio })
}

#[cfg(windows)]
fn platform() -> (bool, String, bool) { (true, String::new(), true) }

#[cfg(target_os = "linux")]
fn platform() -> (bool, String, bool) {
    // SAFETY: plain version getters of the library the webview is linked against
    let (major, minor) = unsafe { (webkit2gtk::ffi::webkit_get_major_version(), webkit2gtk::ffi::webkit_get_minor_version()) };
    platform_linux(major, minor, std::env::var("XDG_RUNTIME_DIR").ok().map(|d| std::path::Path::new(&d).join("pipewire-0").exists()).unwrap_or(false))
}

/// The Linux rule, apart from the machine so it can be tested: WebKitGTK 2.38+ and a PipeWire socket.
#[cfg(any(target_os = "linux", test))]
pub fn platform_linux(major: u32, minor: u32, pipewire: bool) -> (bool, String, bool) {
    if major < 2 || (major == 2 && minor < 38) { return (false, format!("WebKitGTK {major}.{minor} cannot capture the screen (2.38 or newer needed)."), false); }
    if !pipewire { return (false, "No PipeWire session found (screen sharing goes through PipeWire and xdg-desktop-portal).".into(), false); }
    (true, String::new(), false)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn platform() -> (bool, String, bool) { (false, "Screen sharing is not offered by this platform's webview.".into(), false) }

/// Linux: WebKitGTK ships with media streams off; turn them on for the hub's window (not the Tools window).
#[cfg(target_os = "linux")]
pub fn enable_in_webview(w: &tauri::WebviewWindow) {
    let _ = w.with_webview(|pw| {
        use webkit2gtk::{SettingsExt, WebViewExt};
        if let Some(s) = pw.inner().settings() { s.set_enable_media_stream(true); s.set_enable_mediasource(true); s.set_enable_webrtc(true); }
    });
}
#[cfg(not(target_os = "linux"))]
pub fn enable_in_webview(_w: &tauri::WebviewWindow) {}

#[cfg(test)]
mod tests {
    use super::*;
    const HUB: &str = "https://hub.example.org";
    #[test] fn only_the_hubs_own_origin_is_granted() {
        assert!(is_hub_origin("https://hub.example.org/chat?x=1", HUB));
        assert!(is_hub_origin("https://HUB.example.org:443/", HUB));
        assert!(!is_hub_origin("https://hub.example.org.evil.com/", HUB));
        assert!(!is_hub_origin("https://evil.com/hub.example.org", HUB));
        assert!(!is_hub_origin("http://hub.example.org/", HUB));
        assert!(!is_hub_origin("https://hub.example.org:8443/", HUB));
        assert!(!is_hub_origin("tauri://localhost/tools.html", HUB));
        assert!(!is_hub_origin("about:blank", HUB));
        assert!(!is_hub_origin("", HUB));
        assert!(is_hub_origin("http://127.0.0.1:7777/x", "http://127.0.0.1:7777"));
        assert!(!is_hub_origin("http://127.0.0.1:7778/x", "http://127.0.0.1:7777"));
    }
    #[test] fn capture_is_answered_by_origin_and_nothing_else_is_granted() {
        for k in [PermissionKind::DisplayCapture, PermissionKind::Microphone, PermissionKind::Camera] {
            assert!(matches!(decide(&k, "https://hub.example.org/", HUB), PermissionResponse::Allow));
            assert!(matches!(decide(&k, "https://evil.com/", HUB), PermissionResponse::Deny));
            assert!(matches!(decide(&k, "tauri://localhost/tools.html", HUB), PermissionResponse::Deny));
        }
        for k in [PermissionKind::Geolocation, PermissionKind::Notifications, PermissionKind::ClipboardRead, PermissionKind::Other] {
            assert!(matches!(decide(&k, "https://hub.example.org/", HUB), PermissionResponse::Default));
        }
    }
    #[test] fn linux_needs_webkitgtk_238_and_pipewire() {
        assert!(platform_linux(2, 44, true).0);
        assert!(platform_linux(2, 38, true).0);
        assert!(!platform_linux(2, 36, true).0);
        assert!(!platform_linux(2, 44, false).0 && platform_linux(2, 44, false).1.contains("PipeWire"));
    }
}
