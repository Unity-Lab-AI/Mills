//! Versions (spec 9.1): numbered `1, 2, 3 ...` by the store, with channels `draft`, `published` and named tags.
//!
//! - [`VersionRef`] / [`parse_ref`]: `name`, `name@3`, `name@stable`, `name@draft` (what `/hyperwork` and a
//!   `sub-workflow` node take)
//! - [`Channels`]: the numbers a workflow has, what `published` points at, its tags; resolves a reference
//! - [`publish`]: validates (no errors), takes the next number, stamps who / when / message
//! - [`restore`]: an old version becomes the new draft
//! - node kind versions: [`kind_compatible`] (the engine runs the version a document was made with, or a newer one of
//!   the same major)
use crate::model::Document;
use crate::validate::Report;
use crate::{Error, Result};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionRef {
    /// No range: the published version.
    Published,
    Number(u32),
    Tag(String),
    /// The working copy (an editor's own; not runnable by others).
    Draft,
}

impl fmt::Display for VersionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self { VersionRef::Published => Ok(()), VersionRef::Number(n) => write!(f, "@{n}"), VersionRef::Tag(t) => write!(f, "@{t}"), VersionRef::Draft => f.write_str("@draft") }
    }
}

/// `"release-notes@3"` -> (`release-notes`, Number(3)); `"release-notes"` -> Published; `@stable` is a tag.
pub fn parse_ref(s: &str) -> Result<(String, VersionRef)> {
    let (name, range) = match s.split_once('@') { Some((n, r)) => (n, Some(r)), None => (s, None) };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_/.".contains(c)) { return Err(Error::status(400, format!("`{s}` is not a workflow name (letters, digits and - _ / .)"))); }
    let r = match range {
        None => VersionRef::Published,
        Some("") => return Err(Error::status(400, format!("`{s}` has an empty version after the @"))),
        Some("draft") => VersionRef::Draft,
        Some("published") => VersionRef::Published,
        Some(r) if r.chars().all(|c| c.is_ascii_digit()) => VersionRef::Number(r.parse().map_err(|_| Error::status(400, format!("`{r}` is too big for a version number")))?),
        Some(r) if r.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) => VersionRef::Tag(r.to_string()),
        Some(r) => return Err(Error::status(400, format!("`{r}` is not a version number or a tag name"))),
    };
    Ok((name.to_string(), r))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved { Draft, Version(u32) }

/// A workflow's version numbers and pointers, as the store keeps them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Channels {
    pub versions: Vec<u32>,
    pub published: Option<u32>,
    pub tags: BTreeMap<String, u32>,
}

impl Channels {
    /// The next number: one more than the highest ever used (numbers are never reused).
    pub fn next_version(&self) -> u32 { self.versions.iter().copied().max().unwrap_or(0) + 1 }

    pub fn resolve(&self, r: &VersionRef) -> Result<Resolved> {
        match r {
            VersionRef::Draft => Ok(Resolved::Draft),
            VersionRef::Published => self.published.map(Resolved::Version).ok_or_else(|| Error::status(404, "This workflow has no published version yet.")),
            VersionRef::Number(n) if self.versions.contains(n) => Ok(Resolved::Version(*n)),
            VersionRef::Number(n) => Err(Error::status(404, format!("This workflow has no version {n} (it has {}).", self.list()))),
            VersionRef::Tag(t) => self.tags.get(t).map(|n| Resolved::Version(*n)).ok_or_else(|| Error::status(404, format!("This workflow has no tag `{t}`."))),
        }
    }

    /// Point a tag at an existing version.
    pub fn tag(&mut self, tag: &str, version: u32) -> Result<()> {
        if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) || ["draft", "published"].contains(&tag) || tag.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::status(400, format!("`{tag}` cannot be a tag name (letters, digits and - _ .; not `draft`, `published` or a bare number)")));
        }
        if !self.versions.contains(&version) { return Err(Error::status(404, format!("There is no version {version} to tag."))); }
        self.tags.insert(tag.to_string(), version);
        Ok(())
    }

    fn list(&self) -> String { if self.versions.is_empty() { "none".into() } else { self.versions.iter().map(u32::to_string).collect::<Vec<_>>().join(", ") } }
}

