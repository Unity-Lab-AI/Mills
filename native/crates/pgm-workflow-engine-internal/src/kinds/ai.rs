//! The AI kinds (spec 5.1, 5.7): `prompt`, `chat`, `summarise`, `jev`. Each builds its messages from templates, asks
//! down its model chain (mod.rs) and turns the answer into outputs; the engine does retries, timeouts and the log.
use super::{add_usage, ask_model, boxed, model_chain, model_chain_from, render, text_of};
use crate::host::{BoxFut, NodeCall, NodeError, NodeKind, NodeOutput, WorkflowHost};
use pgm_workflow_internal::{expr, Node};
use serde_json::{json, Value};

fn options(call: &NodeCall) -> Value {
    let mut o = json!({});
    for k in ["temperature", "maxTokens", "responseFormat", "stream"] { if let Some(v) = call.cfg(k) { o[k] = v.clone(); } }
    o
}

fn msg(role: &str, content: &str) -> Value { json!({ "role": role, "content": content }) }

/// The JSON in an answer: the whole text, or the first fenced or `{...}` / `[...]` part of it.
pub fn json_in(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) { return Some(v); }
    let t = t.trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) { return Some(v); }
    for (o, c) in [('{', '}'), ('[', ']')] {
        if let (Some(a), Some(b)) = (t.find(o), t.rfind(c)) { if a < b { if let Ok(v) = serde_json::from_str::<Value>(&t[a..=b]) { return Some(v); } } }
    }
    None
}

/// The user text of a prompt node: its `user` template, else its `in` input as text.
fn user_text(call: &NodeCall, host: &dyn WorkflowHost, key: &str, port: &str) -> Result<String, NodeError> {
    match call.cfg(key).and_then(Value::as_str) {
        Some(t) => render(call, host, t, false),
        None => Ok(text_of(call.inputs.get(port).unwrap_or(&Value::Null))),
    }
}

fn system_text(call: &NodeCall, host: &dyn WorkflowHost) -> Result<Option<String>, NodeError> {
    call.cfg("system").and_then(Value::as_str).map(|t| render(call, host, t, false)).transpose()
}

// ---------------------------------------------------------------- prompt

/// `prompt`: one call (spec 5.1). `responseFormat: json | json-schema` parses the answer; a bad one is asked again
/// (`jsonRetries`, default 1) and then fails with `engine.bad-json`, which the node's own retry settings may retry.
pub struct Prompt;
impl NodeKind for Prompt {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        boxed(async move {
            let mut messages = vec![];
            if let Some(s) = system_text(&call, host)? { messages.push(msg("system", &s)); }
            messages.push(msg("user", &user_text(&call, host, "user", "in")?));
            let chain = model_chain(&call);
            let wants_json = matches!(call.cfg("responseFormat").and_then(Value::as_str), Some("json") | Some("json-schema"));
            let mut tries = call.cfg("jsonRetries").and_then(Value::as_u64).unwrap_or(1);
            let mut usage = None;
            loop {
                let a = ask_model(&call, host, "", &chain, messages.clone(), options(&call)).await?;
                usage = add_usage(usage, &a.usage);
                let mut out = NodeOutput::port("text", json!(a.text));
                if wants_json {
                    match json_in(&a.text) {
                        Some(v) => out = out.with("json", v),
                        None if tries > 0 => {
                            tries -= 1;
                            messages.push(msg("assistant", &a.text));
                            messages.push(msg("user", "That was not valid JSON. Answer again with the JSON only, nothing around it."));
                            continue;
                        }
                        None => return Err(NodeError::new("engine.bad-json", format!("node `{}`: the answer is not JSON", call.node.id))),
                    }
                }
                if let Some(u) = usage { out = out.with("usage", u.clone()).usage(u); }
                return Ok(out);
            }
        })
    }
}

// ---------------------------------------------------------------- chat

fn stops(cond: &str, text: &str, turn: u64) -> bool {
    if let Ok(c) = expr::parse_condition(cond) {
        let look = |p: &[String]| -> Option<Value> { match p.first().map(String::as_str) { Some("value") | Some("last") => Some(json!(text)), Some("turn") => Some(json!(turn)), _ => None } };
        if let Ok(b) = c.test(&look) { return b; }
    }
    text.to_lowercase().contains(&cond.to_lowercase())
}

