//! The person's answer to a draft: approve all, approve some (choose the parents), change (with a comment, sent back to
//! the AI as a revise) or reject. [`Flow`] turns that into `ask_user` questions for the terminal card and reads the
//! replies, so the card needs no code of its own; [`Decision`] is the outcome, with the hub's answer body
//! (`POST /api/work/plan/answer`: `approve: "all" | [indexes] | false`, `comment`, `revise`) both ways.
//!
//! A draft is answered once: [`Draft`] holds the state (open, writing, settled) like the hub's `plans` store, and
//! refuses a second answer.

use crate::view;
use crate::write::{write_plan, PlanHub};
use crate::s;
use serde_json::{json, Value};

/// How the person answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Everything: the parents, their sub-tasks and every change to existing tasks.
    ApproveAll,
    /// These parents (0-based) with their sub-tasks and the changes tied to them; `changes` adds the untied ones.
    ApproveSome { parents: Vec<usize>, changes: bool },
    /// Nothing written; the AI gets the comment and revises.
    Change(String),
    /// Nothing written; rejected, with an optional comment.
    Reject(String),
    /// The card was dismissed with no answer: nothing written, nobody told "no".
    Dismissed,
}

impl Decision {
    /// The hub's answer body for this decision (`session` and `id` are added by the caller).
    pub fn to_body(&self) -> Value {
        match self {
            Decision::ApproveAll => json!({ "approve": "all" }),
            Decision::ApproveSome { parents, changes } => json!({ "approve": parents, "changes": changes }),
            Decision::Change(c) => json!({ "approve": false, "revise": true, "comment": c }),
            Decision::Reject(c) => json!({ "approve": false, "comment": c }),
            Decision::Dismissed => json!({ "approve": false }),
        }
    }

    /// Read a hub answer body against a draft of `n` parents, with the hub's rules (`plan_answer`): `"all"` or `true`
    /// approves everything, a list approves those parents (out of range dropped), anything else is a revise (a comment is
    /// then required) or a reject.
    pub fn from_body(b: &Value, n: usize) -> Result<Decision, String> {
        let comment: String = s(b, "comment").trim().chars().take(4000).collect();
        let picked: Vec<usize> = match &b["approve"] {
            Value::String(t) if t == "all" => (0..n).collect(),
            Value::Bool(true) => (0..n).collect(),
            Value::Array(l) => {
                let mut v: Vec<usize> = l.iter().filter_map(|x| x.as_u64().map(|x| x as usize)).filter(|x| *x < n).collect();
                v.sort();
                v.dedup();
                v
            }
            _ => vec![],
        };
        if picked.is_empty() {
            let revise = b["revise"] == json!(true) || b["revise"] == json!(1);
            if revise && comment.is_empty() {
                return Err("Say what to change.".into());
            }
            return Ok(if revise { Decision::Change(comment) } else { Decision::Reject(comment) });
        }
        if picked.len() == n {
            return Ok(Decision::ApproveAll);
        }
        Ok(Decision::ApproveSome { parents: picked, changes: b["changes"] == json!(true) })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Choose,
    Pick,
    Comment { change: bool },
}

/// What [`Flow::feed`] says next.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Ask this (the arguments of an `ask_user` call) and [`Flow::feed`] the result.
    Ask(Value),
    Done(Decision),
}

/// The questions about one draft, one after the other.
#[derive(Debug, Clone)]
pub struct Flow {
    draft: Value,
    stage: Stage,
}

