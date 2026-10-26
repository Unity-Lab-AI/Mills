//! A patch on a workflow document (T-206, the AI side chat): a list of the visual editor's commands (web/editor
//! core.js, T-205) in workflow terms, so the hub can try a patch the AI proposes before anyone sees it:
//! `add {kind, id?, x?, y?, props?}` (a node; `props` is its config), `connect {from: "n1.port", to: "n2.port", id?}`,
//! `disconnect {from, to}` or `{id}`, `setProp {ids, props}` or `{ids, key, value}`, `delete {ids}` (and their wires),
//! `move {ids, dx, dy}`, `place {positions: {id: [x, y]}}`. Anything else is refused: a patch only changes the drawing.
use pgm_workflow_internal::{Document, Edge, Node};
use serde_json::{json, Number, Value};

pub const COMMANDS: &[&str] = &["add", "connect", "disconnect", "setProp", "delete", "move", "place"];

fn nums(x: f64, y: f64) -> Option<Vec<Number>> { Some(vec![Number::from_f64(x)?, Number::from_f64(y)?]) }

/// Applies a patch to a copy of the document. Err: the index of the command that does not fit, and why.
pub fn apply(d: &Document, patch: &[Value]) -> Result<Document, (usize, String)> {
    let mut d = d.clone();
    let mut next = 1usize;
    for (i, c) in patch.iter().enumerate() {
        let fail = |m: String| (i, m);
        let ty = c.get("type").and_then(Value::as_str).unwrap_or("");
        let ids = || -> Vec<String> { c.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default() };
        match ty {
            "add" => {
                let kind = c.get("kind").and_then(Value::as_str).ok_or_else(|| fail("add needs a kind".into()))?;
                let id = match c.get("id").and_then(Value::as_str) {
                    Some(x) => x.to_string(),
                    None => { while d.node(&format!("n{next}")).is_some() { next += 1; } format!("n{next}") }
                };
                if d.node(&id).is_some() { return Err(fail(format!("id already used: {id}"))); }
                let mut n = Node::new(&id, kind);
                n.config = c.get("props").cloned().filter(Value::is_object);
                n.pos = nums(c.get("x").and_then(Value::as_f64).unwrap_or(0.0), c.get("y").and_then(Value::as_f64).unwrap_or(0.0));
                d.nodes.push(n);
            }
            "connect" => {
                let (from, to) = (c.get("from").and_then(Value::as_str).unwrap_or(""), c.get("to").and_then(Value::as_str).unwrap_or(""));
                for (end, what) in [(from, "from"), (to, "to")] {
                    let node = end.rsplit_once('.').map(|x| x.0).ok_or_else(|| fail(format!("{what} must be \"node.port\"")))?;
                    if d.node(node).is_none() { return Err(fail(format!("no such node: {node}"))); }
                }
                if d.edges.iter().any(|e| e.from == from && e.to == to) { return Err(fail("already connected".into())); }
                let id = match c.get("id").and_then(Value::as_str) { Some(x) => x.to_string(), None => { let mut k = d.edges.len() + 1; while d.edge(&format!("e{k}")).is_some() { k += 1; } format!("e{k}") } };
                d.edges.push(Edge::new(&id, from, to));
            }
            "disconnect" => {
                let before = d.edges.len();
                match c.get("id").and_then(Value::as_str) {
                    Some(id) => d.edges.retain(|e| e.id != id),
                    None => { let (f, t) = (c.get("from").and_then(Value::as_str).unwrap_or(""), c.get("to").and_then(Value::as_str).unwrap_or("")); d.edges.retain(|e| !(e.from == f && e.to == t)); }
                }
                if d.edges.len() == before { return Err(fail("no such wire".into())); }
            }
            "setProp" => {
                let set: serde_json::Map<String, Value> = match c.get("props").and_then(Value::as_object) {
                    Some(p) => p.clone(),
                    None => { let k = c.get("key").and_then(Value::as_str).ok_or_else(|| fail("setProp needs props or a key".into()))?; [(k.to_string(), c.get("value").cloned().unwrap_or(Value::Null))].into_iter().collect() }
                };
                for id in ids() {
                    let n = d.nodes.iter_mut().find(|n| n.id == id).ok_or_else(|| fail(format!("no such node: {id}")))?;
                    let mut cfg = n.config.clone().filter(Value::is_object).unwrap_or(json!({}));
                    for (k, v) in &set { if v.is_null() { cfg.as_object_mut().unwrap().remove(k); } else { cfg[k] = v.clone(); } }
                    n.config = Some(cfg);
                }
            }
            "delete" => {
                let gone = ids();
                for id in &gone { if d.node(id).is_none() { return Err(fail(format!("no such node: {id}"))); } }
                d.nodes.retain(|n| !gone.contains(&n.id));
                d.edges.retain(|e| !gone.iter().any(|g| e.from.starts_with(&format!("{g}.")) || e.to.starts_with(&format!("{g}."))));
            }
            "move" => {
                let (dx, dy) = (c.get("dx").and_then(Value::as_f64).unwrap_or(0.0), c.get("dy").and_then(Value::as_f64).unwrap_or(0.0));
                for id in ids() {
                    let n = d.nodes.iter_mut().find(|n| n.id == id).ok_or_else(|| fail(format!("no such node: {id}")))?;
                    let p = n.pos.clone().unwrap_or_default();
                    let (x, y) = (p.first().and_then(Number::as_f64).unwrap_or(0.0), p.get(1).and_then(Number::as_f64).unwrap_or(0.0));
                    n.pos = nums(x + dx, y + dy);
                }
            }
            "place" => {
                for (id, xy) in c.get("positions").and_then(Value::as_object).cloned().unwrap_or_default() {
                    let n = d.nodes.iter_mut().find(|n| n.id == id).ok_or_else(|| fail(format!("no such node: {id}")))?;
                    n.pos = nums(xy[0].as_f64().unwrap_or(0.0), xy[1].as_f64().unwrap_or(0.0));
                }
            }
            other => return Err(fail(format!("`{other}` is not a patch command (a patch only changes the drawing: {})", COMMANDS.join(", ")))),
        }
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patch_in_the_editors_commands() {
        let d = pgm_workflow_internal::doc::from_value(json!({ "schema": 1, "id": "w", "name": "w", "nodes": [{ "id": "s", "kind": "start" }, { "id": "e", "kind": "end" }], "edges": [{ "id": "e1", "from": "s.prompt", "to": "e.result" }] })).unwrap();
        let p = vec![json!({ "type": "add", "kind": "prompt", "id": "p", "x": 100, "y": 20, "props": { "user": "{{inputs.prompt}}" } }),
            json!({ "type": "disconnect", "from": "s.prompt", "to": "e.result" }), json!({ "type": "connect", "from": "s.prompt", "to": "p.in" }), json!({ "type": "connect", "from": "p.text", "to": "e.result" }),
            json!({ "type": "setProp", "ids": ["p"], "key": "system", "value": "Be brief." }), json!({ "type": "move", "ids": ["p"], "dx": 10, "dy": 0 })];
        let n = apply(&d, &p).unwrap();
        assert_eq!(n.nodes.len(), 3);
        assert_eq!(n.edges.iter().map(|e| format!("{}>{}", e.from, e.to)).collect::<Vec<_>>(), ["s.prompt>p.in", "p.text>e.result"]);
        assert_eq!(n.node("p").unwrap().cfg_str("system"), Some("Be brief."));
        assert_eq!(n.node("p").unwrap().pos.as_ref().unwrap()[0].as_f64(), Some(110.0));
        assert_eq!(apply(&d, &[json!({ "type": "publish" })]).unwrap_err().0, 0, "a patch only draws");
        assert!(apply(&d, &[json!({ "type": "connect", "from": "x.a", "to": "e.result" })]).unwrap_err().1.contains("no such node"));
        let gone = apply(&n, &[json!({ "type": "delete", "ids": ["p"] })]).unwrap();
        assert!(gone.edges.is_empty(), "its wires go with it");
    }
}
