//! Laws, saved prompts and groups (spec 6.1). Only the shapes and their checks live here; where they are stored
//! and how they are applied to a node's AI call belong to the store and the engine.
//!
//! ```text
//! Law    { id: "law-..", name, text, check?: { kind: "regex"|"expr"|"jev", .., onFail: "retry"|"error"|"warn" }, tags, group?, scope, owner, version, updatedAt }
//! Prompt { id: "pr-..",  name, text, slots: [{name, type, default?}], tags, group?, scope, owner, version, updatedAt }
//! Group  { id: "grp-..", name, kind: "laws"|"prompts"|"mixed", members: [ids], scope, owner, version }
//! ```
//! Unknown fields are kept, as in the document.
use crate::expr;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

type Extra = Map<String, Value>;
fn is_empty_map(m: &Extra) -> bool { m.is_empty() }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LawCheck {
    /// `regex` (needs `pattern`), `expr` (needs `expr`, a condition) or `jev` (needs `question`).
    pub kind: String,
    #[serde(rename = "onFail")]
    pub on_fail: String,
    #[serde(flatten, skip_serializing_if = "is_empty_map")]
    pub extra: Extra,
}

macro_rules! meta_fields {
    ($(#[$m:meta])* pub struct $name:ident { $($body:tt)* }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct $name {
            $($body)*
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub group: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub scope: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub owner: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub version: Option<u32>,
            #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
            pub updated_at: Option<u64>,
            #[serde(flatten, skip_serializing_if = "is_empty_map")]
            pub extra: Extra,
        }
    };
}

meta_fields! {
    /// A rule written in plain text that a node's AI must follow.
    pub struct Law {
        pub id: String,
        pub name: String,
        pub text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub check: Option<LawCheck>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tags: Option<Vec<String>>,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    pub name: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(flatten, skip_serializing_if = "is_empty_map")]
    pub extra: Extra,
}

meta_fields! {
    /// A named, reusable piece of prompt text with `{{slots}}`.
    pub struct Prompt {
        pub id: String,
        pub name: String,
        pub text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub slots: Option<Vec<Slot>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tags: Option<Vec<String>>,
    }
}

meta_fields! {
    /// A list of ids (a law can sit in many groups; deleting a group never deletes its members).
    pub struct Group {
        pub id: String,
        pub name: String,
        /// `laws`, `prompts` or `mixed`.
        pub kind: String,
        #[serde(default)]
        pub members: Vec<String>,
    }
}

impl Law {
    /// What is wrong with the law (empty: fine).
    pub fn problems(&self) -> Vec<String> {
        let mut out = vec![];
        if !self.id.starts_with("law-") { out.push(format!("a law id starts with `law-`, not `{}`", self.id)); }
        if self.name.trim().is_empty() { out.push("the law has no name".into()); }
        if self.text.trim().is_empty() { out.push("the law has no text".into()); }
        if let Some(c) = &self.check {
            if !["retry", "error", "warn"].contains(&c.on_fail.as_str()) { out.push(format!("check.onFail `{}` must be retry, error or warn", c.on_fail)); }
            let field = |k: &str| c.extra.get(k).and_then(Value::as_str);
            match c.kind.as_str() {
                "regex" => match field("pattern") { None => out.push("a regex check needs a `pattern`".into()), Some(p) => if let Err(e) = expr::regex_search(p, "") { out.push(format!("check.pattern: {e}")); } },
                "expr" => match field("expr") { None => out.push("an expr check needs an `expr`".into()), Some(e) => if let Err(e) = expr::parse_condition(e) { out.push(format!("check.expr: {e}")); } },
                "jev" => if field("question").is_none() { out.push("a jev check needs a `question`".into()); },
                k => out.push(format!("check.kind `{k}` must be regex, expr or jev")),
            }
        }
        out
    }
}

impl Prompt {
    pub fn problems(&self) -> Vec<String> {
        let mut out = vec![];
        if !self.id.starts_with("pr-") { out.push(format!("a prompt id starts with `pr-`, not `{}`", self.id)); }
        if self.name.trim().is_empty() { out.push("the prompt has no name".into()); }
        match expr::parse_template(&self.text) {
            Err(e) => out.push(format!("the text does not parse: {e}")),
            Ok(t) => for p in t.paths() {
                if p.len() == 1 && !self.slots.as_deref().unwrap_or(&[]).iter().any(|s| s.name == p[0]) { out.push(format!("`{{{{{}}}}}` is not a declared slot", p[0])); }
            },
        }
        out
    }

    /// The text with its slots filled from `values` (a slot's default fills a missing value; a slot with neither is an
    /// error).
    pub fn fill(&self, values: &Map<String, Value>) -> Result<String, expr::ExprError> {
        let slots = self.slots.as_deref().unwrap_or(&[]);
        let look = |p: &[String]| -> Option<Value> {
            let root = values.get(&p[0]).cloned().or_else(|| slots.iter().find(|s| s.name == p[0]).and_then(|s| s.default.clone()))?;
            p[1..].iter().try_fold(root, |v, k| match &v { Value::Array(a) => k.parse::<usize>().ok().and_then(|i| a.get(i)).cloned(), o => o.get(k).cloned() })
        };
        expr::parse_template(&self.text)?.render(&look, None)
    }
}

