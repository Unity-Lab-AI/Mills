//! A folder of workflow files, one `<name>.json` each.
//!
//! [`load_dir`] lists them and goes on whatever it finds (FR2 style, like the hub's own data files): a file that is
//! not valid JSON, or not a workflow document, is moved aside as `<file>.bad-<yyyymmdd-hhmmss>` (kept, never deleted,
//! never overwritten by the next save), a notice says so ([`pgm_core_internal::notices`]), and the listing continues.
//! A file of a **newer schema** is not bad data (a newer program wrote it): it stays where it is, is reported in
//! [`Loaded::newer`] and is skipped.
use crate::model::Document;
use crate::{doc, Error, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Aside {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Why it was bad (the parse error's message).
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Loaded {
    /// Every good workflow file, in file name order.
    pub docs: Vec<(PathBuf, Document)>,
    /// The bad files that were moved aside.
    pub moved_aside: Vec<Aside>,
    /// Files of a newer schema than this program reads (left alone), with the message.
    pub newer: Vec<(PathBuf, String)>,
}

/// List a folder of workflow files. A folder that is not there is an empty list.
pub fn load_dir(dir: &Path) -> Loaded {
    let mut out = Loaded::default();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json") && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))).collect();
    files.sort();
    for p in files {
        let Ok(text) = std::fs::read_to_string(&p) else {
            // not readable as text (binary garbage, or no permission): bad data as far as this folder is concerned
            move_aside(&p, "the file is not text".to_string(), &mut out);
            continue;
        };
        if text.trim().is_empty() { continue; }
        match doc::parse(&text) {
            Ok(d) => out.docs.push((p, d)),
            Err(e @ Error::Status { status: 422, .. }) => out.newer.push((p, e.to_string())),
            Err(e) => move_aside(&p, e.to_string(), &mut out),
        }
    }
    out
}

fn move_aside(p: &Path, reason: String, out: &mut Loaded) {
    let stamp: String = pgm_core_internal::time::iso_now().replace(['-', ':'], "").replace('T', "-").trim_end_matches('Z').chars().take(15).collect();
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut to = p.with_file_name(format!("{name}.bad-{stamp}"));
    let mut n = 2;
    while to.exists() { to = p.with_file_name(format!("{name}.bad-{stamp}-{n}")); n += 1; }
    match std::fs::rename(p, &to) {
        Ok(()) => {
            pgm_core_internal::notices::push(format!("**A workflow file could not be read.** `{name}` is not a usable workflow ({reason}); it was moved to `{}` (kept) and the other workflows were loaded. If it held something important, fix it and put it back.", to.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
            out.moved_aside.push(Aside { from: p.to_path_buf(), to, reason });
        }
        Err(e) => pgm_core_internal::notices::push(format!("**A workflow file could not be read.** `{name}` is not a usable workflow ({reason}) and could not be moved aside ({e}); it was skipped.")),
    }
}

/// Read one workflow file (a newer schema is refused, an older one migrated; the file is not changed).
pub fn load_file(p: &Path) -> Result<Document> { doc::parse(&std::fs::read_to_string(p)?) }

/// Write a workflow file in its canonical form, atomically (a temp file, then a rename), creating the folder.
pub fn save(p: &Path, d: &Document) -> Result<()> {
    if let Some(parent) = p.parent() { std::fs::create_dir_all(parent)?; }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, doc::write(d) + "\n")?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}
