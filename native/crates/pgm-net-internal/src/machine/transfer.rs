//! Big-file transfer, the machine's side (plans/linked-machines.md 6.3, T-311).
//!
//! The hub never carries a big file inside a tool call and never buffers one: it issues a one-shot TICKET (bound to the
//! account, the machine, the path as the AI typed it and the direction; valid 5 minutes) and the machine moves the bytes
//! over its own connection to `/api/machine/transfer/<ticket>/...` in chunks of [`CHUNK`] bytes that the hub writes straight
//! to disk (or reads straight from disk).
//! - `transfer_get {path}`: the machine reads the file (scope: `read`, `maxFileMB`) and pushes it up; the hub keeps it in the
//!   account's transfer area and the answer names it (`id`, `size`, `sha256`). Resume: a part file on the hub is
//!   found by a key the machine derives from (machine, path, size, mtime), so the next call carries on at the byte the hub has.
//! - `transfer_put {path, from}`: the file the owner uploaded to the hub's transfer area (`from` = its id) is pulled down into
//!   `<path>.vr-part` (scope: `write`), resumed by that part file's size, verified by sha256, then renamed onto `path`.
//! - Both check the scope on the machine (the hub's check is the first line). `maxBytes` bounds one call; `budgetMs` its time
//!   (default 4 minutes, inside the ticket's 5): a call that stops early answers `complete: false` and the same call again resumes.
use super::exec::{authz, open_real, Fail, Machine};
use super::paths::OpenMode;
use super::scope::{Op, Scope};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub const CHUNK: usize = 4 << 20;
const DEFAULT_BUDGET_MS: u64 = 240_000;

fn hexsha(h: Sha256) -> String { hex::encode(h.finalize()) }

/// sha256 of a whole file, read in 1 MB steps on a blocking thread.
pub async fn sha256_file(f: std::fs::File) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        use std::io::{Seek, SeekFrom};
        let mut f = f;
        f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let (mut h, mut buf) = (Sha256::new(), vec![0u8; 1 << 20]);
        loop { let n = f.read(&mut buf).map_err(|e| e.to_string())?; if n == 0 { break; } h.update(&buf[..n]); }
        Ok(hexsha(h))
    }).await.map_err(|_| "the hashing thread failed".to_string())?
}

async fn post_json(l: &super::exec::Link, path: &str, body: &Value) -> Result<(u16, Value), String> {
    let r = l.http.post(format!("{}{path}", l.base)).bearer_auth(&l.token).json(body).timeout(Duration::from_secs(120)).send().await.map_err(|e| e.to_string())?;
    let st = r.status().as_u16();
    Ok((st, r.json::<Value>().await.unwrap_or(Value::Null)))
}

fn hub_err(st: u16, v: &Value) -> Fail {
    let m = v["error"].as_str().unwrap_or("the hub refused the transfer").to_string();
    if matches!(st, 401 | 403 | 410) { Fail::Denied(format!("the hub refused the ticket: {m}")) } else { Fail::Error(format!("the hub answered {st}: {m}")) }
}

pub(crate) async fn run(m: &Machine, sc: &Scope, tool: &str, a: &Value, now: i64, max_file: u64) -> super::exec::Done {
    let Some(link) = m.link() else { return Err(Fail::Error("This machine is not connected to the hub right now, so it cannot move a file.".into())) };
    let ticket = a["ticket"].as_str().unwrap_or("");
    if ticket.is_empty() { return Err(Fail::Error("A transfer needs a ticket from the hub (the hub issues it when the AI calls transfer_get / transfer_put).".into())); }
    let Some(base) = sc.selected_folder(now).map(std::path::PathBuf::from) else { return Err(Fail::Denied("this scope has no selected folder".into())) };
    let given = a["path"].as_str().unwrap_or("").to_string();
    if given.trim().is_empty() { return Err(Fail::Error("path is the file to move.".into())); }
    let cap = a["cap"].as_u64().unwrap_or(u64::MAX).min(max_file);
    let max_send = a["maxBytes"].as_u64().filter(|n| *n > 0).unwrap_or(u64::MAX);
    let budget = Duration::from_millis(a["budgetMs"].as_u64().filter(|n| *n > 0).unwrap_or(DEFAULT_BUDGET_MS).min(DEFAULT_BUDGET_MS));
    if tool == "transfer_get" { get(m, sc, &link, ticket, &base, &given, cap, max_send, budget, now).await } else { put(m, sc, &link, ticket, &base, &given, a, cap, max_send, budget, now).await }
}

