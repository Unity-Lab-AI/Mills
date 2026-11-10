//! `code_find` (T-331 § 3; team note plans/sessions-always-saved.md): a word index of a worktree's source files, so an
//! AI looks a symbol or a phrase up instead of searching the tree file by file. The files are git's (`git ls-files`:
//! tracked and untracked, ignored ones left out), only source and docs (by extension), each under 1 MB. The index
//! (`pgm_core_internal::index::DocIndex`) lives OUTSIDE the worktree (the caller names its file), so it never shows in
//! git; it is brought up to date before each lookup (only new and changed files are read again).
//! A lookup: the files that have every word (each as the start of one of their words), then in those files the lines
//! that have them all; a line that defines one of the words (fn / struct / class / function / def / const …) comes first.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const MAX_FILE: u64 = 1024 * 1024;
const MAX_FILES: usize = 30_000;
const EXT: &[&str] = &["rs", "js", "mjs", "cjs", "ts", "tsx", "jsx", "py", "go", "java", "kt", "c", "h", "cc", "cpp", "hpp", "cs", "swift", "rb", "php", "lua",
    "html", "css", "scss", "vue", "svelte", "md", "txt", "json", "toml", "yaml", "yml", "sh", "ps1", "cmd", "bat", "sql", "proto", "graphql"];

/// The files of a worktree that are worth indexing (paths relative to it, `/`-separated).
pub fn files(dir: &Path) -> Vec<String> {
    let out = { #[allow(unused_mut)] let mut c = std::process::Command::new("git"); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c }.args(["-C", &dir.display().to_string(), "ls-files", "-z", "--cached", "--others", "--exclude-standard"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output();
    let names: Vec<String> = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).split('\0').filter(|s| !s.is_empty()).map(String::from).collect(),
        _ => { let mut v = vec![]; walk(dir, dir, &mut v); v } // not a git folder: everything but the usual build / dependency folders
    };
    let mut keep: Vec<String> = names.into_iter().filter(|n| {
        let ext = n.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        EXT.contains(&ext.as_str()) && !n.split('/').any(|seg| matches!(seg, "node_modules" | "target" | "dist" | ".git" | "vendor" | "__pycache__"))
    }).collect();
    keep.sort(); keep.dedup(); keep.truncate(MAX_FILES);
    keep
}
fn walk(base: &Path, d: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    for e in rd.flatten() {
        let p = e.path(); let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with('.') || matches!(n.as_str(), "node_modules" | "target" | "dist" | "vendor") { continue; }
        if p.is_dir() { walk(base, &p, out); } else if let Ok(r) = p.strip_prefix(base) { out.push(r.to_string_lossy().replace('\\', "/")); }
        if out.len() > MAX_FILES * 2 { return; }
    }
}
/// Does this line define `word` (a function, type, class, constant … of that name)?
pub fn defines(line: &str, word: &str) -> bool {
    let l = line.trim_start().to_lowercase();
    const KW: &[&str] = &["fn ", "pub fn ", "pub(crate) fn ", "async fn ", "pub async fn ", "struct ", "pub struct ", "enum ", "pub enum ", "trait ", "pub trait ", "type ", "pub type ", "impl ", "mod ", "pub mod ",
        "const ", "pub const ", "static ", "pub static ", "function ", "async function ", "export function ", "export async function ", "export const ", "export default function ", "class ", "export class ", "interface ", "export interface ", "def ", "async def ", "let ", "var "];
    KW.iter().any(|k| l.strip_prefix(k).is_some_and(|rest| { let name: String = rest.trim_start_matches('*').chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect(); name.to_lowercase() == word }))
}

