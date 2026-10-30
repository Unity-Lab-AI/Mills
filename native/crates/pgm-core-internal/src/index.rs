//! Word indexes (T-331; team note plans/sessions-always-saved.md § 3): finding something is a lookup, not a scan.
//! A `DocIndex` knows, for a set of documents (saved sessions, notes, board tasks …), which words each holds, and
//! answers "which documents have all of these words" (each query word matches the words it begins: "deplo" finds
//! "deploy"). It is kept on disk (one JSON file, written through the write buffer) and brought up to date by `sync`
//! with what is there now: only documents whose stamp (their time and size) changed are read again; gone ones leave.
//! It is a cache: deleting the file only means it is built again on the next use.
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// The words of a text: lowercase runs of letters and digits, 2 to 48 characters.
pub fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| { let n = w.chars().count(); (2..=48).contains(&n) }).map(|w| w.to_lowercase()).collect()
}

#[derive(Default)]
pub struct DocIndex {
    path: PathBuf,
    /// document id → (stamp, its words)
    docs: BTreeMap<String, (String, Vec<String>)>,
    /// word → the documents that have it
    postings: BTreeMap<String, BTreeSet<String>>,
}
impl DocIndex {
    /// The index kept in `path` (empty when there is none or it cannot be read).
    pub fn open(path: &Path) -> DocIndex {
        let mut ix = DocIndex { path: path.to_path_buf(), ..Default::default() };
        if let Some(v) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            for (id, d) in v["docs"].as_object().into_iter().flatten() {
                let ws: Vec<String> = d["w"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                ix.put(id, d["s"].as_str().unwrap_or(""), ws);
            }
        }
        ix
    }
    fn put(&mut self, id: &str, stamp: &str, ws: Vec<String>) {
        self.drop_doc(id);
        for w in &ws { self.postings.entry(w.clone()).or_default().insert(id.to_string()); }
        self.docs.insert(id.to_string(), (stamp.to_string(), ws));
    }
    fn drop_doc(&mut self, id: &str) {
        if let Some((_, ws)) = self.docs.remove(id) { for w in ws { if let Some(p) = self.postings.get_mut(&w) { p.remove(id); if p.is_empty() { self.postings.remove(&w); } } } }
    }
    /// Brings the index up to date with the documents there now (`(id, stamp)`): a new or changed one is read with `text`
    /// (None = it cannot be read now: it stays out), a gone one leaves. Saved when anything changed. Returns how many
    /// documents were read.
    pub fn sync(&mut self, now: &[(String, String)], text: impl Fn(&str) -> Option<String>) -> usize {
        let have: BTreeSet<&str> = now.iter().map(|(i, _)| i.as_str()).collect();
        let gone: Vec<String> = self.docs.keys().filter(|k| !have.contains(k.as_str())).cloned().collect();
        let mut read = 0;
        for g in &gone { self.drop_doc(g); }
        for (id, stamp) in now {
            if self.docs.get(id).is_some_and(|d| &d.0 == stamp) { continue; }
            read += 1;
            match text(id) { Some(t) => self.put(id, stamp, words(&t).into_iter().collect()), None => self.drop_doc(id) }
        }
        if read > 0 || !gone.is_empty() { self.save(); }
        read
    }
    /// The documents that have every word of the query (each as the start of one of their words), sorted by id.
    pub fn find(&self, q: &str) -> Vec<String> {
        let qs: Vec<String> = words(q).into_iter().collect();
        if qs.is_empty() { return vec![]; }
        let mut out: Option<BTreeSet<String>> = None;
        for w in qs {
            let mut hit: BTreeSet<String> = BTreeSet::new();
            for (_, ids) in self.postings.range(w.clone()..).take_while(|(k, _)| k.starts_with(&w)) { hit.extend(ids.iter().cloned()); }
            out = Some(match out { None => hit, Some(o) => o.intersection(&hit).cloned().collect() });
            if out.as_ref().is_some_and(|o| o.is_empty()) { break; }
        }
        out.unwrap_or_default().into_iter().collect()
    }
    pub fn len(&self) -> usize { self.docs.len() }
    pub fn is_empty(&self) -> bool { self.docs.is_empty() }
    fn save(&self) {
        let docs: Map<String, Value> = self.docs.iter().map(|(k, (s, w))| (k.clone(), json!({ "s": s, "w": w }))).collect();
        crate::store::global().replace(&self.path, serde_json::to_vec(&json!({ "v": 1, "docs": docs })).unwrap_or_default(), true);
    }
}

/// The open indexes of this process, by file (one load per index, then kept in memory).
pub fn shared(path: &Path) -> Arc<Mutex<DocIndex>> {
    static S: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<DocIndex>>>>> = OnceLock::new();
    let mut m = S.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner());
    m.entry(path.to_path_buf()).or_insert_with(|| Arc::new(Mutex::new(DocIndex::open(path)))).clone()
}
/// A file's stamp for `sync`: its modification time (ms) and size.
pub fn stamp_of(p: &Path) -> String {
    let m = std::fs::metadata(p).ok();
    let t = m.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis()).unwrap_or(0);
    format!("{t}:{}", m.map(|m| m.len()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_lookup_finds_every_word_as_a_start_and_follows_changes() {
        let d = std::env::temp_dir().join(format!("pgm-index-{}", crate::crypto::random_hex(4))); std::fs::create_dir_all(&d).unwrap();
        let f = d.join("ix.json");
        let texts: HashMap<&str, &str> = [("a", "How do I deploy the hub? Run restart.js"), ("b", "Le déploiement du hub, étape 2"), ("c", "Nothing about it")].into_iter().collect();
        let mut ix = DocIndex::open(&f);
        let now = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        assert_eq!(ix.sync(&now(&[("a", "1"), ("b", "1"), ("c", "1")]), |id| texts.get(id).map(|t| t.to_string())), 3);
        assert_eq!(ix.find("deplo hub"), vec!["a"]);
        assert_eq!(ix.find("HUB"), vec!["a", "b"]);
        assert_eq!(ix.find("déploiement"), vec!["b"]);
        assert!(ix.find("ploy").is_empty(), "a word is matched from its start");
        assert!(ix.find("").is_empty());
        // only what changed is read again; a gone one leaves
        assert_eq!(ix.sync(&now(&[("a", "1"), ("b", "2")]), |id| if id == "b" { Some("now about cats".into()) } else { None }), 1);
        assert_eq!(ix.find("hub"), vec!["a"]); assert_eq!(ix.find("cats"), vec!["b"]); assert!(ix.find("nothing").is_empty());
        // kept on disk: a new process finds the same
        crate::store::global().flush();
        let again = DocIndex::open(&f);
        assert_eq!(again.len(), 2); assert_eq!(again.find("deploy"), vec!["a"]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
