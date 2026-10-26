//! Reading and writing a workflow document.
//!
//! - [`parse`] / [`from_value`]: a document of an older `schema` is migrated on read (the file is never rewritten by
//!   reading); a **newer** schema is refused with a clear message and nothing is guessed (compatibility law, spec 9.1).
//! - [`write`] / [`to_value`]: pretty JSON in the canonical field order (unknown fields last, sub-graph keys sorted), so
//!   `write(parse(write(x)))` is `write(x)` and a canonical file round-trips byte for byte.
use crate::model::Document;
use crate::{Error, Result, MAX_BYTES, SCHEMA};
use serde_json::{Map, Value};

/// A document read from text: older schemas are migrated to [`SCHEMA`].
pub fn parse(text: &str) -> Result<Document> {
    if text.len() > MAX_BYTES { return Err(Error::status(413, format!("This workflow is {} bytes; a document may be at most {MAX_BYTES}.", text.len()))); }
    let v: Value = serde_json::from_str(text).map_err(|e| Error::status(400, format!("This is not valid JSON: {e}")))?;
    from_value(v)
}

/// The same from a JSON value.
pub fn from_value(v: Value) -> Result<Document> {
    let from = schema_of(&v)?;
    if from > SCHEMA {
        return Err(Error::status(422, format!(
            "This workflow was saved with document schema {from}, which is newer than this program understands (schema {SCHEMA}). It was not opened and not changed: update VibeRoller to open it.")));
    }
    let v = if from < SCHEMA { migrate(v, from)? } else { v };
    let mut doc: Document = serde_json::from_value(v).map_err(|e| Error::status(400, format!("This is not a workflow document: {e}")))?;
    doc.schema = SCHEMA;
    Ok(doc)
}

/// The `schema` a document says it has (a document without one is schema 0, the pre-release sketch).
pub fn schema_of(v: &Value) -> Result<u32> {
    let obj = v.as_object().ok_or_else(|| Error::status(400, "This is not a workflow document: the top level must be a JSON object."))?;
    match obj.get("schema") {
        None => Ok(0),
        Some(s) => s.as_u64().filter(|n| *n <= u32::MAX as u64).map(|n| n as u32)
            .ok_or_else(|| Error::status(400, format!("This is not a workflow document: `schema` must be a whole number, not {s}."))),
    }
}

/// Migrate a document value from schema `from` up to [`SCHEMA`], one step at a time. Each step is a plain function of
/// the JSON; unknown fields pass through untouched.
pub fn migrate(mut v: Value, from: u32) -> Result<Value> {
    let mut at = from;
    while at < SCHEMA {
        v = match at {
            0 => step_0_to_1(v),
            n => return Err(Error::status(500, format!("no migration from schema {n}"))),
        };
        at += 1;
    }
    Ok(v)
}

/// Schema 0 (the early draft sketch): wires were `connections`, a node's kind was `type`, and there was no `schema`.
fn step_0_to_1(mut v: Value) -> Value {
    fn fix_graph(g: &mut Map<String, Value>) {
        if !g.contains_key("edges") { if let Some(c) = g.remove("connections") { g.insert("edges".into(), c); } }
        if let Some(Value::Array(nodes)) = g.get_mut("nodes") {
            for n in nodes { if let Some(o) = n.as_object_mut() { if !o.contains_key("kind") { if let Some(t) = o.remove("type") { o.insert("kind".into(), t); } } } }
        }
    }
    if let Some(o) = v.as_object_mut() {
        fix_graph(o);
        if let Some(Value::Object(subs)) = o.get_mut("subgraphs") { for (_, s) in subs.iter_mut() { if let Some(so) = s.as_object_mut() { fix_graph(so); } } }
        o.insert("schema".into(), Value::from(1));
    }
    v
}

/// The document as a JSON value.
pub fn to_value(doc: &Document) -> Value { serde_json::to_value(doc).unwrap_or(Value::Null) }

/// The canonical text of a document: pretty JSON, no trailing newline.
pub fn write(doc: &Document) -> String { serde_json::to_string_pretty(doc).unwrap_or_default() }

/// The canonical text of some document text (parse, then write): what a file should contain.
pub fn canonical(text: &str) -> Result<String> { Ok(write(&parse(text)?)) }
