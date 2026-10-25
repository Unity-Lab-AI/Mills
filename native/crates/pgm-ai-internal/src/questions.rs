//! The "AI asks the user" question protocol (T-245 = T-195 · 1/4; spec in docs/question-protocol.md).
//! This file holds the pure part: reading the `ask_user` tool's arguments into a checked list of questions, and turning
//! what the person picked (or the recommended defaults) into the answers the tool returns. The open questions of a
//! run, their timeout and their events live in work.rs (`Work::ask`, `Work::answer`).
use serde_json::{json, Value};
use pgm_core_internal::json::s;

pub const MAX_QUESTIONS: usize = 30;
pub const MAX_OPTIONS: usize = 300;
/// How long a question waits for its person when nothing says otherwise (T-195: 10 minutes).
pub const DEFAULT_TIMEOUT_SEC: u64 = 600;
pub const MAX_TIMEOUT_SEC: u64 = 86_400;

fn clip(v: &Value, n: usize) -> String { v.as_str().map(|x| x.trim().chars().take(n).collect()).unwrap_or_default() }

/// One question as the page gets it: `{id, text, kind: single | multi | checklist | text, options: [{id, label, hint?}],
/// other, otherLabel?, default: [ids] | "text", required}`. Err = what is wrong, for the AI to fix and ask again.
fn question(raw: &Value, n: usize) -> Result<Value, String> {
    if !raw.is_object() { return Err(format!("question {n}: must be an object {{text, kind, options, ...}}")); }
    let text = { let t = clip(&raw["text"], 2000); if t.is_empty() { clip(&raw["question"], 2000) } else { t } };
    if text.is_empty() { return Err(format!("question {n}: text is empty")); }
    let opts_raw = raw["options"].as_array().cloned().unwrap_or_default();
    let mut kind = clip(&raw["kind"], 20).to_lowercase();
    kind = match kind.as_str() { "" => if opts_raw.is_empty() { "text".into() } else { "single".into() }, "checkbox" | "checkboxes" | "checks" => "checklist".into(), "multiple" | "multi-choice" => "multi".into(), "single-choice" | "choice" | "radio" => "single".into(), k => k.into() };
    if !["single", "multi", "checklist", "text"].contains(&kind.as_str()) { return Err(format!("question {n}: kind must be single, multi, checklist or text")); }
    if opts_raw.len() > MAX_OPTIONS { return Err(format!("question {n}: at most {MAX_OPTIONS} options")); }
    let mut options: Vec<Value> = vec![];
    for (i, o) in opts_raw.iter().enumerate() {
        let (id, label, hint) = match o { Value::String(x) => (x.trim().chars().take(300).collect::<String>(), x.trim().chars().take(300).collect::<String>(), String::new()),
            Value::Object(_) => { let label = clip(&o["label"], 300); let id = { let x = clip(&o["id"], 100); if x.is_empty() { label.clone() } else { x } }; (id, label, clip(&o["hint"], 500)) }
            _ => return Err(format!("question {n}: option {} must be a string or {{id, label}}", i + 1)) };
        if id.is_empty() || label.is_empty() { return Err(format!("question {n}: option {} has no label", i + 1)); }
        if options.iter().any(|x| x["id"] == json!(id)) { return Err(format!("question {n}: two options share the id \"{id}\"")); }
        let mut ov = json!({ "id": id, "label": label }); if !hint.is_empty() { ov["hint"] = json!(hint); }
        options.push(ov);
    }
    if kind != "text" && options.is_empty() { return Err(format!("question {n}: a {kind} question needs options")); }
    let ids: Vec<String> = options.iter().map(|o| s(o, "id")).collect();
    let id = { let x = clip(&raw["id"], 40); if x.is_empty() { format!("q{n}") } else { x } };
    // "my own answer": on by default for single / multi, off for a long checklist (it can be turned on)
    let other = match raw["other"] { Value::Bool(b) => b, Value::String(ref t) if !t.is_empty() => true, _ => kind == "single" || kind == "multi" } && kind != "text";
    let mut q = json!({ "id": id, "text": text, "kind": kind, "options": options, "other": other, "required": raw["required"] == json!(true) });
    if other { if let Some(l) = raw["other"].as_str().filter(|l| !l.trim().is_empty()) { q["otherLabel"] = json!(l.trim().chars().take(100).collect::<String>()); } }
    // the recommended answer: option ids (one for single), or the text of a text question; anything that is not an option is dropped
    let def = &raw["default"];
    if kind == "text" { let d = clip(def, 4000); if !d.is_empty() { q["default"] = json!(d); } }
    else {
        let want: Vec<String> = match def { Value::String(x) => vec![x.clone()], Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(), _ => vec![] };
        let mut got: Vec<String> = vec![]; for w in want { if ids.contains(&w) && !got.contains(&w) { got.push(w); } }
        if kind == "single" { got.truncate(1); }
        if !got.is_empty() { q["default"] = json!(got); }
    }
    Ok(q)
}