#[allow(clippy::too_many_arguments)]
async fn get(m: &Machine, sc: &Scope, l: &super::exec::Link, ticket: &str, base: &std::path::Path, given: &str, cap: u64, max_send: u64, budget: Duration, now: i64) -> super::exec::Done {
    let r = authz(sc, Op::Read, base, given, now)?;
    let f = open_real(sc, &r, Op::Read, OpenMode::Read, now)?;
    let meta = f.metadata().map_err(|e| Fail::Error(e.to_string()))?;
    if !meta.is_file() { return Err(Fail::Error("That is not a file.".into())); }
    let (size, mtime) = (meta.len(), meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0));
    if size > cap { return Err(Fail::Denied(format!("that file is {} MB, more than the allowed {} MB", size >> 20, cap >> 20))); }
    let key = pgm_core_internal::crypto::sha256_hex(format!("{}|get|{}|{size}|{mtime}", m.id, r.real.display()).as_bytes());
    let t0 = Instant::now();
    let (st, v) = post_json(l, &format!("/api/machine/transfer/{ticket}/status"), &json!({ "path": given, "key": key, "size": size })).await.map_err(Fail::Error)?;
    if st != 200 { return Err(hub_err(st, &v)); }
    let mut offset = v["received"].as_u64().unwrap_or(0);
    let mut restart = false;
    if offset > size { offset = 0; restart = true; }
    let resumed = offset;
    let mut tf = tokio::fs::File::from_std(f.try_clone().map_err(|e| Fail::Error(e.to_string()))?);
    tf.seek(std::io::SeekFrom::Start(offset)).await.map_err(|e| Fail::Error(e.to_string()))?;
    let (mut sent, mut buf) = (0u64, vec![0u8; CHUNK]);
    while offset < size && sent < max_send && t0.elapsed() < budget {
        let want = (CHUNK as u64).min(size - offset).min(max_send - sent) as usize;
        let mut got = 0;
        while got < want { let n = tf.read(&mut buf[got..want]).await.map_err(|e| Fail::Error(e.to_string()))?; if n == 0 { break; } got += n; }
        if got == 0 { return Err(Fail::Error("The file got shorter while it was being sent.".into())); }
        let url = format!("{}/api/machine/transfer/{ticket}/chunk?offset={offset}&key={key}&path={}{}", l.base, pgm_core_internal::text::urlencode(given), if restart { "&restart=1" } else { "" });
        let mut tries = 0;
        loop {
            let res = l.http.post(&url).bearer_auth(&l.token).header("content-type", "application/octet-stream").body(buf[..got].to_vec()).timeout(Duration::from_secs(120)).send().await;
            match res {
                Ok(r) if r.status().is_success() => { restart = false; break; }
                Ok(r) => { let st = r.status().as_u16(); let v: Value = r.json().await.unwrap_or(Value::Null); if st < 500 { return Err(hub_err(st, &v)); } tries += 1; if tries >= 3 { return Err(hub_err(st, &v)); } }
                Err(e) => { tries += 1; if tries >= 3 { return Err(Fail::Error(format!("the hub is not reachable: {e}"))); } }
            }
            tokio::time::sleep(Duration::from_millis(250 << tries)).await;
        }
        offset += got as u64; sent += got as u64;
    }
    if offset < size {
        return Ok((json!({ "complete": false, "received": offset, "resumedFrom": resumed, "size": size, "ticket": ticket, "id": key, "hint": "Not finished in this call. Call transfer_get again with the same path: it carries on from the byte the hub has." }), sent));
    }
    // the whole file's checksum, and the same file as when it began
    let sha = sha256_file(f.try_clone().map_err(|e| Fail::Error(e.to_string()))?).await.map_err(Fail::Error)?;
    let again = f.metadata().map_err(|e| Fail::Error(e.to_string()))?;
    if again.len() != size { return Err(Fail::Error("The file changed while it was being sent; call transfer_get again.".into())); }
    let (st, v) = post_json(l, &format!("/api/machine/transfer/{ticket}/done"), &json!({ "path": given, "key": key, "size": size, "sha256": sha })).await.map_err(Fail::Error)?;
    if st != 200 { return Err(hub_err(st, &v)); }
    Ok((json!({ "complete": true, "ticket": ticket, "id": key, "resumedFrom": resumed, "size": size, "sha256": sha, "name": v["name"], "download": format!("/api/transfers/{key}"), "hubPath": v["hubPath"], "hint": "The file is in the hub's transfer area (kept an hour); the owner downloads it from `download`." }), sent))
}