/// `chat`: a conversation with a limit (spec 5.1). The node's model talks to a **partner** (`partner: {system, model}`,
/// an AI critic; by default the same model as a reviewer) until `maxTurns` (default 4) or until an answer of the node's
/// model meets `stopWhen` (a condition on `value` / `turn`, or a phrase it contains).
pub struct Chat;
impl NodeKind for Chat {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        boxed(async move {
            let system = system_text(&call, host)?;
            let opening = match call.cfg("opening").and_then(Value::as_str) { Some(t) => render(&call, host, t, false)?, None => text_of(call.inputs.get("in").unwrap_or(&Value::Null)) };
            let max = call.cfg("maxTurns").and_then(Value::as_u64).unwrap_or(4).clamp(1, 50);
            let stop = call.cfg("stopWhen").and_then(Value::as_str).map(str::to_string);
            let partner = call.cfg("partner").cloned().unwrap_or(json!({}));
            let p_system = partner.get("system").and_then(Value::as_str).unwrap_or("You review the other side's answer: say what is wrong or missing, briefly. Say DONE when nothing is.").to_string();
            let chain = model_chain(&call);
            let p_chain = model_chain_from(&call, partner.get("model").cloned());
            let mut convo = vec![msg("user", &opening)];
            let (mut last, mut turns, mut stopped, mut usage) = (String::new(), 0u64, "maxTurns", None);
            for turn in 1..=max {
                let mut ms = vec![];
                if let Some(s) = &system { ms.push(msg("system", s)); }
                ms.extend(convo.iter().cloned());
                let a = ask_model(&call, host, "", &chain, ms, options(&call)).await?;
                usage = add_usage(usage, &a.usage);
                convo.push(msg("assistant", &a.text));
                last = a.text;
                turns = turn;
                if stop.as_deref().is_some_and(|c| stops(c, &last, turn)) { stopped = "stopWhen"; break; }
                if turn == max { break; }
                // the partner sees the same conversation from the other side
                let mut ps = vec![msg("system", &p_system)];
                ps.extend(convo.iter().map(|m| msg(if m["role"] == "assistant" { "user" } else { "assistant" }, m["content"].as_str().unwrap_or(""))));
                let r = ask_model(&call, host, "#partner", &p_chain, ps, json!({})).await?;
                usage = add_usage(usage, &r.usage);
                convo.push(msg("user", &r.text));
            }
            let mut out = NodeOutput::port("messages", json!(convo)).with("last", json!(last)).with("turns", json!(turns)).with("stopped", json!(stopped));
            if let Some(u) = usage { out = out.usage(u); }
            Ok(out)
        })
    }
}

// ---------------------------------------------------------------- summarise

