//! The hub's side of a linked machine's tools (plans/linked-machines.md 4.2, 6, T-311): the job queue a machine's poll drains,
//! the answers that come back signed, and the routing of a run's PC / machine tool call to its owner's online machine.
//!
//! - [`MachineJobs::call`] signs the request (`machine::sign`: the owner, the actor taken from the run record, the tool, a
//!   digest of the exact arguments, a nonce, 60 s), queues it for that machine, and waits for the reply that carries its id.
//!   A reply that never comes is a clean timeout (the job is withdrawn if the machine never took it); an unlink fails
//!   every waiting call at once ("machine unlinked").
//! - The machine verifies the signature, the owner and the scope itself (second line); this is the first.
//! - [`MachineJobs::route`] decides where a call goes: a machine the caller NAMED (their own, else it falls through to the
//!   older checks), else their linked machine that is online and not paused, else on to the AI host / browser folder as before.
//! - Replies are matched by (machine record id, request id). The route that takes a reply has already checked the device
//!   signature (`Devices::verify_machine_reply`), so a body that gets here came from that machine's key.
use pgm_net_internal::machine::{Actor, Envelope, HubKey};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{oneshot, Notify};

use crate::machine_transfer::{Dir, Transfers};

/// The five tools of `pc_*`, which a machine answers exactly as the AI host does.
pub const FILE_TOOLS: [&str; 5] = ["list", "read", "write", "delete", "mkdir"];
/// The machine-only tools (6.1).
pub const MACHINE_TOOLS: [&str; 13] = ["machine_info", "shell_run", "job_start", "job_read", "job_stop", "proc_list", "proc_kill", "re_list", "re_run", "re_logs", "re_stop", "transfer_get", "transfer_put"];
/// A machine counts as listening this long after its last poll (the poll is held up to 25 s).
const LISTENING_MS: u64 = 40_000;
const MAX_QUEUE: usize = 64;

fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }
fn now_ms() -> u64 { pgm_core_internal::now_ms() }

/// One request as the machine receives it.
#[derive(Clone, Debug)]
pub struct Job { pub id: String, pub tool: String, pub body: String, pub sig: String }
impl Job { pub fn to_json(&self) -> Value { json!({ "id": self.id, "tool": self.tool, "body": self.body, "sig": self.sig }) } }

struct Pend { machine: String, tx: oneshot::Sender<Result<String, (u16, String)>> }
#[derive(Default)]
struct Inner { queues: HashMap<String, VecDeque<Job>>, pending: HashMap<String, Pend>, polled: HashMap<String, u64> }

pub struct MachineJobs {
    inner: Mutex<Inner>,
    pub notify: Notify,
    sign: Option<Arc<HubKey>>,
    pub xfer: Transfers,
}

/// Where a call went.
pub enum Routed { /** answered (by a machine, or a refusal that must not fall back) */ Done(Value), /** not for a machine: on to the host / browser */ Pass }

impl MachineJobs {
    pub fn new(root: &std::path::Path, sign: Option<Arc<HubKey>>, max_transfer_mb: u64) -> MachineJobs { MachineJobs { inner: Mutex::default(), notify: Notify::new(), sign, xfer: Transfers::new(root, max_transfer_mb) } }
    /// The hub's public key and its id (what a machine pins), when this hub signs.
    pub fn public_key(&self) -> Option<(String, String)> { self.sign.as_ref().map(|k| (k.public_hex(), k.kid())) }

