//! Canonical JSON and the hash of a value: the same value gives the same hash on every copy, whatever order its keys
//! were written in (objects sorted by key, no spaces).
use serde_json::Value;

/// `v` as canonical JSON: object keys sorted, compact.
pub fn canonical(v: &Value) -> String { let mut s = String::new(); write(v, &mut s); s }
fn write(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 { out.push(','); }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write(&m[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() { if i > 0 { out.push(','); } write(x, out); }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}
/// The hash of a value (32 hex characters of the SHA-256 of its canonical JSON).
pub fn hash(v: &Value) -> String { pgm_core_internal::crypto::sha256_hex(canonical(v).as_bytes())[..32].to_string() }

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn key_order_does_not_change_the_hash() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"y":[1,2,{"q":1,"p":2}],"x":"z"}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"x":"z","y":[1,2,{"p":2,"q":1}]},"b":1}"#).unwrap();
        assert_eq!(hash(&a), hash(&b));
        assert_eq!(canonical(&json!({ "b": "é\"", "a": null })), r#"{"a":null,"b":"é\""}"#);
        assert_ne!(hash(&json!([1, 2])), hash(&json!([2, 1])), "array order counts");
    }
}