fn pieces(text: &str, size: usize) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    for para in text.split_inclusive("\n\n") {
        if !cur.is_empty() && cur.chars().count() + para.chars().count() > size { out.push(std::mem::take(&mut cur)); }
        if para.chars().count() > size {
            let cs: Vec<char> = para.chars().collect();
            for c in cs.chunks(size) { out.push(c.iter().collect()); }
        } else {
            cur.push_str(para);
        }
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

/// `summarise`: shrinks a text to a `target` (`{bullets}`, `{chars}` or `{tokens}`) in a `style`, keeping the `keep`
/// items (spec 5.1). A long text is cut into pieces (`pieceChars`, default 12000), each summarised, then merged. A
/// `keep` item that was in the text and is missing from the summary is added at its end: it stays, whatever the model did.
pub struct Summarise;
impl NodeKind for Summarise {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        boxed(async move {
            let text = text_of(call.inputs.get("text").unwrap_or(&Value::Null));
            let target = call.cfg("target").cloned().unwrap_or(json!({ "bullets": 8 }));
            let aim = if let Some(n) = target.get("bullets").and_then(Value::as_u64) { format!("at most {n} bullet points") }
                else if let Some(n) = target.get("chars").and_then(Value::as_u64) { format!("at most {n} characters") }
                else if let Some(n) = target.get("tokens").and_then(Value::as_u64) { format!("at most about {n} tokens") }
                else { "a short summary".to_string() };
            let style = call.cfg("style").and_then(Value::as_str).map(|s| format!(" Style: {s}.")).unwrap_or_default();
            let keep: Vec<String> = call.cfg("keep").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
            let keep_txt = if keep.is_empty() { String::new() } else { format!(" Keep these exactly as they are: {}.", keep.join(", ")) };
            let ask = format!("Summarise the text in {aim}.{style}{keep_txt} Answer with the summary only.");
            let chain = model_chain(&call);
            let size = call.cfg("pieceChars").and_then(Value::as_u64).unwrap_or(12_000).max(200) as usize;
            let parts = pieces(&text, size);
            let mut usage = None;
            let mut summary = if parts.len() <= 1 {
                let a = ask_model(&call, host, "", &chain, vec![msg("system", &ask), msg("user", &text)], json!({})).await?;
                usage = add_usage(usage, &a.usage);
                a.text
            } else {
                let mut partial = vec![];
                for (i, p) in parts.iter().enumerate() {
                    let a = ask_model(&call, host, &format!("#part{}", i + 1), &chain, vec![msg("system", &format!("Summarise this part of a longer text, keeping every fact that matters.{keep_txt}")), msg("user", p)], json!({})).await?;
                    usage = add_usage(usage, &a.usage);
                    partial.push(a.text);
                }
                let a = ask_model(&call, host, "#merge", &chain, vec![msg("system", &ask), msg("user", &partial.join("\n\n"))], json!({})).await?;
                usage = add_usage(usage, &a.usage);
                a.text
            };
            let missing: Vec<&String> = keep.iter().filter(|k| text.contains(k.as_str()) && !summary.contains(k.as_str())).collect();
            if !missing.is_empty() { summary.push_str(&format!("\n\nKept: {}", missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "))); }
            let kept: Vec<&String> = keep.iter().filter(|k| summary.contains(k.as_str())).collect();
            let mut out = NodeOutput::port("summary", json!(summary)).with("kept", json!(kept));
            if let Some(u) = usage { out = out.usage(u); }
            Ok(out)
        })
    }
}

// ---------------------------------------------------------------- jev

#[derive(Debug, Clone)]
struct Choice { id: String, label: String, hint: String }

fn choices(node: &Node) -> Vec<Choice> {
    if node.cfg_str("mode") == Some("yes-no") {
        return vec![Choice { id: "yes".into(), label: "yes".into(), hint: String::new() }, Choice { id: "no".into(), label: "no".into(), hint: String::new() }];
    }
    node.cfg("choices").and_then(Value::as_array).map(|a| a.iter().enumerate().map(|(i, c)| match c {
        Value::String(s) => Choice { id: s.clone(), label: s.clone(), hint: String::new() },
        o => {
            let id = o.get("id").or_else(|| o.get("name")).and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("case{}", i + 1));
            Choice { label: o.get("label").and_then(Value::as_str).unwrap_or(&id).to_string(), hint: o.get("hint").and_then(Value::as_str).unwrap_or("").to_string(), id }
        }
    }).collect()).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
struct Decision { choice: Option<String>, score: Option<f64>, ranking: Option<Vec<String>>, reason: String, confidence: f64 }

fn shape(mode: &str, v: &Value, ids: &[String]) -> Option<Decision> {
    let reason = v.get("reason").and_then(Value::as_str).unwrap_or("").to_string();
    let confidence = v.get("confidence").and_then(Value::as_f64).unwrap_or(1.0).clamp(0.0, 1.0);
    match mode {
        "score" => Some(Decision { choice: None, score: Some(v.get("score")?.as_f64()?), ranking: None, reason, confidence }),
        "rank" => {
            let r: Vec<String> = v.get("ranking")?.as_array()?.iter().filter_map(Value::as_str).map(str::to_string).collect();
            if r.is_empty() || r.iter().any(|x| !ids.contains(x)) { return None; }
            Some(Decision { choice: r.first().cloned(), score: None, ranking: Some(r), reason, confidence })
        }
        _ => {
            let c = v.get("choice")?.as_str()?.trim().to_string();
            let c = ids.iter().find(|i| i.eq_ignore_ascii_case(&c))?.clone();
            Some(Decision { choice: Some(c), score: None, ranking: None, reason, confidence })
        }
    }
}

/// `jev`: a **decision**, not a conversation (spec 5.7). Asks the decision model (`model`: `via: openrouter | ollama |
/// host`) for one of the choices as JSON `{choice, reason, confidence}` (`score`: `{score}`, `rank`: `{ranking}`),
/// `votes` times (the majority wins). An answer off the shape is asked again once, then fails (`engine.jev-shape`).
/// The model: the node's own, else the scope's decision model ([`WorkflowHost::decision_model`]), else the sections'
/// and the workflow's like any AI node; its `fallback` list is tried when one is unreachable.
/// Under `minConfidence`: `onLowConfidence` = `default` (config `default`, else the first choice), `second-opinion`
/// (`secondModel`, else the same chain, asked once more), `ask-person` (the run waits for a person to pick), `error`.
pub struct Jev;
impl NodeKind for Jev {
    fn run<'a>(&'a self, call: NodeCall, host: &'a dyn WorkflowHost) -> BoxFut<'a, Result<NodeOutput, NodeError>> {
        boxed(async move {
            let node = &call.node;
            let mode = node.cfg_str("mode").unwrap_or("pick").to_string();
            let cs = choices(node);
            let ids: Vec<String> = cs.iter().map(|c| c.id.clone()).collect();
            let context = call.inputs.get("context").cloned().unwrap_or(Value::Null);
            // the person was asked: their pick is the decision
            if let Some(a) = &call.answer {
                let pick = a.get("choice").and_then(Value::as_str).map(str::to_string).or_else(|| a.as_array().and_then(|x| x.first()).and_then(|x| x.as_str().map(str::to_string).or_else(|| x.as_array().and_then(|y| y.first()).and_then(Value::as_str).map(str::to_string))));
                if let Some(c) = pick.and_then(|p| ids.iter().find(|i| i.eq_ignore_ascii_case(&p)).cloned()) {
                    return Ok(outputs(&mode, &Decision { choice: Some(c), score: None, ranking: None, reason: "a person decided".into(), confidence: 1.0 }, &context, None));
                }
            }
            let question = match call.inputs.get("question").filter(|q| !q.is_null()) { Some(q) => text_of(q), None => match node.cfg_str("question") { Some(t) => render(&call, host, t, false)?, None => "Decide.".into() } };
            let listing = cs.iter().map(|c| if c.hint.is_empty() { format!("- {} ({})", c.id, c.label) } else { format!("- {} ({}): {}", c.id, c.label, c.hint) }).collect::<Vec<_>>().join("\n");
            let format = match mode.as_str() {
                "score" => "{\"score\": <a number from 0 to 10>, \"reason\": \"<one sentence>\", \"confidence\": <0 to 1>}".to_string(),
                "rank" => format!("{{\"ranking\": [<every id, best first>], \"reason\": \"<one sentence>\", \"confidence\": <0 to 1>}} with the ids {}", ids.join(", ")),
                _ => format!("{{\"choice\": \"<one of: {}>\", \"reason\": \"<one sentence>\", \"confidence\": <0 to 1>}}", ids.join(", ")),
            };
            let system = format!("You are a decision model. Decide; do not discuss. Answer with JSON only, exactly: {format}");
            let user = if listing.is_empty() { format!("Question: {question}\n\nContext:\n{}", text_of(&context)) } else { format!("Question: {question}\n\nChoices:\n{listing}\n\nContext:\n{}", text_of(&context)) };
            let votes = node.cfg("votes").and_then(Value::as_u64).unwrap_or(1).clamp(1, 9);
            let mut usage = None;
            // the node's own model, else the scope's decision model (the decision-model slot), else as any AI node
            let chain = model_chain_from(&call, call.cfg("model").cloned().or_else(|| host.decision_model()));
            let mut ds = vec![];
            for v in 0..votes {
                let suffix = if votes > 1 { format!("#vote{}", v + 1) } else { String::new() };
                ds.push(decide_once(&call, host, &suffix, &chain, &system, &user, &mode, &ids, &mut usage).await?);
            }
            let mut d = majority(&mode, ds);
            let min = node.cfg("minConfidence").and_then(Value::as_f64).unwrap_or(0.0);
            if d.confidence < min {
                match node.cfg_str("onLowConfidence").unwrap_or("error") {
                    "default" => { d.choice = Some(node.cfg_str("default").map(str::to_string).or_else(|| ids.first().cloned()).unwrap_or_default()); d.reason = format!("low confidence ({:.2}); the default was taken", d.confidence); }
                    "second-opinion" => {
                        let second = model_chain_from(&call, node.cfg("secondModel").cloned());
                        let s = decide_once(&call, host, "#second", &second, &system, &user, &mode, &ids, &mut usage).await?;
                        if s.confidence >= d.confidence { d = s; }
                    }
                    "ask-person" => {
                        let qs = json!([{ "text": format!("{question} (the decision model was unsure: {:.2})", d.confidence), "kind": "single", "options": ids, "default": d.choice }]);
                        return Err(NodeError::ask(qs, json!({ "model": d.choice, "confidence": d.confidence })));
                    }
                    _ => return Err(NodeError::fatal("engine.low-confidence", format!("node `{}`: the decision model was only {:.2} sure (at least {min} needed)", node.id, d.confidence))),
                }
            }
            Ok(outputs(&mode, &d, &context, usage))
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn decide_once(call: &NodeCall, host: &dyn WorkflowHost, suffix: &str, chain: &[Value], system: &str, user: &str, mode: &str, ids: &[String], usage: &mut Option<Value>) -> Result<Decision, NodeError> {
    let mut ms = vec![msg("system", system), msg("user", user)];
    for left in (0..2).rev() {
        let a = ask_model(call, host, suffix, chain, ms.clone(), json!({ "responseFormat": "json", "temperature": 0 })).await?;
        *usage = add_usage(usage.take(), &a.usage);
        if let Some(d) = json_in(&a.text).and_then(|v| shape(mode, &v, ids)) { return Ok(d); }
        if left == 0 { break; }
        ms.push(msg("assistant", &a.text));
        ms.push(msg("user", "That does not fit. Answer with the JSON only, in exactly the shape asked, with one of the given ids."));
    }
    Err(NodeError::new("engine.jev-shape", format!("node `{}`: the decision model's answer does not fit the shape", call.node.id)))
}

fn majority(mode: &str, ds: Vec<Decision>) -> Decision {
    if ds.len() == 1 { return ds.into_iter().next().unwrap(); }
    if mode == "score" {
        let n = ds.len() as f64;
        let mut d = ds[0].clone();
        d.score = Some(ds.iter().filter_map(|x| x.score).sum::<f64>() / n);
        d.confidence = ds.iter().map(|x| x.confidence).sum::<f64>() / n;
        return d;
    }
    let mut counts: Vec<(Option<String>, usize)> = vec![];
    for d in &ds { match counts.iter_mut().find(|c| c.0 == d.choice) { Some(c) => c.1 += 1, None => counts.push((d.choice.clone(), 1)) } }
    let best = counts.iter().max_by(|a, b| a.1.cmp(&b.1).then(std::cmp::Ordering::Greater)).map(|c| c.0.clone()).unwrap_or(None);
    let mine: Vec<&Decision> = ds.iter().filter(|d| d.choice == best).collect();
    let mut d = mine[0].clone();
    d.confidence = mine.iter().map(|x| x.confidence).sum::<f64>() / mine.len() as f64 * (mine.len() as f64 / ds.len() as f64);
    d.reason = format!("{} ({} of {} votes)", d.reason, mine.len(), ds.len());
    d
}

fn outputs(mode: &str, d: &Decision, context: &Value, usage: Option<Value>) -> NodeOutput {
    let mut out = NodeOutput::port("reason", json!(d.reason)).with("confidence", json!(d.confidence));
    match mode {
        "score" => out = out.with("score", json!(d.score)),
        "rank" => out = out.with("ranking", json!(d.ranking)),
        _ => {}
    }
    if let Some(c) = &d.choice {
        out = out.with("choice", json!(c));
        if mode != "score" && mode != "rank" { out = out.with(c, context.clone()); }
    }
    if let Some(u) = usage { out = out.usage(u); }
    out
}
