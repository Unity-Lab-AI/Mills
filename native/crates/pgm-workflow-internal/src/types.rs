//! Port types and the rules for joining ports (spec 3.1).
//!
//! `text`, `number`, `bool`, `json` (any), `list<T>`, `file`, `messages`, `error`, and `flow` (no value: "go now").
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PortType {
    Text,
    Number,
    Bool,
    Json,
    File,
    Messages,
    Error,
    Flow,
    List(Box<PortType>),
}

impl PortType {
    /// `"text"`, `"list<file>"`, ... (surrounding spaces are fine); None when it is not a port type.
    pub fn parse(s: &str) -> Option<PortType> {
        let s = s.trim();
        Some(match s {
            "text" => PortType::Text,
            "number" => PortType::Number,
            "bool" => PortType::Bool,
            "json" => PortType::Json,
            "file" => PortType::File,
            "messages" => PortType::Messages,
            "error" => PortType::Error,
            "flow" => PortType::Flow,
            _ => {
                let inner = s.strip_prefix("list<")?.strip_suffix('>')?;
                PortType::List(Box::new(PortType::parse(inner)?))
            }
        })
    }

    /// Can a value of type `self` be given to an input of type `to`? Same type, or a safe conversion:
    /// number to text, anything to json (text to json as a string), and a list when its items convert.
    /// `flow` carries no value: it joins `flow` (and json, as a go signal with an empty value) and nothing else.
    pub fn flows_into(&self, to: &PortType) -> bool {
        if self == to || *to == PortType::Json { return true; }
        match (self, to) {
            (PortType::Number, PortType::Text) => true,
            (PortType::List(a), PortType::List(b)) => a.flows_into(b),
            _ => false,
        }
    }

    pub fn is_flow(&self) -> bool { *self == PortType::Flow }
}

impl fmt::Display for PortType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PortType::Text => f.write_str("text"),
            PortType::Number => f.write_str("number"),
            PortType::Bool => f.write_str("bool"),
            PortType::Json => f.write_str("json"),
            PortType::File => f.write_str("file"),
            PortType::Messages => f.write_str("messages"),
            PortType::Error => f.write_str("error"),
            PortType::Flow => f.write_str("flow"),
            PortType::List(t) => write!(f, "list<{t}>"),
        }
    }
}

/// One socket of a node kind.
#[derive(Debug, Clone, PartialEq)]
pub struct PortSpec {
    pub name: String,
    pub ty: PortType,
    /// An input with no edge and no default is an error when it is required.
    pub required: bool,
    /// An input that takes many edges (joins collect a list).
    pub many: bool,
    /// A default the kind itself gives (a value in the node's config can also be one).
    pub default: Option<serde_json::Value>,
}

impl PortSpec {
    pub fn new(name: &str, ty: PortType) -> Self { PortSpec { name: name.to_string(), ty, required: false, many: false, default: None } }
    pub fn required(mut self) -> Self { self.required = true; self }
    pub fn many(mut self) -> Self { self.many = true; self }
}

/// The ports a node ends up with (its kind's, plus any its config adds).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ports {
    pub inputs: Vec<PortSpec>,
    pub outputs: Vec<PortSpec>,
}

impl Ports {
    pub fn input(&self, name: &str) -> Option<&PortSpec> { self.inputs.iter().find(|p| p.name == name) }
    pub fn output(&self, name: &str) -> Option<&PortSpec> { self.outputs.iter().find(|p| p.name == name) }
}

/// `"n3.in"` -> `("n3", "in")`: the node id is everything before the last dot.
pub fn split_endpoint(s: &str) -> Option<(&str, &str)> {
    let (n, p) = s.rsplit_once('.')?;
    if n.is_empty() || p.is_empty() { None } else { Some((n, p)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn t(s: &str) -> PortType { PortType::parse(s).unwrap() }
    #[test]
    fn parse_and_show() {
        for s in ["text", "number", "bool", "json", "file", "messages", "error", "flow", "list<text>", "list<list<file>>"] { assert_eq!(t(s).to_string(), s); }
        assert!(PortType::parse("list<>").is_none() && PortType::parse("txt").is_none() && PortType::parse("list<nope>").is_none());
    }
    #[test]
    fn rules() {
        assert!(t("text").flows_into(&t("text")));
        assert!(t("number").flows_into(&t("text")) && !t("text").flows_into(&t("number")));
        assert!(t("text").flows_into(&t("json")) && t("list<file>").flows_into(&t("json")) && t("error").flows_into(&t("json")));
        assert!(!t("json").flows_into(&t("text")) && !t("bool").flows_into(&t("text")));
        assert!(t("list<number>").flows_into(&t("list<text>")) && !t("list<text>").flows_into(&t("list<number>")));
        assert!(t("flow").flows_into(&t("flow")) && !t("flow").flows_into(&t("text")) && !t("text").flows_into(&t("flow")));
        assert_eq!(split_endpoint("n3.in"), Some(("n3", "in")));
        assert_eq!(split_endpoint("a.b.c"), Some(("a.b", "c")));
        assert_eq!(split_endpoint("nodot"), None);
    }
}
