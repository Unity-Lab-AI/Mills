//! A change record: one write of one record of one kind, as it travels between copies and sits in a journal.
//! `{ kind, id, rev, time, origin, originName, seq, value }` or, for a delete, `deleted: true` instead of the value.
//! Fields a newer build adds are kept as they came (held records keep the whole JSON).
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    /// what it is ("board.task", "note", "chat.message" …)
    pub kind: String,
    /// which one of that kind (a task id, a note's path …)
    pub id: String,
    /// the kind's own revision (0 when it has none); with `time` it orders two versions: higher wins
    pub rev: i64,
    /// when the change was made (ms)
    pub time: i64,
    /// the copy (hub) that made it, and its name ("live", "dev")
    pub origin: String,
    pub origin_name: String,
    /// its place in the origin's journal (1, 2, 3 …)
    pub seq: u64,
    /// the record's new value; None = deleted (a tombstone)
    pub value: Option<Value>,
}

fn kind_ok(k: &str) -> bool { !k.is_empty() && k.len() <= 64 && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'_') }
fn text_ok(s: &str, max: usize) -> bool { !s.is_empty() && s.len() <= max && !s.chars().any(|c| c.is_control()) }

impl Record {
    /// A record as JSON (what goes into a journal and onto the wire).
    pub fn to_value(&self) -> Value {
        let mut v = json!({ "kind": self.kind, "id": self.id, "rev": self.rev, "time": self.time, "origin": self.origin, "originName": self.origin_name, "seq": self.seq });
        match &self.value { Some(x) => { v["value"] = x.clone(); } None => { v["deleted"] = json!(true); } }
        v
    }
    /// The generic shape check every record passes before any kind looks at it: a kind name (lowercase letters, digits,
    /// `.`, `-`, `_`), an id and an origin (text without control characters), a seq above 0, and a value or
    /// `deleted: true` (not both, not neither).
    pub fn from_value(v: &Value) -> Result<Record, String> {
        if !v.is_object() { return Err("a record is a JSON object".into()); }
        let kind = v["kind"].as_str().filter(|k| kind_ok(k)).ok_or("bad or missing kind")?;
        let id = v["id"].as_str().filter(|s| text_ok(s, 1024)).ok_or("bad or missing id")?;
        let origin = v["origin"].as_str().filter(|s| text_ok(s, 64)).ok_or("bad or missing origin")?;
        let origin_name = v["originName"].as_str().unwrap_or("");
        if origin_name.len() > 40 || origin_name.chars().any(|c| c.is_control()) { return Err("bad originName".into()); }
        let seq = v["seq"].as_u64().filter(|s| *s > 0).ok_or("bad or missing seq")?;
        let num = |k: &str| -> Result<i64, String> { match &v[k] { Value::Null => Ok(0), x => x.as_i64().ok_or_else(|| format!("{k} is not a whole number")) } };
        let (rev, time) = (num("rev")?, num("time")?);
        let deleted = v["deleted"] == json!(true);
        let has_value = v.get("value").map(|x| !x.is_null()).unwrap_or(false);
        let value = match (has_value, deleted) {
            (true, false) => Some(v["value"].clone()),
            (false, true) => None,
            (true, true) => return Err("a record has a value or is deleted, not both".into()),
            (false, false) => return Err("a record needs a value (or deleted: true)".into()),
        };
        Ok(Record { kind: kind.into(), id: id.into(), rev, time, origin: origin.into(), origin_name: origin_name.into(), seq, value })
    }
    /// (rev, time): which of two versions is newer (higher wins; the engine breaks a tie by the value's hash).
    pub fn order(&self) -> (i64, i64) { (self.rev, self.time) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_record_round_trips_and_bad_shapes_are_refused() {
        let r = Record { kind: "board.task".into(), id: "T-1".into(), rev: 2, time: 5, origin: "h1".into(), origin_name: "live".into(), seq: 9, value: Some(json!({ "title": "x" })) };
        assert_eq!(Record::from_value(&r.to_value()).unwrap(), r);
        let t = Record { value: None, ..r.clone() };
        assert_eq!(Record::from_value(&t.to_value()).unwrap(), t, "a tombstone too");
        for (bad, why) in [
            (json!("x"), "not an object"), (json!({ "kind": "Board", "id": "a", "origin": "h", "seq": 1, "value": 1 }), "upper case kind"),
            (json!({ "kind": "k", "id": "", "origin": "h", "seq": 1, "value": 1 }), "empty id"), (json!({ "kind": "k", "id": "a\n", "origin": "h", "seq": 1, "value": 1 }), "control character"),
            (json!({ "kind": "k", "id": "a", "origin": "h", "seq": 0, "value": 1 }), "seq 0"), (json!({ "kind": "k", "id": "a", "origin": "h", "seq": 1 }), "no value"),
            (json!({ "kind": "k", "id": "a", "origin": "h", "seq": 1, "value": 1, "deleted": true }), "both"), (json!({ "kind": "k", "id": "a", "origin": "h", "seq": 1, "value": 1, "rev": "x" }), "rev text"),
        ] { assert!(Record::from_value(&bad).is_err(), "{why}"); }
    }
}