fn picked_of(result: &Value) -> Vec<String> {
    result["answers"][0]["picked"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn text_of(result: &Value) -> String {
    result["answers"][0]["text"].as_str().unwrap_or("").trim().to_string()
}

impl Flow {
    pub fn new(draft: Value) -> Flow {
        Flow { draft, stage: Stage::Choose }
    }

    fn n(&self) -> usize {
        self.draft["parents"].as_array().map(|p| p.len()).unwrap_or(0)
    }

    fn loose_changes(&self) -> bool {
        self.draft["existingChanges"].as_array().map(|c| c.iter().any(|x| x["parent"].is_null())).unwrap_or(false)
    }

    /// The question to ask now: the arguments of an `ask_user` call that waits until it is answered.
    pub fn question(&self) -> Value {
        match self.stage {
            Stage::Choose => json!({
                "title": "Plan draft",
                "timeoutSec": 0,
                "questions": [{
                    "id": "decision", "kind": "single", "other": false, "required": true,
                    "text": view::table(&self.draft),
                    "options": [
                        { "id": "all", "label": "Approve all", "hint": "write the parents, sub-tasks and changes" },
                        { "id": "some", "label": "Approve some", "hint": "choose which parents" },
                        { "id": "change", "label": "Change it", "hint": "say what; the AI revises the draft" },
                        { "id": "reject", "label": "Reject", "hint": "write nothing" },
                    ],
                }],
            }),
            Stage::Pick => {
                let mut options: Vec<Value> = self.draft["parents"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        let subs = p["subtasks"].as_array().map(|a| a.len()).unwrap_or(0);
                        json!({ "id": (i + 1).to_string(), "label": format!("{}. {} ({})", i + 1, s(p, "title"), s(p, "owner")), "hint": format!("{subs} sub-task{}", if subs == 1 { "" } else { "s" }) })
                    })
                    .collect();
                if self.loose_changes() {
                    options.push(json!({ "id": "changes", "label": "Also the changes to existing tasks that belong to no parent" }));
                }
                json!({
                    "title": "Plan draft: approve some",
                    "timeoutSec": 0,
                    "questions": [{
                        "id": "parents", "kind": "checklist", "required": true,
                        "text": "Tick the parents to write. Their sub-tasks and the changes tied to them come with them. Nothing ticked goes back to the first question.",
                        "options": options,
                    }],
                })
            }
            Stage::Comment { change } => json!({
                "title": if change { "Plan draft: change it" } else { "Plan draft: reject" },
                "timeoutSec": 0,
                "questions": [{
                    "id": "comment", "kind": "text",
                    "text": if change { "What should change? The AI gets your words and sends a new draft. (Empty goes back.)" } else { "Why? (optional, Enter to skip)" },
                }],
            }),
        }
    }

    /// Read the result of the question just asked.
    pub fn feed(&mut self, result: &Value) -> Step {
        match self.stage {
            Stage::Choose => {
                match picked_of(result).first().map(String::as_str) {
                    Some("all") => return Step::Done(Decision::ApproveAll),
                    Some("some") => self.stage = Stage::Pick,
                    Some("change") => self.stage = Stage::Comment { change: true },
                    Some("reject") => self.stage = Stage::Comment { change: false },
                    _ => return Step::Done(Decision::Dismissed),
                }
                Step::Ask(self.question())
            }
            Stage::Pick => {
                let picked = picked_of(result);
                let n = self.n();
                let mut parents: Vec<usize> = picked.iter().filter_map(|p| p.parse::<usize>().ok()).filter(|i| *i >= 1 && *i <= n).map(|i| i - 1).collect();
                parents.sort();
                parents.dedup();
                if parents.is_empty() {
                    self.stage = Stage::Choose;
                    return Step::Ask(self.question());
                }
                let changes = picked.iter().any(|p| p == "changes");
                if parents.len() == n && (changes || !self.loose_changes()) {
                    return Step::Done(Decision::ApproveAll);
                }
                Step::Done(Decision::ApproveSome { parents, changes })
            }
            Stage::Comment { change } => {
                let text = text_of(result);
                if change {
                    if text.is_empty() {
                        self.stage = Stage::Choose;
                        return Step::Ask(self.question());
                    }
                    Step::Done(Decision::Change(text))
                } else {
                    Step::Done(Decision::Reject(text))
                }
            }
        }
    }
}

/// Drive a whole [`Flow`] with `ask` (one `ask_user` call: arguments in, result out). An error from `ask` ends it.
pub fn run(draft: &Value, ask: &mut dyn FnMut(Value) -> Result<Value, String>) -> Result<Decision, String> {
    let mut flow = Flow::new(draft.clone());
    let mut q = flow.question();
    loop {
        let result = ask(q)?;
        match flow.feed(&result) {
            Step::Ask(next) => q = next,
            Step::Done(d) => return Ok(d),
        }
    }
}

/// An open draft: answered once, like the hub's store (`open` -> `writing` -> `settled`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Open,
    Writing,
    Settled,
}

