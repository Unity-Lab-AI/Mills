//! The desktop app updates itself from its hub: GET /api/native?app=vr-desktop&target=<this> (signed in: the session
//! the hub's page handed over; a hub before 4.0.0 had it public as /api/pub/native) is a release manifest signed with
//! VibeRoller's release key (Ed25519, its public half built into this program); the binary is fetched, checked against
//! the manifest, swapped in (the old one kept as .old) and the app restarts on it. The AI host next to it (vr-host)
//! updates itself the same way through its own connection to the hub.
use serde_json::{json, Value};

pub const TARGET: &str = env!("VR_TARGET");
pub const VERSION: &str = match option_env!("VR_VERSION") { Some(v) => v, None => env!("CARGO_PKG_VERSION") };

/// { available, version, current } or an error.
pub async fn check(hub: &str, session: &str) -> Result<Value, String> {
    let (m, f, _) = manifest(hub, session).await?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mine = vr_core::crypto::sha256_hex(&std::fs::read(&exe).map_err(|e| e.to_string())?);
    Ok(json!({ "available": f.as_ref().map(|f| f.sha256 != mine).unwrap_or(false), "version": m.version, "current": VERSION, "target": TARGET }))
}
fn client(hub: &str, session: &str) -> Result<vr_core::hub::HubClient, String> { (if session.is_empty() { vr_core::hub::HubClient::new(hub) } else { vr_core::hub::HubClient::with_token(hub, session) }).map_err(|e| e.to_string()) }
/// The manifest, this program's file in it, and the route prefix it came from ("/api/native" or the older "/api/pub/native").
async fn manifest(hub: &str, session: &str) -> Result<(vr_core::update::Manifest, Option<vr_core::host::ManifestFile>, &'static str), String> {
    let c = client(hub, session)?;
    let q = format!("?app=vr-desktop&target={TARGET}");
    let (s, base): (vr_core::update::Signed, &str) = match if session.is_empty() { Err(vr_core::Error::msg("not signed in")) } else { c.get(&format!("/api/native{q}")).await } {
        Ok(s) => (s, "/api/native"),
        // not signed in, or a hub before 4.0.0 (it has no /api/native): the old public route, which 4.0.0 hubs refuse
        Err(e) => match c.get(&format!("/api/pub/native{q}")).await { Ok(s) => (s, "/api/pub/native"), Err(_) => return Err(if session.is_empty() { "Sign in on the hub's page first: updates need a VibeRoller account.".into() } else { e.to_string() }) },
    };
    let m = vr_core::update::verify(&s, None).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let name = exe.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let f = m.files.iter().find(|f| f.name == name || f.name.trim_end_matches(".exe") == "vr-desktop").cloned();
    Ok((m, f, base))
}
/// Downloads, checks and swaps in the new version; true when the app should restart on it.
pub async fn apply(hub: &str, session: &str) -> Result<bool, String> {
    let (m, f, base) = manifest(hub, session).await?;
    let Some(f) = f else { return Ok(false) };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if vr_core::crypto::sha256_hex(&std::fs::read(&exe).map_err(|e| e.to_string())?) == f.sha256 { return Ok(false); }
    let (bytes, _) = client(hub, session)?.get_bytes(&format!("{base}/file?app=vr-desktop&target={TARGET}&name={}", vr_core::hub::urlencode(&f.name))).await.map_err(|e| e.to_string())?;
    vr_core::update::swap_in(&exe, &bytes, &f.sha256).map_err(|e| e.to_string())?;
    let _ = m;
    Ok(true)
}
