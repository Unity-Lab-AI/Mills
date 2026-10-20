//! The desktop app updates itself from its hub: GET /api/pub/native?app=vr-desktop&target=<this> is a release manifest
//! signed with VibeRoller's release key (Ed25519, its public half built into this program); the binary is fetched,
//! checked against the manifest, swapped in (the old one kept as .old) and the app restarts on it. The AI host next to
//! it (vr-host) updates itself the same way through its own connection to the hub.
use serde_json::{json, Value};

pub const TARGET: &str = env!("VR_TARGET");
pub const VERSION: &str = match option_env!("VR_VERSION") { Some(v) => v, None => env!("CARGO_PKG_VERSION") };

/// { available, version, current } or an error.
pub async fn check(hub: &str) -> Result<Value, String> {
    let (m, f) = manifest(hub).await?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mine = vr_core::crypto::sha256_hex(&std::fs::read(&exe).map_err(|e| e.to_string())?);
    Ok(json!({ "available": f.as_ref().map(|f| f.sha256 != mine).unwrap_or(false), "version": m.version, "current": VERSION, "target": TARGET }))
}
async fn manifest(hub: &str) -> Result<(vr_core::update::Manifest, Option<vr_core::host::ManifestFile>), String> {
    let c = vr_core::hub::HubClient::new(hub).map_err(|e| e.to_string())?;
    let s: vr_core::update::Signed = c.get(&format!("/api/pub/native?app=vr-desktop&target={TARGET}")).await.map_err(|e| e.to_string())?;
    let m = vr_core::update::verify(&s, None).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let name = exe.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let f = m.files.iter().find(|f| f.name == name || f.name.trim_end_matches(".exe") == "vr-desktop").cloned();
    Ok((m, f))
}
/// Downloads, checks and swaps in the new version; true when the app should restart on it.
pub async fn apply(hub: &str) -> Result<bool, String> {
    let (m, f) = manifest(hub).await?;
    let Some(f) = f else { return Ok(false) };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if vr_core::crypto::sha256_hex(&std::fs::read(&exe).map_err(|e| e.to_string())?) == f.sha256 { return Ok(false); }
    let c = vr_core::hub::HubClient::new(hub).map_err(|e| e.to_string())?;
    let (bytes, _) = c.get_bytes(&format!("/api/pub/native/file?app=vr-desktop&target={TARGET}&name={}", vr_core::hub::urlencode(&f.name))).await.map_err(|e| e.to_string())?;
    vr_core::update::swap_in(&exe, &bytes, &f.sha256).map_err(|e| e.to_string())?;
    let _ = m;
    Ok(true)
}
