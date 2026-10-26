//! `script` (spec 5.3) in the **transform** language: lines of `name = expression` in the condition language of
//! pgm-workflow-internal (`expr.rs`), evaluated in order; a later line sees the earlier names, the input ports (the
//! `in` port is `input`: `in` is the language's own operator), and the template values (`inputs`, `node`, `state`,
//! `loop`, `item`). The outputs are the names `config.outputs` lists. No
//! loops, no files, no network, no clock: nothing in the language can reach them. A step limit (`maxSteps`, default
//! 10000, counted over every part of every expression) stops an oversized script with `engine.script-steps`. Heavier
//! scripts in a sandbox come with custom nodes (T-203): any other `lang` is `engine.script-lang` here.
use super::boxed;
use crate::host::{walk, BoxFut, NodeCall, NodeError, NodeKind, NodeOutput, WorkflowHost};
use pgm_workflow_internal::expr::{self, Expr};
use serde_json::{Map, Value};

fn size(e: &Expr) -> u64 {
    1 + match e {
        Expr::Lit(_) | Expr::Path(_) => 0,
        Expr::List(v) | Expr::Call(_, v) => v.iter().map(size).sum(),
        Expr::Not(x) | Expr::Neg(x) => size(x),
        Expr::Bin(_, a, b) => size(a) + size(b),
    }
}

/// Parses a transform script: `(name, expression)` per line; blank lines and `#` comments are skipped.
pub fn parse(code: &str) -> Result<Vec<(String, Expr)>, String> {
    let mut out = vec![];
    for (i, line) in code.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') { continue; }
        let (name, rhs) = l.split_once('=').filter(|(_, r)| !r.starts_with('=')).ok_or_else(|| format!("line {}: `name = expression` expected", i + 1))?;
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') { return Err(format!("line {}: `{name}` cannot be a name", i + 1)); }
        let e = expr::parse_condition(rhs.trim()).map_err(|e| format!("line {}: {e}", i + 1))?;
        out.push((name.to_string(), e));
    }
    Ok(out)
}

pub struct Script;
impl NodeKind for Script {
    fn run<'a>(&'a self, call: NodeCall, _host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        boxed(async move {
            let id = &call.node.id;
            let lang = call.cfg("lang").and_then(Value::as_str).unwrap_or("transform");
            if lang != "transform" { return Err(NodeError::fatal("engine.script-lang", format!("node `{id}`: `{lang}` scripts are not available on this runner (only the transform language)"))); }
            let code = call.cfg("code").and_then(Value::as_str).unwrap_or("");
            let lines = parse(code).map_err(|e| NodeError::fatal("engine.script", format!("node `{id}`: {e}")))?;
            let max = call.cfg("maxSteps").and_then(Value::as_u64).unwrap_or(10_000);
            let steps: u64 = lines.iter().map(|(_, e)| size(e)).sum();
            if steps > max { return Err(NodeError::fatal("engine.script-steps", format!("node `{id}`: the script takes {steps} steps, more than its limit of {max}"))); }
            let mut names: Map<String, Value> = Map::new();
            for (name, e) in &lines {
                let v = {
                    let look = |p: &[String]| -> Option<Value> {
                        let (f, rest) = p.split_first()?;
                        if let Some(v) = names.get(f) { return walk(v, rest); }
                        if f == "input" { if let Some(v) = call.inputs.get("in") { return walk(v, rest); } }
                        call.lookup(p)
                    };
                    e.eval(&look).map_err(|x| NodeError::fatal("engine.script", format!("node `{id}`, `{name}`: {x}")))?
                };
                names.insert(name.clone(), v);
            }
            let wanted: Vec<String> = match call.cfg("outputs").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(|o| o.as_str().map(str::to_string).or_else(|| o.get("name").and_then(Value::as_str).map(str::to_string))).collect(),
                None => vec!["out".to_string()],
            };
            let mut out = NodeOutput::new();
            for w in wanted {
                let v = names.get(&w).cloned().ok_or_else(|| NodeError::fatal("engine.script", format!("node `{id}`: the script never sets its output `{w}`")))?;
                out = out.with(&w, v);
            }
            Ok(out)
        })
    }
}