/// A draft that waits for its answer, and what the answer does.
#[derive(Debug, Clone)]
pub struct Draft {
    pub draft: Value,
    pub state: State,
}

/// The tool result of an approval: the written plan ([`write_plan`]) plus `partial`, `approvedParents`, the person's
/// `comment` (when there is one) and the message the AI reads. The hub's shape; both the hub's answer route and
/// [`Draft::answer`] use it.
pub fn approve(hub: &dyn PlanHub, draft: &Value, picked: &[usize], with_changes: bool, comment: &str, day: &str) -> Value {
    let n = draft["parents"].as_array().map(|p| p.len()).unwrap_or(0);
    let mut out = write_plan(hub, draft, picked, with_changes, day);
    out["partial"] = json!(picked.len() < n);
    out["approvedParents"] = json!(picked);
    if !comment.is_empty() {
        out["comment"] = json!(comment);
    }
    let created: Vec<String> = out["created"].as_array().map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    out["message"] = json!(format!(
        "Approved and written{}: {}. Do not write these again.",
        hub.written_by(),
        if created.is_empty() { "nothing could be created (see errors)".to_string() } else { created.join(", ") }
    ));
    out
}

/// The tool result of a change request: nothing written, the AI revises and drafts again.
pub fn change_result(comment: &str) -> Value {
    json!({ "approved": false, "changeRequested": true, "comment": comment, "message": "Nothing was written. The person wants this changed: revise the draft and call plan_draft again." })
}

/// The tool result of a rejection: nothing written.
pub fn reject_result(comment: &str) -> Value {
    json!({ "approved": false, "rejected": true, "comment": comment, "message": "The person rejected the draft. Nothing was written." })
}

impl Draft {
    pub fn new(draft: Value) -> Draft {
        Draft { draft, state: State::Open }
    }