/// The tool's arguments as (title, questions). `questions` is a list; a lone question may also be given as the arguments
/// themselves. Many questions are a series: the page shows them one after the other (Next, then Done).
pub fn normalize(args: &Value) -> Result<(String, Vec<Value>), String> {
    let raw: Vec<Value> = if let Some(a) = args["questions"].as_array() { a.clone() } else if !args["text"].is_null() || !args["question"].is_null() { vec![args.clone()] } else { vec![] };
    if raw.is_empty() { return Err("questions is empty: give a list of {text, kind, options}".into()); }
    if raw.len() > MAX_QUESTIONS { return Err(format!("at most {MAX_QUESTIONS} questions in one series")); }
    let mut out: Vec<Value> = vec![];
    for (i, r) in raw.iter().enumerate() {
        let q = question(r, i + 1)?;
        if out.iter().any(|x| x["id"] == q["id"]) { return Err(format!("question {}: the id \"{}\" is used twice", i + 1, s(&q, "id"))); }
        out.push(q);
    }
    Ok((clip(&args["title"], 200), out))
}

/// The seconds a question waits: the tool's `timeoutSec` (0 = until answered), else `fallback`.
pub fn timeout_sec(args: &Value, fallback: u64) -> u64 { match args["timeoutSec"].as_u64().or_else(|| args["timeoutSec"].as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)) { Some(t) => t.min(MAX_TIMEOUT_SEC), None => fallback.min(MAX_TIMEOUT_SEC) } }