impl Group {
    pub fn problems(&self) -> Vec<String> {
        let mut out = vec![];
        if !self.id.starts_with("grp-") { out.push(format!("a group id starts with `grp-`, not `{}`", self.id)); }
        if !["laws", "prompts", "mixed"].contains(&self.kind.as_str()) { out.push(format!("the group kind `{}` must be laws, prompts or mixed", self.kind)); }
        for m in &self.members {
            let law = m.starts_with("law-");
            let pr = m.starts_with("pr-");
            if !(law || pr) { out.push(format!("member `{m}` is neither a law (law-..) nor a prompt (pr-..)")); }
            else if self.kind == "laws" && !law { out.push(format!("member `{m}` is not a law, in a laws group")); }
            else if self.kind == "prompts" && !pr { out.push(format!("member `{m}` is not a prompt, in a prompts group")); }
        }
        out
    }
}

/// `laws: [ <law id> | <group id> ... ]` as the laws they stand for, in order, each once (spec 6.2). Ids that match
/// nothing are returned in the second list.
pub fn expand_laws<'a>(ids: &[String], laws: &'a [Law], groups: &[Group]) -> (Vec<&'a Law>, Vec<String>) {
    let (mut out, mut missing): (Vec<&Law>, Vec<String>) = (vec![], vec![]);
    let add = |id: &str, out: &mut Vec<&'a Law>| -> bool { match laws.iter().find(|l| l.id == id) { Some(l) => { if !out.iter().any(|x| x.id == l.id) { out.push(l); } true } None => false } };
    for id in ids {
        if let Some(g) = groups.iter().find(|g| g.id == *id) { for m in &g.members { if m.starts_with("law-") && !add(m, &mut out) { missing.push(m.clone()); } } }
        else if !add(id, &mut out) { missing.push(id.clone()); }
    }
    (out, missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shapes_round_trip_with_unknown_fields() {
        let law: Law = serde_json::from_value(json!({ "id": "law-tone", "name": "Tone", "text": "Answer in the team's tone", "check": { "kind": "regex", "pattern": "^ok", "onFail": "retry" }, "tags": ["style"], "scope": "team-core", "owner": "gee", "version": 2, "updatedAt": 5, "future": [1] })).unwrap();
        assert!(law.problems().is_empty());
        assert_eq!(serde_json::to_value(&law).unwrap()["future"], json!([1]));
        assert_eq!(serde_json::to_value(&law).unwrap()["check"]["pattern"], "^ok");
        let mut bad = law.clone();
        bad.id = "x".into(); bad.check = Some(LawCheck { kind: "expr".into(), on_fail: "shrug".into(), extra: json!({ "expr": "a ==" }).as_object().unwrap().clone() });
        assert_eq!(bad.problems().len(), 3);
    }

    #[test]
    fn prompts_fill_slots() {
        let p: Prompt = serde_json::from_value(json!({ "id": "pr-sum", "name": "Summary", "text": "Summarise {{diff}} for {{audience}}", "slots": [{ "name": "diff", "type": "text" }, { "name": "audience", "default": "the team" }] })).unwrap();
        assert!(p.problems().is_empty());
        assert_eq!(p.fill(json!({ "diff": "D" }).as_object().unwrap()).unwrap(), "Summarise D for the team");
        assert!(p.fill(&Map::new()).unwrap_err().0.contains("diff"));
        let mut q = p.clone();
        q.text = "{{other}} {{oops".into();
        assert_eq!(q.problems().len(), 1);
        q.text = "{{other}}".into();
        assert!(q.problems()[0].contains("not a declared slot"));
    }

    #[test]
    fn groups_expand() {
        let l = |id: &str| Law { id: id.into(), name: id.into(), text: "t".into(), check: None, tags: None, group: None, scope: None, owner: None, version: None, updated_at: None, extra: Map::new() };
        let laws = vec![l("law-a"), l("law-b"), l("law-c")];
        let groups = vec![Group { id: "grp-s".into(), name: "s".into(), kind: "laws".into(), members: vec!["law-b".into(), "law-a".into(), "law-gone".into()], group: None, scope: None, owner: None, version: None, updated_at: None, extra: Map::new() }];
        let (got, missing) = expand_laws(&["law-a".into(), "grp-s".into(), "law-zzz".into()], &laws, &groups);
        assert_eq!(got.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["law-a", "law-b"]);
        assert_eq!(missing, ["law-gone", "law-zzz"]);
        let mut g = groups[0].clone();
        g.members = vec!["pr-x".into(), "nope".into()];
        assert_eq!(g.problems().len(), 2);
        g.kind = "mixed".into();
        assert_eq!(g.problems().len(), 1);
    }
}