    /// A machine's poll is open now.
    pub fn polled(&self, machine: &str) { lk(&self.inner).polled.insert(machine.to_string(), now_ms()); }
    pub fn polling(&self, machine: &str) -> bool { lk(&self.inner).polled.get(machine).map(|t| now_ms().saturating_sub(*t) < LISTENING_MS).unwrap_or(false) }
    /// What a machine's poll gets: everything queued for it.
    pub fn take(&self, machine: &str) -> Vec<Job> { lk(&self.inner).queues.get_mut(machine).map(|q| q.drain(..).collect()).unwrap_or_default() }
    pub fn queued(&self, machine: &str) -> usize { lk(&self.inner).queues.get(machine).map(|q| q.len()).unwrap_or(0) }
    /// A verified reply body for a request: hands it to the waiting call. False when nobody waits (late, or never asked).
    pub fn deliver(&self, machine: &str, request: &str, body: &str) -> bool {
        let p = { let mut g = lk(&self.inner); if g.pending.get(request).map(|p| p.machine == machine).unwrap_or(false) { g.pending.remove(request) } else { None } };
        match p { Some(p) => { let _ = p.tx.send(Ok(body.to_string())); true } None => false }
    }
    /// The machine was unlinked / revoked: its queue is dropped and every call waiting on it fails now.
    pub fn drop_machine(&self, machine: &str, why: &str) {
        let waiting: Vec<Pend> = { let mut g = lk(&self.inner); g.queues.remove(machine); g.polled.remove(machine); let ids: Vec<String> = g.pending.iter().filter(|(_, p)| p.machine == machine).map(|(k, _)| k.clone()).collect(); ids.into_iter().filter_map(|i| g.pending.remove(&i)).collect() };
        for p in waiting { let _ = p.tx.send(Err((503, why.to_string()))); }
    }

    /// One tool call to a machine: signed for `owner` with `actor`, waits at most `wait` for the signed reply.
    pub async fn call(&self, machine: &str, owner: &str, actor: &Actor, tool: &str, args: &Value, wait: Duration) -> Result<Value, (u16, String)> {
        if owner.is_empty() || owner != actor.account { return Err((403, "That machine belongs to someone else.".into())); }
        let Some(key) = &self.sign else { return Err((503, "This hub has no signing key, so it cannot ask a machine for anything.".into())) };
        let body = args.to_string();
        let env = Envelope::new(owner, actor.clone(), tool, body.as_bytes(), machine, (now_ms() / 1000) as i64);
        let job = Job { id: pgm_core_internal::crypto::random_hex(12), tool: tool.to_string(), body, sig: key.sign(&env).to_header() };
        let (tx, rx) = oneshot::channel();
        {
            let mut g = lk(&self.inner);
            let q = g.queues.entry(machine.to_string()).or_default();
            if q.len() >= MAX_QUEUE { return Err((429, "That machine has too many requests waiting.".into())); }
            q.push_back(job.clone());
            g.pending.insert(job.id.clone(), Pend { machine: machine.to_string(), tx });
        }
        self.notify.notify_waiters();
        let r = tokio::time::timeout(wait, rx).await;
        let withdraw = || { let mut g = lk(&self.inner); g.pending.remove(&job.id); if let Some(q) = g.queues.get_mut(machine) { q.retain(|j| j.id != job.id); } };
        match r {
            Ok(Ok(Ok(body))) => serde_json::from_str::<Value>(&body).map_err(|_| (502, "The machine's answer was not JSON.".to_string())),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => { withdraw(); Err((503, "The machine's answer was lost.".into())) }
            Err(_) => { withdraw(); Err((504, "The machine did not answer in time.".into())) }
        }
    }