#[allow(clippy::too_many_arguments)]
async fn put(m: &Machine, sc: &Scope, l: &super::exec::Link, ticket: &str, base: &std::path::Path, given: &str, a: &Value, cap: u64, max_send: u64, budget: Duration, now: i64) -> super::exec::Done {
    let _ = m;
    let (size, want_sha) = (a["size"].as_u64().unwrap_or(0), a["sha256"].as_str().unwrap_or("").to_string());
    if want_sha.len() != 64 { return Err(Fail::Error("The hub did not say which file this is (no sha256).".into())); }
    if size > cap { return Err(Fail::Denied(format!("that file is {} MB, more than the allowed {} MB", size >> 20, cap >> 20))); }
    if given.ends_with('/') || given.ends_with('\\') { return Err(Fail::Error("path is a file name, not a folder.".into())); }
    let target = authz(sc, Op::Write, base, given, now)?;
    if let Some(p) = target.real.to_path_buf().parent() { std::fs::create_dir_all(p).map_err(|e| Fail::Error(e.to_string()))?; }
    let part_given = format!("{given}.vr-part");
    let part = authz(sc, Op::Write, base, &part_given, now)?;
    let have = std::fs::metadata(part.real.to_path_buf()).map(|m| m.len()).unwrap_or(0);
    if have > size { let _ = std::fs::remove_file(part.real.to_path_buf()); }
    let mut offset = if have > size { 0 } else { have };
    let resumed = offset;
    let mut f = open_real(sc, &part, Op::Write, OpenMode::Append, now)?;
    let (mut got_total, t0) = (0u64, Instant::now());
    while offset < size && got_total < max_send && t0.elapsed() < budget {
        let len = (CHUNK as u64).min(size - offset).min(max_send - got_total);
        let mut tries = 0;
        let bytes = loop {
            let res = l.http.post(format!("{}/api/machine/transfer/{ticket}/data", l.base)).bearer_auth(&l.token).json(&json!({ "path": given, "offset": offset, "len": len })).timeout(Duration::from_secs(120)).send().await;
            match res {
                Ok(r) if r.status().is_success() => match r.bytes().await { Ok(b) => break b, Err(e) => { tries += 1; if tries >= 3 { return Err(Fail::Error(e.to_string())); } } },
                Ok(r) => { let st = r.status().as_u16(); let v: Value = r.json().await.unwrap_or(Value::Null); if st < 500 { return Err(hub_err(st, &v)); } tries += 1; if tries >= 3 { return Err(hub_err(st, &v)); } }
                Err(e) => { tries += 1; if tries >= 3 { return Err(Fail::Error(format!("the hub is not reachable: {e}"))); } }
            }
            tokio::time::sleep(Duration::from_millis(250 << tries)).await;
        };
        if bytes.is_empty() { return Err(Fail::Error("The hub sent nothing for that part of the file.".into())); }
        { use std::io::Write; f.write_all(&bytes).map_err(|e| Fail::Error(e.to_string()))?; }
        offset += bytes.len() as u64; got_total += bytes.len() as u64;
    }
    drop(f);
    if offset < size {
        return Ok((json!({ "complete": false, "received": offset, "resumedFrom": resumed, "size": size, "ticket": ticket, "hint": "Not finished in this call. Call transfer_put again with the same path and from: it carries on from the part already here." }), got_total));
    }
    let pf = open_real(sc, &part, Op::Read, OpenMode::Read, now)?;
    let sha = sha256_file(pf).await.map_err(Fail::Error)?;
    if sha != want_sha { let _ = std::fs::remove_file(part.real.to_path_buf()); return Err(Fail::Error("The file did not arrive intact (sha256 differs); the part was removed, call transfer_put again.".into())); }
    let t2 = authz(sc, Op::Write, base, given, now)?;
    std::fs::rename(part.real.to_path_buf(), t2.real.to_path_buf()).map_err(|e| Fail::Error(e.to_string()))?;
    Ok((json!({ "complete": true, "ticket": ticket, "path": given, "resumedFrom": resumed, "size": size, "sha256": sha }), got_total))
}