/// Looks `q` up in the worktree `dir`, with its index kept in `index_file`: at most `limit` lines.
pub fn find(dir: &Path, index_file: &Path, q: &str, limit: usize) -> Value {
    let words: Vec<String> = pgm_core_internal::index::words(q).into_iter().collect();
    if words.is_empty() { return json!({ "error": "Say what to look for: a name, or a few words." }); }
    let ident: String = { let t = q.trim().to_lowercase(); if !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') { t } else { String::new() } };
    let fs = files(dir);
    let abs = |p: &str| -> PathBuf { dir.join(p) };
    let now: Vec<(String, String)> = fs.iter().filter(|p| std::fs::metadata(abs(p)).map(|m| m.len() <= MAX_FILE).unwrap_or(false)).map(|p| (p.clone(), pgm_core_internal::index::stamp_of(&abs(p)))).collect();
    let ix = pgm_core_internal::index::shared(index_file);
    let (cands, read) = { let mut g = ix.lock().unwrap_or_else(|e| e.into_inner()); let read = g.sync(&now, |p| std::fs::read_to_string(abs(p)).ok().map(|t| format!("{p}\n{t}"))); (g.find(q), read) };
    pgm_core_internal::obs::count("vr_index_lookups", &[("index", "code")]);
    let mut hits: Vec<(bool, String, usize, String)> = vec![];
    for p in &cands {
        let Ok(t) = std::fs::read_to_string(abs(p)) else { continue };
        let path_words = pgm_core_internal::index::words(p);
        for (i, line) in t.lines().enumerate() {
            let lw = pgm_core_internal::index::words(line);
            // every word on the line, or (for a word only the path has) the rest on the line
            if !words.iter().all(|w| lw.iter().any(|x| x.starts_with(w.as_str())) || path_words.iter().any(|x| x.starts_with(w.as_str()))) { continue; }
            if !words.iter().any(|w| lw.iter().any(|x| x.starts_with(w.as_str()))) { continue; }
            // a query that is one name (journal_file, journalFile) is matched whole for a definition, as are its words
            let def = (!ident.is_empty() && defines(line, &ident)) || words.iter().any(|w| defines(line, w));
            hits.push((def, p.clone(), i + 1, line.trim().chars().take(240).collect()));
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let total = hits.len();
    json!({ "files": cands.len(), "indexed": now.len(), "readNow": read, "total": total,
        "hits": hits.into_iter().take(limit).map(|(def, p, l, t)| json!({ "path": p, "line": l, "def": def, "text": t })).collect::<Vec<_>>() })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_definition_comes_first_and_only_matching_files_are_opened() {
        let d = std::env::temp_dir().join(format!("pgm-codefind-{}", pgm_core_internal::crypto::random_hex(4)));
        std::fs::create_dir_all(d.join("src")).unwrap(); std::fs::create_dir_all(d.join("node_modules/x")).unwrap();
        std::fs::write(d.join("src/a.rs"), "use x;\npub fn journal_file(a: &Account) -> PathBuf { todo!() }\n").unwrap();
        std::fs::write(d.join("src/b.js"), "// calls journal_file twice\nconst j = journalFile();\nfunction other() {}\n").unwrap();
        std::fs::write(d.join("node_modules/x/c.js"), "journal_file").unwrap();
        std::fs::write(d.join("logo.png"), "journal_file").unwrap();
        let ix = d.join("..").join(format!("{}.ix.json", d.file_name().unwrap().to_string_lossy()));
        let r = find(&d, &ix, "journal_file", 20);
        let hits = r["hits"].as_array().unwrap(); assert!(!hits.is_empty(), "{r}");
        assert_eq!(hits[0]["path"], json!("src/a.rs")); assert_eq!(hits[0]["def"], json!(true)); assert_eq!(hits[0]["line"], json!(2));
        assert!(hits.iter().any(|h| h["path"] == json!("src/b.js")));
        assert!(!hits.iter().any(|h| h["path"].as_str().unwrap().contains("node_modules") || h["path"].as_str().unwrap().ends_with(".png")));
        assert_eq!(r["indexed"], json!(2));
        // a second lookup reads nothing again (nothing changed); a changed file is read again
        assert_eq!(find(&d, &ix, "other", 5)["readNow"], json!(0));
        std::fs::write(d.join("src/b.js"), "function brandNewThing() {}\n").unwrap();
        let r2 = find(&d, &ix, "brandnewthing", 5);
        assert_eq!(r2["readNow"], json!(1)); assert_eq!(r2["hits"][0]["def"], json!(true));
        assert!(find(&d, &ix, "  ", 5)["error"].is_string());
        let _ = std::fs::remove_dir_all(&d); let _ = std::fs::remove_file(&ix);
    }
}