    fn usable(&self, c: &Value) -> bool { c["paused"] != json!(true) && c["online"] == json!(true) && c["id"].as_str().map(|i| self.polling(i)).unwrap_or(false) }
    fn named_is(c: &Value, n: &str) -> bool { c["id"].as_str().map(|i| i.to_lowercase() == n).unwrap_or(false) || c["name"].as_str().map(|i| i.to_lowercase() == n).unwrap_or(false) }
    /// The card a call goes to when [`MachineJobs::route`] sends it to a machine (None: it passes on or is refused). The run notes it,
    /// so the Work bar and the owner's card can say which machine is in use (T-314, plans/linked-machines.md 4.6).
    pub fn pick<'a>(&self, cards: &'a [Value], named: &str, name: &str) -> Option<&'a Value> {
        if !FILE_TOOLS.contains(&name) && !MACHINE_TOOLS.contains(&name) { return None; }
        let n = named.trim().to_lowercase();
        if n.is_empty() { cards.iter().find(|c| self.usable(c)) } else { cards.iter().find(|c| Self::named_is(c, &n)).filter(|c| self.usable(c)) }
    }

    /// Where a PC / machine tool call goes (see the module docs). `cards` are the caller's own machines (`Devices::machines_of`).
    #[allow(clippy::too_many_arguments)]
    pub async fn route(&self, cards: &[Value], account: &str, named: &str, name: &str, args: &Value, actor: &Actor) -> Routed {
        let is_file = FILE_TOOLS.contains(&name);
        let is_machine = MACHINE_TOOLS.contains(&name);
        if !is_file && !is_machine { return Routed::Pass; }
        let usable = |c: &Value| self.usable(c);
        let n = named.trim().to_lowercase();
        let chosen: Option<&Value> = if n.is_empty() { cards.iter().find(|c| usable(c)) } else {
            match cards.iter().find(|c| Self::named_is(c, &n)) {
                Some(c) if usable(c) => Some(c),
                // the caller's own machine, but not listening: say so, never fall back to another place (T-308)
                Some(c) => return Routed::Done(json!({ "ok": false, "error": format!("Your machine {} is {} right now.", c["name"].as_str().unwrap_or(&n), if c["paused"] == json!(true) { "paused" } else { "offline" }) })),
                None => return Routed::Pass, // not one of theirs: the older host checks answer (404 / 403)
            }
        };
        let Some(card) = chosen else {
            if is_machine { return Routed::Done(json!({ "ok": false, "error": format!("{name} needs a linked machine of yours (the VibeRoller app on your computer, linked from Settings), and none is online right now.") })); }
            return Routed::Pass;
        };
        let id = card["id"].as_str().unwrap_or("").to_string();
        let mut args = args.clone();
        let mut wait = Duration::from_secs(60);
        match name {
            "shell_run" => wait = Duration::from_secs(args["timeoutSec"].as_u64().unwrap_or(600).clamp(1, 3600) + 30),
            "job_stop" => wait = Duration::from_secs(30),
            "transfer_get" | "transfer_put" => {
                let path = args["path"].as_str().unwrap_or("").to_string();
                if path.trim().is_empty() { return Routed::Done(json!({ "ok": false, "error": "path is the file on the machine." })); }
                let ttl = args["ticketSecs"].as_i64();
                if let Some(o) = args.as_object_mut() { o.remove("ticketSecs"); }
                let now = (now_ms() / 1000) as i64;
                let t = if name == "transfer_get" { self.xfer.issue(account, &id, &path, Dir::Get, None, None, ttl, now) } else {
                    let from = args["from"].as_str().unwrap_or("").to_string();
                    match self.xfer.meta(account, &from).filter(|m| m["complete"] == json!(true)) {
                        None => return Routed::Done(json!({ "ok": false, "error": "from is the id of a file you uploaded to the hub's transfer area (POST /api/transfers); none like that is kept for you." })),
                        Some(m) => {
                            if let Some(o) = args.as_object_mut() { o.insert("size".into(), m["size"].clone()); o.insert("sha256".into(), m["sha256"].clone()); }
                            self.xfer.issue(account, &id, &path, Dir::Put, Some(from), None, ttl, now)
                        }
                    }
                };
                match t {
                    Ok(t) => { if let Some(o) = args.as_object_mut() { o.insert("ticket".into(), json!(t.id)); o.insert("cap".into(), json!(t.cap)); } }
                    Err((s, m)) => return Routed::Done(json!({ "ok": false, "error": m, "_status": s })),
                }
                wait = Duration::from_secs(300);
            }
            _ => {}
        }
        match self.call(&id, account, actor, name, &args, wait).await {
            Ok(v) => Routed::Done(v),
            Err((st, m)) => Routed::Done(json!({ "ok": false, "error": format!("Your machine {}: {m}", card["name"].as_str().unwrap_or("")), "_status": st })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgm_net_internal::machine::Verifier;

    fn actor(a: &str) -> Actor { Actor { account: a.into(), session: "s1".into(), run: "r1".into() } }
    fn setup() -> (Arc<MachineJobs>, Arc<HubKey>, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("vr-mj-{}", pgm_core_internal::crypto::random_hex(4)));
        std::fs::create_dir_all(&d).unwrap();
        let k = Arc::new(HubKey::generate());
        (Arc::new(MachineJobs::new(&d, Some(k.clone()), 10)), k, d)
    }
    fn card(id: &str, online: bool, paused: bool) -> Value { json!({ "id": id, "name": format!("box-{id}"), "online": online, "paused": paused }) }

    #[tokio::test]
    async fn a_call_is_signed_queued_answered_and_matched_by_id() {
        let (j, k, d) = setup();
        let v = Verifier::new(&k.public_hex(), "sponge", "m1").unwrap();
        let j2 = j.clone();
        let worker = tokio::spawn(async move {
            loop {
                let jobs = j2.take("m1");
                if let Some(job) = jobs.first() {
                    let who = v.verify(&job.sig, &job.tool, job.body.as_bytes(), (now_ms() / 1000) as i64).unwrap();
                    assert_eq!(who.run, "r1");
                    assert!(j2.deliver("m1", &job.id, &json!({ "ok": true, "result": format!("did {}", job.tool) }).to_string()));
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        let r = j.call("m1", "sponge", &actor("sponge"), "list", &json!({ "path": "" }), Duration::from_secs(5)).await.unwrap();
        assert_eq!(r["result"], json!("did list"));
        worker.await.unwrap();
        // the owner check: an actor who is not the owner never gets a request signed
        assert_eq!(j.call("m1", "sponge", &actor("gee"), "list", &json!({}), Duration::from_secs(1)).await.unwrap_err().0, 403);
        assert_eq!(j.queued("m1"), 0);
        // a reply for nobody (late, wrong machine, wrong id) is not delivered
        assert!(!j.deliver("m1", "nope", "{}"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[tokio::test]
    async fn no_answer_times_out_cleanly_and_an_unlink_fails_waiting_calls_at_once() {
        let (j, _k, d) = setup();
        let r = j.call("m1", "sponge", &actor("sponge"), "list", &json!({}), Duration::from_millis(150)).await;
        assert_eq!(r.unwrap_err().0, 504);
        assert_eq!(j.queued("m1"), 0, "a job nobody took is withdrawn");
        let j2 = j.clone();
        let w = tokio::spawn(async move { j2.call("m2", "sponge", &actor("sponge"), "list", &json!({}), Duration::from_secs(30)).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t0 = std::time::Instant::now();
        j.drop_machine("m2", "machine unlinked");
        let e = w.await.unwrap().unwrap_err();
        assert!(e.0 == 503 && e.1.contains("unlinked") && t0.elapsed().as_secs() < 2);
        // a reply from another machine does not answer someone else's request
        let j3 = j.clone();
        let w = tokio::spawn(async move { j3.call("m1", "sponge", &actor("sponge"), "list", &json!({}), Duration::from_millis(400)).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let id = j.take("m1")[0].id.clone();
        assert!(!j.deliver("m9", &id, "{\"ok\":true}"));
        assert_eq!(w.await.unwrap().unwrap_err().0, 504);
        let _ = std::fs::remove_dir_all(d);
    }

    #[tokio::test]
    async fn routing_picks_the_owners_online_machine_and_never_falls_back_when_one_is_named() {
        let (j, _k, d) = setup();
        let a = actor("sponge");
        let none = |r: Routed| matches!(r, Routed::Pass);
        // nothing online: file tools pass on (host / browser), machine-only tools say they need one
        let cards = vec![card("m1", true, false)];
        assert!(none(j.route(&cards, "sponge", "", "list", &json!({}), &a).await), "online by the card but not polling: passes on");
        assert!(none(j.route(&[], "sponge", "", "write", &json!({}), &a).await));
        match j.route(&[], "sponge", "", "shell_run", &json!({ "cmd": "ls" }), &a).await { Routed::Done(v) => assert!(v["error"].as_str().unwrap().contains("needs a linked machine")), _ => panic!() }
        assert!(none(j.route(&cards, "sponge", "", "board_list", &json!({}), &a).await), "not a machine tool");
        // named and theirs but offline / paused: says so
        match j.route(&cards, "sponge", "box-m1", "list", &json!({}), &a).await { Routed::Done(v) => assert!(v["error"].as_str().unwrap().contains("offline")), _ => panic!() }
        match j.route(&[card("m1", true, true)], "sponge", "m1", "list", &json!({}), &a).await { Routed::Done(v) => assert!(v["error"].as_str().unwrap().contains("paused")), _ => panic!() }
        // named, not theirs: the older checks answer
        assert!(none(j.route(&cards, "sponge", "somebody-elses", "list", &json!({}), &a).await));
        // polling now: the call goes to m1 (a worker answers it)
        j.polled("m1");
        let j2 = j.clone();
        let w = tokio::spawn(async move { loop { if let Some(job) = j2.take("m1").first() { j2.deliver("m1", &job.id, &json!({ "ok": true, "result": "from m1" }).to_string()); return; } tokio::time::sleep(Duration::from_millis(10)).await; } });
        match j.route(&cards, "sponge", "", "list", &json!({}), &a).await { Routed::Done(v) => assert_eq!(v["result"], json!("from m1")), _ => panic!() }
        w.await.unwrap();
        // pick names the same card route uses (T-314), and none for what passes on or is refused
        assert_eq!(j.pick(&cards, "", "list").and_then(|c| c["id"].as_str()), Some("m1"));
        assert_eq!(j.pick(&cards, "BOX-M1", "shell_run").and_then(|c| c["id"].as_str()), Some("m1"));
        assert!(j.pick(&cards, "", "board_list").is_none() && j.pick(&cards, "other", "list").is_none() && j.pick(&[card("m1", true, true)], "", "list").is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[tokio::test]
    async fn transfer_calls_carry_a_ticket_bound_to_the_account_machine_and_path() {
        let (j, _k, d) = setup();
        let a = actor("sponge");
        j.polled("m1");
        let cards = vec![card("m1", true, false)];
        let j2 = j.clone();
        let w = tokio::spawn(async move { loop { if let Some(job) = j2.take("m1").first() { let args: Value = serde_json::from_str(&job.body).unwrap(); j2.deliver("m1", &job.id, &json!({ "ok": true, "result": args }).to_string()); return; } tokio::time::sleep(Duration::from_millis(10)).await; } });
        let v = match j.route(&cards, "sponge", "", "transfer_get", &json!({ "path": "big.bin", "ticketSecs": 5 }), &a).await { Routed::Done(v) => v, _ => panic!() };
        w.await.unwrap();
        let t = v["result"]["ticket"].as_str().unwrap().to_string();
        let now = (now_ms() / 1000) as i64;
        assert!(j.xfer.check(&t, "m1", "sponge", "big.bin", now).is_ok());
        assert!(j.xfer.check(&t, "m1", "sponge", "big.bin", now + 6).is_err(), "ticketSecs: 5");
        assert!(j.xfer.check(&t, "m1", "gee", "big.bin", now).is_err() && j.xfer.check(&t, "m1", "sponge", "other.bin", now).is_err());
        assert!(v["result"].get("ticketSecs").is_none(), "the hub keeps that argument");
        // a put needs a file the owner uploaded
        match j.route(&cards, "sponge", "", "transfer_put", &json!({ "path": "x.bin", "from": "01234567".repeat(4) }), &a).await { Routed::Done(v) => assert_eq!(v["ok"], json!(false)), _ => panic!() }
        let _ = std::fs::remove_dir_all(d);
    }
}