    /// Apply a decision. Approve writes through `hub` (the state is `Writing` meanwhile, so one answer wins); change,
    /// reject and dismiss write nothing. Returns the tool result the AI reads (the hub's shapes). `day` is today
    /// (`YYYY-MM-DD`) for the notes.
    pub fn answer(&mut self, decision: &Decision, hub: &dyn PlanHub, day: &str) -> Result<Value, String> {
        if self.state != State::Open {
            return Err(if self.state == State::Writing { "That draft is being written.".into() } else { "That draft was answered already.".into() });
        }
        let n = self.draft["parents"].as_array().map(|p| p.len()).unwrap_or(0);
        let out = match decision {
            Decision::ApproveAll | Decision::ApproveSome { .. } => {
                self.state = State::Writing;
                let (picked, with_changes): (Vec<usize>, bool) = match decision {
                    Decision::ApproveSome { parents, changes } => (parents.iter().copied().filter(|i| *i < n).collect(), *changes),
                    _ => ((0..n).collect(), true),
                };
                approve(hub, &self.draft, &picked, with_changes, "", day)
            }
            Decision::Change(c) => change_result(c),
            Decision::Reject(c) => reject_result(c),
            Decision::Dismissed => json!({ "approved": false, "dismissed": true, "message": "The person closed the draft without answering. Nothing was written; ask what they want (ask_user) before drafting again." }),
        };
        self.state = State::Settled;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{sample, FakeHub};

    fn picks(ids: &[&str]) -> Value {
        json!({ "answered": true, "answers": [{ "picked": ids }] })
    }
    fn says(t: &str) -> Value {
        json!({ "answered": true, "answers": [{ "picked": [], "text": t }] })
    }

    #[test]
    fn approve_all_is_one_question() {
        let d = run(&sample(), &mut |q| {
            assert_eq!(q["timeoutSec"], 0, "a plan never waits into a default");
            assert!(q["questions"][0]["text"].as_str().unwrap().contains("Draft view in the TUI"));
            Ok(picks(&["all"]))
        });
        assert_eq!(d.unwrap(), Decision::ApproveAll);
    }

    #[test]
    fn approve_some_asks_which_parents_and_numbers_them_from_one() {
        let mut asked: Vec<Value> = vec![];
        let mut answers = vec![picks(&["some"]), picks(&["2"])].into_iter();
        let d = run(&sample(), &mut |q| {
            asked.push(q);
            Ok(answers.next().unwrap())
        })
        .unwrap();
        assert_eq!(d, Decision::ApproveSome { parents: vec![1], changes: false });
        let opts = asked[1]["questions"][0]["options"].as_array().unwrap();
        assert_eq!(asked[1]["questions"][0]["kind"], "checklist");
        assert!(opts[0]["label"].as_str().unwrap().starts_with("1. Draft view in the TUI (gee)"));
        assert_eq!(opts[0]["hint"], "2 sub-tasks");
        assert_eq!(opts.len(), 2, "the only change is tied to a parent: no loose-changes option");
    }

    #[test]
    fn nothing_ticked_goes_back_and_all_ticked_is_approve_all() {
        let mut answers = vec![picks(&["some"]), picks(&[]), picks(&["some"]), picks(&["1", "2"])].into_iter();
        let mut n = 0;
        let d = run(&sample(), &mut |q| {
            n += 1;
            if n == 3 {
                assert_eq!(q["questions"][0]["id"], "decision", "back at the first question");
            }
            Ok(answers.next().unwrap())
        })
        .unwrap();
        assert_eq!(d, Decision::ApproveAll);
    }

    #[test]
    fn loose_changes_are_their_own_tick() {
        let mut d = sample();
        d["existingChanges"][0]["parent"] = Value::Null;
        let mut flow = Flow::new(d);
        flow.feed(&picks(&["some"]));
        assert!(flow.question()["questions"][0]["options"].as_array().unwrap().iter().any(|o| o["id"] == "changes"));
        assert_eq!(flow.clone().feed(&picks(&["1", "2"])), Step::Done(Decision::ApproveSome { parents: vec![0, 1], changes: false }));
        assert_eq!(flow.clone().feed(&picks(&["1", "2", "changes"])), Step::Done(Decision::ApproveAll));
        assert_eq!(flow.feed(&picks(&["1", "changes"])), Step::Done(Decision::ApproveSome { parents: vec![0], changes: true }));
    }

    #[test]
    fn change_needs_words_and_empty_goes_back() {
        let mut answers = vec![picks(&["change"]), says("  "), picks(&["change"]), says("split the first one")].into_iter();
        let d = run(&sample(), &mut |_| Ok(answers.next().unwrap())).unwrap();
        assert_eq!(d, Decision::Change("split the first one".into()));
    }

    #[test]
    fn reject_takes_an_optional_comment_and_a_dismissed_card_is_not_a_no() {
        let mut a = vec![picks(&["reject"]), says("")].into_iter();
        assert_eq!(run(&sample(), &mut |_| Ok(a.next().unwrap())).unwrap(), Decision::Reject(String::new()));
        let mut a = vec![picks(&["reject"]), says("too big")].into_iter();
        assert_eq!(run(&sample(), &mut |_| Ok(a.next().unwrap())).unwrap(), Decision::Reject("too big".into()));
        assert_eq!(run(&sample(), &mut |_| Ok(picks(&[]))).unwrap(), Decision::Dismissed);
        assert!(run(&sample(), &mut |_| Err("Run ended".into())).is_err());
    }

    #[test]
    fn the_hub_answer_body_means_the_same() {
        assert_eq!(Decision::from_body(&json!({ "approve": "all" }), 3).unwrap(), Decision::ApproveAll);
        assert_eq!(Decision::from_body(&json!({ "approve": true }), 3).unwrap(), Decision::ApproveAll);
        assert_eq!(Decision::from_body(&json!({ "approve": [2, 0, 0, 9] }), 3).unwrap(), Decision::ApproveSome { parents: vec![0, 2], changes: false });
        assert_eq!(Decision::from_body(&json!({ "approve": [0], "changes": true }), 3).unwrap(), Decision::ApproveSome { parents: vec![0], changes: true });
        assert_eq!(Decision::from_body(&json!({ "approve": false, "revise": true, "comment": "x" }), 3).unwrap(), Decision::Change("x".into()));
        assert_eq!(Decision::from_body(&json!({ "approve": false, "revise": true }), 3).unwrap_err(), "Say what to change.");
        assert_eq!(Decision::from_body(&json!({ "approve": false }), 3).unwrap(), Decision::Reject(String::new()));
        assert_eq!(Decision::from_body(&json!({ "approve": [] }), 3).unwrap(), Decision::Reject(String::new()));
        for d in [Decision::ApproveAll, Decision::ApproveSome { parents: vec![1], changes: true }, Decision::Change("c".into()), Decision::Reject("r".into())] {
            assert_eq!(Decision::from_body(&d.to_body(), 3).unwrap(), d);
        }
    }

    #[test]
    fn answers_write_only_on_approval_and_only_once() {
        let hub = FakeHub::default();
        let mut d = Draft::new(sample());
        let r = d.answer(&Decision::Change("again".into()), &hub, "2026-10-25").unwrap();
        assert_eq!(r["changeRequested"], true);
        assert!(hub.calls.borrow().is_empty(), "a change request writes nothing");
        assert!(d.answer(&Decision::ApproveAll, &hub, "2026-10-25").is_err(), "answered once");

        let mut d = Draft::new(sample());
        let r = d.answer(&Decision::Reject("no".into()), &hub, "2026-10-25").unwrap();
        assert_eq!(r["rejected"], true);
        assert!(hub.calls.borrow().is_empty());

        let mut d = Draft::new(sample());
        let r = d.answer(&Decision::Dismissed, &hub, "2026-10-25").unwrap();
        assert_eq!(r["dismissed"], true);
        assert!(hub.calls.borrow().is_empty());

        let mut d = Draft::new(sample());
        let r = d.answer(&Decision::ApproveAll, &hub, "2026-10-25").unwrap();
        assert_eq!(r["partial"], false);
        assert_eq!(r["created"].as_array().unwrap().len(), 4);
        assert!(r["message"].as_str().unwrap().contains("T-1") && r["message"].as_str().unwrap().starts_with("Approved and written: "));
        assert_eq!(d.state, State::Settled);
        assert!(d.answer(&Decision::ApproveAll, &hub, "2026-10-25").unwrap_err().contains("answered already"));
    }

    #[test]
    fn approving_some_writes_only_those_parents_and_what_is_tied_to_them() {
        let hub = FakeHub::default();
        let mut d = Draft::new(sample());
        let r = d.answer(&Decision::ApproveSome { parents: vec![1], changes: false }, &hub, "2026-10-25").unwrap();
        assert_eq!(r["partial"], true);
        assert_eq!(r["approvedParents"], json!([1]));
        assert_eq!(r["created"].as_array().unwrap().len(), 1, "parent 2 has no sub-tasks");
        assert!(r["changes"].as_array().unwrap().is_empty(), "the change belongs to parent 1");
        let creates: Vec<String> = hub.calls.borrow().iter().filter(|c| c.0 == "create").map(|c| c.1["title"].as_str().unwrap().to_string()).collect();
        assert_eq!(creates, vec!["Hub login for the writer".to_string()]);
    }
}