/// Publish a draft: refused with the errors named when the report has any, or without a message. On success the
/// returned document carries the new `version`, `publishedAt`, `publishedBy`, `message`, and `channels` has it as
/// the published version. The old versions stay.
pub fn publish(draft: &Document, report: &Report, channels: &mut Channels, by: &str, message: &str, now_ms: u64) -> Result<Document> {
    let errs: Vec<&crate::validate::Issue> = report.errors().collect();
    if !errs.is_empty() {
        let first = errs.iter().take(3).map(|i| i.message.as_str()).collect::<Vec<_>>().join("; ");
        return Err(Error::status(422, format!("This workflow has {} error{} and cannot be published: {first}{}", errs.len(), if errs.len() == 1 { "" } else { "s" }, if errs.len() > 3 { "; ..." } else { "" })));
    }
    if message.trim().is_empty() { return Err(Error::status(400, "A publish needs a message saying what changed.")); }
    let v = channels.next_version();
    let mut doc = draft.clone();
    doc.version = Some(v);
    doc.published_at = Some(now_ms);
    doc.updated_at = Some(now_ms);
    doc.published_by = Some(by.to_string());
    doc.message = Some(message.trim().to_string());
    channels.versions.push(v);
    channels.published = Some(v);
    Ok(doc)
}

/// An old version as the new draft: the publish stamp and the number are cleared (the store numbers the next publish).
pub fn restore(old: &Document, now_ms: u64) -> Document {
    let mut d = old.clone();
    d.version = None;
    d.published_at = None;
    d.published_by = None;
    d.message = None;
    d.updated_at = Some(now_ms);
    d
}

/// Can the installed kind version (`"1.4.0"`) run a node made with kind version `pinned` (`"1.2.0"`, or just `1`)?
/// The same major, and not older than what the node was made with.
pub fn kind_compatible(installed: &str, pinned: &str) -> bool {
    match (crate::kinds::parse_semver(installed), crate::kinds::parse_semver(pinned)) { (Some(i), Some(p)) => i.0 == p.0 && i >= p, _ => false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs() {
        assert_eq!(parse_ref("notes").unwrap(), ("notes".into(), VersionRef::Published));
        assert_eq!(parse_ref("notes@3").unwrap().1, VersionRef::Number(3));
        assert_eq!(parse_ref("acme/notes@stable").unwrap(), ("acme/notes".into(), VersionRef::Tag("stable".into())));
        assert_eq!(parse_ref("notes@draft").unwrap().1, VersionRef::Draft);
        for bad in ["", "@3", "a b", "notes@", "notes@a b", "notes@99999999999"] { assert!(parse_ref(bad).is_err(), "{bad}"); }
        assert_eq!(VersionRef::Number(3).to_string(), "@3");
    }

    #[test]
    fn channels_and_publish() {
        let mut ch = Channels::default();
        assert_eq!(ch.next_version(), 1);
        assert_eq!(ch.resolve(&VersionRef::Published).unwrap_err().code(), 404);
        let d = Document::new("wf-1", "x");
        let ok = Report::default();
        let p1 = publish(&d, &ok, &mut ch, "gee", "first", 1000).unwrap();
        let p2 = publish(&p1, &ok, &mut ch, "sponge", " second ", 2000).unwrap();
        assert_eq!((p1.version, p2.version, p2.published_by.as_deref(), p2.message.as_deref(), p2.published_at), (Some(1), Some(2), Some("sponge"), Some("second"), Some(2000)));
        assert_eq!(ch.resolve(&VersionRef::Published).unwrap(), Resolved::Version(2));
        assert_eq!(ch.resolve(&VersionRef::Number(1)).unwrap(), Resolved::Version(1));
        assert!(ch.resolve(&VersionRef::Number(7)).unwrap_err().to_string().contains("1, 2"));
        ch.tag("stable", 1).unwrap();
        assert_eq!(ch.resolve(&VersionRef::Tag("stable".into())).unwrap(), Resolved::Version(1));
        assert!(ch.tag("draft", 1).is_err() && ch.tag("3", 1).is_err() && ch.tag("x", 9).is_err());
        assert!(publish(&d, &ok, &mut ch, "gee", "  ", 1).is_err());
        assert_eq!(ch.next_version(), 3, "a failed publish uses no number");
        let r = restore(&p1, 5000);
        assert_eq!((r.version, r.published_by, r.message, r.updated_at), (None, None, None, Some(5000)));
        let bad = Report { issues: vec![crate::validate::validate(&d, &crate::validate::Options::new(&crate::kinds::DefaultRegistry::new())).issues.remove(0)] };
        let e = publish(&d, &bad, &mut ch, "gee", "m", 1).unwrap_err();
        assert!(e.to_string().contains("cannot be published") && ch.next_version() == 3);
    }

    #[test]
    fn kinds() {
        assert!(kind_compatible("1.4.0", "1.2.0") && kind_compatible("1.2.0", "1") && !kind_compatible("2.0.0", "1.9.0") && !kind_compatible("1.1.0", "1.2.0"));
    }
}