fn answer_of(q: &Value, picked: Vec<String>, other: String, text: String, by: &str) -> Value {
    let labels: Vec<String> = picked.iter().map(|p| q["options"].as_array().and_then(|o| o.iter().find(|x| x["id"] == json!(p))).map(|x| s(x, "label")).unwrap_or_default()).collect();
    let mut a = json!({ "id": q["id"], "question": q["text"], "kind": q["kind"], "picked": picked, "labels": labels, "by": by });
    if !other.is_empty() { a["other"] = json!(other); }
    if q["kind"] == json!("text") { a["text"] = json!(text); }
    a
}
fn default_answer(q: &Value) -> Value {
    if q["kind"] == json!("text") { return answer_of(q, vec![], String::new(), s(q, "default"), "default"); }
    let picked: Vec<String> = q["default"].as_array().map(|d| d.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    answer_of(q, picked, String::new(), String::new(), "default")
}
/// The recommended answer of every question (the timeout, or "use the defaults").
pub fn default_answers(qs: &[Value]) -> Value { json!(qs.iter().map(default_answer).collect::<Vec<_>>()) }

/// What the person sent (`[{id, picked: [ids], other?, text?}]`) against the questions: a question they did not answer
/// takes its recommended answer (`by: "default"`); an answer is cut down to what the question allows.
pub fn answers_from(qs: &[Value], given: &Value) -> Value {
    let list = given.as_array().cloned().unwrap_or_default();
    json!(qs.iter().map(|q| {
        let Some(g) = list.iter().find(|g| g["id"] == q["id"]) else { return default_answer(q) };
        let kind = s(q, "kind");
        if kind == "text" { return answer_of(q, vec![], String::new(), clip(&g["text"], 8000), "user"); }
        let ids: Vec<String> = q["options"].as_array().map(|o| o.iter().map(|x| s(x, "id")).collect()).unwrap_or_default();
        let mut picked: Vec<String> = vec![];
        for p in g["picked"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str()) { if ids.iter().any(|i| i == p) && !picked.iter().any(|i| i == p) { picked.push(p.into()); } }
        let other = if q["other"] == json!(true) { clip(&g["other"], 4000) } else { String::new() };
        if kind == "single" { if !other.is_empty() { picked.clear(); } picked.truncate(1); }
        answer_of(q, picked, other, String::new(), "user")
    }).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_a_series_and_checks_it() {
        let (t, qs) = normalize(&json!({ "title": "Plan", "questions": [
            { "text": "Which?", "options": ["a", { "id": "b", "label": "Bee" }], "default": "b" },
            { "text": "Which ones?", "kind": "checkbox", "options": ["x", "y", "z"], "default": ["x", "q", "z"] },
            { "text": "Why?" }] })).unwrap();
        assert_eq!(t, "Plan"); assert_eq!(qs.len(), 3);
        assert_eq!(qs[0]["kind"], json!("single")); assert_eq!(qs[0]["other"], json!(true)); assert_eq!(qs[0]["default"], json!(["b"]));
        assert_eq!(qs[1]["kind"], json!("checklist")); assert_eq!(qs[1]["other"], json!(false)); assert_eq!(qs[1]["default"], json!(["x", "z"]));
        assert_eq!(qs[2]["kind"], json!("text"));
        assert!(normalize(&json!({ "questions": [] })).is_err());
        assert!(normalize(&json!({ "questions": [{ "text": "x", "kind": "multi" }] })).is_err());
        assert!(normalize(&json!({ "questions": [{ "text": "x", "options": ["a", "a"] }] })).is_err());
        assert_eq!(normalize(&json!({ "text": "alone", "options": ["a", "b"] })).unwrap().1.len(), 1);
    }
    #[test]
    fn answers_are_cut_to_what_a_question_allows() {
        let (_, qs) = normalize(&json!({ "questions": [
            { "id": "one", "text": "Which?", "options": ["a", "b"], "default": "a" },
            { "id": "many", "text": "Which ones?", "kind": "multi", "options": ["x", "y"], "default": ["y"] },
            { "id": "free", "text": "Why?", "default": "because" }] })).unwrap();
        let a = answers_from(&qs, &json!([{ "id": "one", "picked": ["b", "a", "zzz"] }, { "id": "many", "picked": ["x", "x", "nope"], "other": "w" }]));
        assert_eq!(a[0]["picked"], json!(["b"])); assert_eq!(a[0]["by"], json!("user"));
        assert_eq!(a[1]["picked"], json!(["x"])); assert_eq!(a[1]["other"], json!("w"));
        assert_eq!(a[2]["text"], json!("because")); assert_eq!(a[2]["by"], json!("default"));
        let own = answers_from(&qs, &json!([{ "id": "one", "picked": ["a"], "other": "my own" }]));
        assert_eq!(own[0]["picked"], json!([])); assert_eq!(own[0]["other"], json!("my own"));
        let d = default_answers(&qs); assert_eq!(d[0]["picked"], json!(["a"])); assert_eq!(d[0]["labels"], json!(["a"])); assert_eq!(d[1]["by"], json!("default"));
    }
    #[test]
    fn the_timeout_defaults_to_ten_minutes_and_zero_means_never() {
        assert_eq!(timeout_sec(&json!({}), DEFAULT_TIMEOUT_SEC), 600); assert_eq!(timeout_sec(&json!({ "timeoutSec": 0 }), 600), 0); assert_eq!(timeout_sec(&json!({ "timeoutSec": 3 }), 600), 3);
        assert_eq!(timeout_sec(&json!({ "timeoutSec": 99999999 }), 600), MAX_TIMEOUT_SEC);
    }
}
