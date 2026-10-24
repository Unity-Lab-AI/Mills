//! The retrieval filter (Jcode-Mods DECISIONS item 62): when the earlier context is over a token budget, keep the
//! chunks most likely to matter for the latest user message and drop the rest. OFF by default; `Shadow` mode computes
//! what it WOULD drop and records it ([`AbRecord`]) without changing the request, so the savings and the "regret"
//! (a dropped chunk that was referenced anyway) are measured before it is ever turned on ([`AbSummary`]).
//!
//! Deterministic and pure: no I/O, no clock, no model. Guard rails: pinned chunks and the newest N turns are always
//! kept, and a tool_use / tool_result pair (same `pair_id`) is kept or dropped together, never split.
//!
//! The query match is BM25 over the request's chunks by default ([`Bm25`], behind the [`Ranker`] trait; `select_with`
//! takes any ranker, `Params::use_bm25 = false` restores the plain term overlap). [`apply_sticky`] makes applying the
//! filter prefix-stable: drops are permanent ([`DropLedger`]), batched onto every Nth turn and never for small chunks
//! ([`DropPolicy`]), with a stub per dropped chunk, so the cache is missed once per batch rather than every turn.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    ToolOutput,
    FileRead,
    UserMessage,
    AssistantMessage,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub kind: Kind,
    pub tokens: u64,
    /// How many turns ago the chunk entered the context (0 = this turn).
    pub age_turns: u32,
    /// The latest turn in which the model or a tool referred to the chunk, if ever.
    pub last_referenced_turn: Option<u32>,
    pub pinned: bool,
    /// Lower-case terms of the chunk's text (the caller tokenises, see [`terms`]).
    pub text_terms: Vec<String>,
    /// Occurrences of each entry of `text_terms` (same length), for BM25's term frequency. Empty = every term once
    /// (or, for a `with_terms` list that repeats a word, the repeats are counted). Old serialised chunks parse.
    #[serde(default)]
    pub term_counts: Vec<u32>,
    /// Shared by a tool_use and its tool_result: both are kept or both dropped.
    #[serde(default)]
    pub pair_id: Option<String>,
}

impl Chunk {
    pub fn new(id: &str, kind: Kind, tokens: u64, age_turns: u32) -> Chunk {
        Chunk {
            id: id.into(),
            kind,
            tokens,
            age_turns,
            last_referenced_turn: None,
            pinned: false,
            text_terms: vec![],
            term_counts: vec![],
            pair_id: None,
        }
    }
    pub fn with_text(mut self, text: &str) -> Chunk {
        let (t, c) = counted_terms(text);
        self.text_terms = t;
        self.term_counts = c;
        self
    }
    pub fn with_terms(mut self, t: &[&str]) -> Chunk {
        self.text_terms = t.iter().map(|s| s.to_lowercase()).collect();
        self.term_counts = vec![];
        self
    }
    pub fn pinned(mut self) -> Chunk {
        self.pinned = true;
        self
    }
    pub fn referenced(mut self, turn: u32) -> Chunk {
        self.last_referenced_turn = Some(turn);
        self
    }
    pub fn paired(mut self, pair: &str) -> Chunk {
        self.pair_id = Some(pair.into());
        self
    }
}

/// Lower-case alphanumeric / underscore words of 3+ chars, minus a few stop words, deduplicated, in first-seen order.
pub fn terms(text: &str) -> Vec<String> {
    counted_terms(text).0
}

/// [`terms`] plus how often each term occurs (same order, same filters): BM25 needs the term frequency.
pub fn counted_terms(text: &str) -> (Vec<String>, Vec<u32>) {
    const STOP: [&str; 14] = [
        "the", "and", "for", "with", "that", "this", "are", "you", "not", "but", "from", "have",
        "was", "can",
    ];
    let mut index: HashMap<String, usize> = HashMap::new();
    let (mut out, mut counts): (Vec<String>, Vec<u32>) = (vec![], vec![]);
    for w in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        let w = w.to_lowercase();
        if w.chars().count() < 3 || STOP.contains(&w.as_str()) {
            continue;
        }
        match index.get(&w) {
            Some(&i) => counts[i] += 1,
            None => {
                index.insert(w.clone(), out.len());
                out.push(w);
                counts.push(1);
            }
        }
    }
    (out, counts)
}

/// The term set of the latest user message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    terms: HashSet<String>,
}
impl Query {
    pub fn from_text(text: &str) -> Query {
        Query {
            terms: terms(text).into_iter().collect(),
        }
    }
    pub fn from_terms<S: AsRef<str>>(t: &[S]) -> Query {
        Query {
            terms: t.iter().map(|s| s.as_ref().to_lowercase()).collect(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
    pub fn len(&self) -> usize {
        self.terms.len()
    }
    /// Share of the query terms the chunk contains: 0.0 … 1.0 (0.0 for an empty query).
    pub fn overlap(&self, chunk: &Chunk) -> f32 {
        if self.terms.is_empty() {
            return 0.0;
        }
        let have: HashSet<String> = chunk.text_terms.iter().map(|t| t.to_lowercase()).collect();
        self.terms.iter().filter(|t| have.contains(*t)).count() as f32 / self.terms.len() as f32
    }
}

/// How chunks are scored and what is always kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// The turn being built; `last_referenced_turn` is measured against it.
    pub current_turn: u32,
    /// Chunks younger than this many turns are always kept (`age_turns < newest_turns`).
    pub newest_turns: u32,
    /// Half-life, in turns, of both recency decays.
    pub half_life_turns: f32,
    pub w_overlap: f32,
    pub w_recency: f32,
    pub w_reference: f32,
    pub w_tool_output: f32,
    pub w_file_read: f32,
    pub w_user: f32,
    pub w_assistant: f32,
    pub w_other: f32,
    /// `select` ranks the query match with BM25 over the request's chunks (default) instead of the plain share of
    /// query terms. Old serialised params parse as true.
    #[serde(default = "yes")]
    pub use_bm25: bool,
}

fn yes() -> bool {
    true
}

impl Default for Params {
    fn default() -> Params {
        Params {
            current_turn: 0,
            newest_turns: 2,
            half_life_turns: 4.0,
            w_overlap: 1.0,
            w_recency: 0.5,
            w_reference: 0.5,
            w_tool_output: 0.8,
            w_file_read: 1.0,
            w_user: 1.3,
            w_assistant: 1.0,
            w_other: 1.0,
            use_bm25: true,
        }
    }
}

impl Params {
    fn kind_weight(&self, k: Kind) -> f32 {
        match k {
            Kind::ToolOutput => self.w_tool_output,
            Kind::FileRead => self.w_file_read,
            Kind::UserMessage => self.w_user,
            Kind::AssistantMessage => self.w_assistant,
            Kind::Other => self.w_other,
        }
    }
    fn decay(&self, turns: u32) -> f32 {
        if self.half_life_turns <= 0.0 {
            return if turns == 0 { 1.0 } else { 0.0 };
        }
        0.5f32.powf(turns as f32 / self.half_life_turns)
    }
    /// True for chunks that must stay whatever the budget: pinned, or within the newest N turns.
    pub fn mandatory(&self, c: &Chunk) -> bool {
        c.pinned || c.age_turns < self.newest_turns
    }
}

/// The score of one chunk: kind weight × (overlap + recency decay + reference-recency decay, each weighted). Pinned
/// chunks score `f32::MAX`. Higher = more worth keeping.
pub fn score(chunk: &Chunk, query: &Query, params: &Params) -> f32 {
    score_with_match(chunk, query.overlap(chunk), params)
}

/// [`score`] with the query match given (a [`Ranker`]'s number in 0.0 … 1.0 instead of the term overlap).
fn score_with_match(chunk: &Chunk, matched: f32, params: &Params) -> f32 {
    if chunk.pinned {
        return f32::MAX;
    }
    let reference = chunk
        .last_referenced_turn
        .map(|t| params.decay(params.current_turn.saturating_sub(t)))
        .unwrap_or(0.0);
    params.kind_weight(chunk.kind)
        * (params.w_overlap * matched
            + params.w_recency * params.decay(chunk.age_turns)
            + params.w_reference * reference)
}

/// Scores how well each chunk matches the query, 0.0 … 1.0, index-aligned with `chunks` (the whole request is the
/// corpus, so a ranker may use corpus statistics). Must be deterministic. A cross-encoder can be a later impl behind
/// a feature; the match number then replaces the overlap term of [`score`] and the recency / kind weights still apply.
pub trait Ranker {
    fn rank(&self, chunks: &[Chunk], query: &Query) -> Vec<f32>;
    fn name(&self) -> &str;
}

/// The old measure as a ranker: the share of query terms the chunk contains.
#[derive(Debug, Clone, Copy, Default)]
pub struct OverlapRanker;
impl Ranker for OverlapRanker {
    fn rank(&self, chunks: &[Chunk], query: &Query) -> Vec<f32> {
        chunks.iter().map(|c| query.overlap(c)).collect()
    }
    fn name(&self) -> &str {
        "overlap"
    }
}

/// BM25 over the chunks of one request: document frequency from the chunks given (the corpus IS the request), term
/// frequency from [`Chunk::term_counts`], length = the chunk's term occurrences against the mean over the chunks. Rare
/// query terms weigh more than words every chunk has, which plain overlap cannot tell apart. The sum is divided by its
/// ceiling (`Σ idf·(k1+1)`) so the result stays in 0.0 … 1.0 and mixes with the recency terms like overlap did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bm25 {
    pub k1: f32,
    pub b: f32,
}
impl Default for Bm25 {
    fn default() -> Bm25 {
        Bm25 { k1: 1.2, b: 0.75 }
    }
}

fn term_freq(c: &Chunk) -> HashMap<String, u32> {
    let mut m: HashMap<String, u32> = HashMap::new();
    let counted = c.term_counts.len() == c.text_terms.len();
    for (i, t) in c.text_terms.iter().enumerate() {
        *m.entry(t.to_lowercase()).or_insert(0) += if counted && !c.term_counts.is_empty() {
            c.term_counts[i].max(1)
        } else {
            1
        };
    }
    m
}

impl Ranker for Bm25 {
    fn rank(&self, chunks: &[Chunk], query: &Query) -> Vec<f32> {
        let mut out = vec![0.0f32; chunks.len()];
        if chunks.is_empty() || query.terms.is_empty() {
            return out;
        }
        // sorted: float sums must not depend on hash-set order
        let mut q: Vec<&String> = query.terms.iter().collect();
        q.sort();
        let tfs: Vec<HashMap<String, u32>> = chunks.iter().map(term_freq).collect();
        let lens: Vec<f32> = tfs.iter().map(|m| m.values().sum::<u32>() as f32).collect();
        let n = chunks.len() as f32;
        let avg = (lens.iter().sum::<f32>() / n).max(1.0);
        let idf: Vec<f32> = q
            .iter()
            .map(|t| {
                let df = tfs.iter().filter(|m| m.contains_key(*t)).count() as f32;
                (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
            })
            .collect();
        let ceiling: f32 = idf.iter().map(|i| i * (self.k1 + 1.0)).sum();
        if ceiling <= 0.0 {
            return out;
        }
        for (i, m) in tfs.iter().enumerate() {
            let norm = self.k1 * (1.0 - self.b + self.b * lens[i] / avg);
            let mut s = 0.0f32;
            for (t, w) in q.iter().zip(&idf) {
                if let Some(&tf) = m.get(*t) {
                    let tf = tf as f32;
                    s += w * tf * (self.k1 + 1.0) / (tf + norm);
                }
            }
            out[i] = (s / ceiling).clamp(0.0, 1.0);
        }
        out
    }
    fn name(&self) -> &str {
        "bm25"
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reason {
    pub id: String,
    pub keep: bool,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Selection {
    /// Ids kept, in the order the chunks came in.
    pub keep: Vec<String>,
    pub drop: Vec<String>,
    pub reasons: Vec<Reason>,
    pub tokens_full: u64,
    pub tokens_kept: u64,
    /// True when the mandatory chunks alone exceed the budget (they are kept anyway).
    pub over_budget: bool,
}

/// Picks the chunks to keep within `budget_tokens`. Under budget everything is kept. Otherwise pinned and newest-N
/// chunks (and any pair they belong to) are kept first, then the rest by score, best first, while they fit. A pair
/// scores as its best member and costs both members; it is never split.
///
/// The query match is BM25 over the chunks (`params.use_bm25`, default) or the plain term overlap; see [`select_with`]
/// for any other [`Ranker`].
pub fn select(chunks: &[Chunk], query: &Query, budget_tokens: u64, params: &Params) -> Selection {
    if params.use_bm25 {
        select_with(chunks, query, budget_tokens, params, &Bm25::default())
    } else {
        select_with(chunks, query, budget_tokens, params, &OverlapRanker)
    }
}

/// [`select`] with the ranker given.
pub fn select_with(
    chunks: &[Chunk],
    query: &Query,
    budget_tokens: u64,
    params: &Params,
    ranker: &dyn Ranker,
) -> Selection {
    let tokens_full: u64 = chunks.iter().map(|c| c.tokens).sum();
    let mut sel = Selection {
        keep: vec![],
        drop: vec![],
        reasons: vec![],
        tokens_full,
        tokens_kept: 0,
        over_budget: false,
    };
    if tokens_full <= budget_tokens {
        for c in chunks {
            sel.keep.push(c.id.clone());
            sel.reasons.push(Reason {
                id: c.id.clone(),
                keep: true,
                why: "under budget".into(),
            });
        }
        sel.tokens_kept = tokens_full;
        return sel;
    }
    // groups: a pair is one group, everything else stands alone (indices into `chunks`)
    let mut groups: Vec<Vec<usize>> = vec![];
    let mut by_pair: HashMap<&str, usize> = HashMap::new();
    for (i, c) in chunks.iter().enumerate() {
        match c.pair_id.as_deref() {
            Some(p) => {
                let g = *by_pair.entry(p).or_insert_with(|| {
                    groups.push(vec![]);
                    groups.len() - 1
                });
                groups[g].push(i);
            }
            None => groups.push(vec![i]),
        }
    }
    let mut keep_group = vec![false; groups.len()];
    let mut why: Vec<String> = vec![String::new(); chunks.len()];
    let mut used = 0u64;
    let gtokens = |g: &Vec<usize>| g.iter().map(|&i| chunks[i].tokens).sum::<u64>();
    for (gi, g) in groups.iter().enumerate() {
        let pinned = g.iter().any(|&i| chunks[i].pinned);
        if pinned || g.iter().any(|&i| params.mandatory(&chunks[i])) {
            keep_group[gi] = true;
            used += gtokens(g);
            let w = if pinned { "pinned" } else { "newest turns" };
            for &i in g {
                why[i] = if g.len() > 1 && !(chunks[i].pinned || params.mandatory(&chunks[i])) {
                    format!("{w} (pair partner)")
                } else {
                    w.into()
                };
            }
        }
    }
    sel.over_budget = used > budget_tokens;
    let ranks = ranker.rank(chunks, query);
    let group_score = |g: &Vec<usize>| {
        g.iter()
            .map(|&i| score_with_match(&chunks[i], ranks.get(i).copied().unwrap_or(0.0), params))
            .fold(f32::MIN, f32::max)
    };
    let mut order: Vec<usize> = (0..groups.len()).filter(|&g| !keep_group[g]).collect();
    order.sort_by(|&a, &b| {
        let (sa, sb) = (group_score(&groups[a]), group_score(&groups[b]));
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| groups[a][0].cmp(&groups[b][0]).reverse()) // newer (later) first on equal score
    });
    for gi in order {
        let g = &groups[gi];
        let t = gtokens(g);
        let s = group_score(g);
        if used + t <= budget_tokens {
            keep_group[gi] = true;
            used += t;
            for &i in g {
                why[i] = format!("score {s:.2}, fits");
            }
        } else {
            for &i in g {
                why[i] = format!("score {s:.2}, over budget");
            }
        }
    }
    let kept: HashSet<usize> = groups
        .iter()
        .enumerate()
        .filter(|(gi, _)| keep_group[*gi])
        .flat_map(|(_, g)| g.iter().copied())
        .collect();
    for (i, c) in chunks.iter().enumerate() {
        let k = kept.contains(&i);
        if k {
            sel.keep.push(c.id.clone());
            sel.tokens_kept += c.tokens;
        } else {
            sel.drop.push(c.id.clone());
        }
        sel.reasons.push(Reason {
            id: c.id.clone(),
            keep: k,
            why: std::mem::take(&mut why[i]),
        });
    }
    sel
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    Shadow,
    On,
}

/// The filter's switch. Off by default: `enabled` is false and the mode is Off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FilterConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: Mode,
}

impl FilterConfig {
    /// The mode in force: Off unless `enabled`.
    pub fn effective(&self) -> Mode {
        if self.enabled {
            self.mode
        } else {
            Mode::Off
        }
    }
}

/// One shadow (or on) measurement for the ledger. `referenced_ids` is what the turn actually referred to, filled in
/// when the turn ends ([`AbRecord::with_references`]); the regret measure needs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbRecord {
    pub turn: u32,
    pub tokens_full: u64,
    pub tokens_filtered: u64,
    pub dropped_ids: Vec<String>,
    #[serde(default)]
    pub referenced_ids: Vec<String>,
    /// True when the sticky drop policy ([`apply_sticky`]) produced the record; lets the plugin tell the two modes
    /// apart in the ledger. Old lines parse as false.
    #[serde(default)]
    pub sticky: bool,
}

impl AbRecord {
    pub fn with_references(mut self, ids: &[&str]) -> AbRecord {
        self.referenced_ids = ids.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn tokens_saved(&self) -> u64 {
        self.tokens_full.saturating_sub(self.tokens_filtered)
    }
}

/// What the filter decided for one request.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// The chunk ids to put in the request: all of them unless the mode is On.
    pub send: Vec<String>,
    /// The selection, computed in Shadow and On only.
    pub selection: Option<Selection>,
    /// The ledger record, written in Shadow and On only.
    pub record: Option<AbRecord>,
    /// One stub per dropped chunk for the caller to put where the chunk was ([`apply_sticky`] with
    /// [`StubStyle::Reference`]); empty from [`apply`].
    pub stubs: Vec<Stub>,
}

/// The line that stands where a dropped chunk was, so the model knows it exists and how to get it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stub {
    pub id: String,
    pub text: String,
}

/// Runs the filter in the configured mode. Off: nothing computed, everything sent. Shadow: the selection and the
/// record are computed but everything is still sent. On: only the kept chunks are sent.
pub fn apply(
    cfg: &FilterConfig,
    chunks: &[Chunk],
    query: &Query,
    budget_tokens: u64,
    params: &Params,
) -> Applied {
    let all = || chunks.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
    let mode = cfg.effective();
    if mode == Mode::Off {
        return Applied {
            send: all(),
            selection: None,
            record: None,
            stubs: vec![],
        };
    }
    let sel = select(chunks, query, budget_tokens, params);
    let record = AbRecord {
        turn: params.current_turn,
        tokens_full: sel.tokens_full,
        tokens_filtered: sel.tokens_kept,
        dropped_ids: sel.drop.clone(),
        referenced_ids: vec![],
        sticky: false,
    };
    let send = if mode == Mode::On {
        sel.keep.clone()
    } else {
        all()
    };
    Applied {
        send,
        selection: Some(sel),
        record: Some(record),
        stubs: vec![],
    }
}

/// What may be dropped, and when, so that applying the filter does not wreck the prefix cache. Providers cache a
/// prefix: one changed byte early in the request invalidates everything after it (TOKENS.md §1). A plain `apply` in
/// On mode re-decides every turn, so a chunk can drop out, come back, and drop again, and each flip moves bytes in the
/// middle of the transcript. The policy makes drops rare, batched and permanent instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DropPolicy {
    /// Chunks (a pair counts as the sum of its members) under this many tokens are never dropped: the stub would cost
    /// nearly as much as the chunk, and the prefix would change for nothing.
    pub min_chunk_tokens: u64,
    /// New drops happen only on turns where `turn % every_n_turns == 0`, so the cache miss they cause is batched into
    /// one turn instead of paid on many. 0 or 1 = any turn.
    pub every_n_turns: u32,
    pub stub: StubStyle,
}
impl Default for DropPolicy {
    fn default() -> DropPolicy {
        DropPolicy {
            min_chunk_tokens: 400,
            every_n_turns: 4,
            stub: StubStyle::Reference,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StubStyle {
    /// A one-line stub per dropped chunk saying what it was and how to fetch it (reversible: nothing is silently lost).
    #[default]
    Reference,
    /// No stub; the chunk is simply absent (for callers that leave their own marker).
    Silent,
}

/// Where and when a chunk was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedAt {
    pub turn: u32,
    pub tokens: u64,
}

/// What the sticky policy has dropped so far in a session. A dropped chunk stays dropped on every later turn: if it came
/// back, that would itself be a change in the middle of the prefix. Persist it with the session (serde).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DropLedger {
    dropped: BTreeMap<String, DroppedAt>,
}
impl DropLedger {
    pub fn new() -> DropLedger {
        DropLedger::default()
    }
    pub fn is_dropped(&self, id: &str) -> bool {
        self.dropped.contains_key(id)
    }
    pub fn get(&self, id: &str) -> Option<&DroppedAt> {
        self.dropped.get(id)
    }
    pub fn len(&self) -> usize {
        self.dropped.len()
    }
    pub fn is_empty(&self) -> bool {
        self.dropped.is_empty()
    }
    /// Ids in a fixed (sorted) order.
    pub fn ids(&self) -> impl Iterator<Item = &String> {
        self.dropped.keys()
    }
    pub fn record(&mut self, id: &str, turn: u32, tokens: u64) {
        self.dropped
            .entry(id.to_string())
            .or_insert(DroppedAt { turn, tokens });
    }
    /// Lets a chunk come back (the model fetched it, or the session was compacted). An explicit, rare act.
    pub fn release(&mut self, id: &str) -> bool {
        self.dropped.remove(id).is_some()
    }
}

/// The stub for a dropped chunk. Deterministic in (id, tokens, turn): the same text on every later turn.
pub fn stub_text(id: &str, tokens: u64, turn: u32) -> String {
    format!(
        "[context: tool output {id} ({tokens} tokens) set aside at turn {turn}; context_fetch id={id} brings it back]"
    )
}

/// The filter with a prefix-stable drop policy, in On semantics (the caller sends `send` and puts `stubs` where the
/// dropped chunks were). Rules, in order:
/// 1. Everything in `ledger` that is still among `chunks` stays dropped, on every turn (sticky).
/// 2. On a turn where `turn % every_n_turns == 0` the ranker-based [`select`] runs over the remaining chunks; what it
///    drops becomes a new drop only if its group (a pair counts as one) has at least `min_chunk_tokens`.
/// 3. Pairs go together, pinned chunks and the newest `params.newest_turns` never drop ([`select`]'s guard rails).
///
/// New drops are written to `ledger`. The record carries `sticky: true`; its `tokens_filtered` includes the stubs.
pub fn apply_sticky(
    chunks: &[Chunk],
    query: &Query,
    budget_tokens: u64,
    params: &Params,
    policy: &DropPolicy,
    ledger: &mut DropLedger,
    turn: u32,
) -> Applied {
    let tokens_full: u64 = chunks.iter().map(|c| c.tokens).sum();
    let mut dropped: HashSet<usize> = HashSet::new();
    for (i, c) in chunks.iter().enumerate() {
        if ledger.is_dropped(&c.id) {
            dropped.insert(i);
        }
    }
    // a pair whose one half was dropped is dropped whole
    let mut pair_dropped: HashSet<&str> = HashSet::new();
    for &i in &dropped {
        if let Some(p) = chunks[i].pair_id.as_deref() {
            pair_dropped.insert(p);
        }
    }
    for (i, c) in chunks.iter().enumerate() {
        if c.pair_id.as_deref().map_or(false, |p| pair_dropped.contains(p)) {
            dropped.insert(i);
        }
    }
    let allowed = policy.every_n_turns <= 1 || turn % policy.every_n_turns == 0;
    let mut sel_over_budget = false;
    let mut new_reason: HashMap<usize, String> = HashMap::new();
    if allowed {
        let rest: Vec<Chunk> = chunks
            .iter()
            .enumerate()
            .filter(|(i, _)| !dropped.contains(i))
            .map(|(_, c)| c.clone())
            .collect();
        let index_of: Vec<usize> = (0..chunks.len()).filter(|i| !dropped.contains(i)).collect();
        let sel = select(&rest, query, budget_tokens, params);
        sel_over_budget = sel.over_budget;
        let want: HashSet<&str> = sel.drop.iter().map(String::as_str).collect();
        // eligibility is per group: a pair is judged by the sum of its members
        let mut group_tokens: HashMap<String, u64> = HashMap::new();
        let key = |c: &Chunk| c.pair_id.clone().unwrap_or_else(|| format!("\u{0}{}", c.id));
        for c in &rest {
            *group_tokens.entry(key(c)).or_insert(0) += c.tokens;
        }
        for (ri, c) in rest.iter().enumerate() {
            if !want.contains(c.id.as_str()) {
                continue;
            }
            if group_tokens[&key(c)] >= policy.min_chunk_tokens {
                dropped.insert(index_of[ri]);
                new_reason.insert(index_of[ri], "dropped now (score, over budget)".into());
            } else {
                new_reason.insert(
                    index_of[ri],
                    format!("kept: under {} tokens, not worth a stub", policy.min_chunk_tokens),
                );
            }
        }
    }
    let mut sel = Selection {
        keep: vec![],
        drop: vec![],
        reasons: vec![],
        tokens_full,
        tokens_kept: 0,
        over_budget: sel_over_budget,
    };
    let mut stubs = vec![];
    for (i, c) in chunks.iter().enumerate() {
        if dropped.contains(&i) {
            let new = !ledger.is_dropped(&c.id);
            if new {
                ledger.record(&c.id, turn, c.tokens);
            }
            let at = *ledger.get(&c.id).expect("recorded above");
            sel.drop.push(c.id.clone());
            sel.reasons.push(Reason {
                id: c.id.clone(),
                keep: false,
                why: if new {
                    new_reason.remove(&i).unwrap_or_else(|| "dropped now".into())
                } else {
                    format!("sticky: set aside at turn {}", at.turn)
                },
            });
            if policy.stub == StubStyle::Reference {
                let t = stub_text(&c.id, at.tokens, at.turn);
                sel.tokens_kept += (t.len() as u64 + 3) / 4;
                stubs.push(Stub { id: c.id.clone(), text: t });
            }
        } else {
            sel.keep.push(c.id.clone());
            sel.tokens_kept += c.tokens;
            sel.reasons.push(Reason {
                id: c.id.clone(),
                keep: true,
                why: new_reason.remove(&i).unwrap_or_else(|| {
                    if allowed {
                        "kept".into()
                    } else {
                        format!("kept: no new drops on turn {turn} (every {} turns)", policy.every_n_turns)
                    }
                }),
            });
        }
    }
    let record = AbRecord {
        turn,
        tokens_full,
        tokens_filtered: sel.tokens_kept,
        dropped_ids: sel.drop.clone(),
        referenced_ids: vec![],
        sticky: true,
    };
    Applied {
        send: sel.keep.clone(),
        selection: Some(sel),
        record: Some(record),
        stubs,
    }
}

/// Aggregates shadow records: how much the filter would have saved and how often it would have been wrong.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AbSummary {
    pub records: u64,
    pub tokens_full: u64,
    pub tokens_filtered: u64,
    pub tokens_saved: u64,
    /// Mean of per-record saved / full over records with a non-empty request (0.0 if none).
    pub mean_savings: f64,
    /// Chunks the filter would have dropped, counted once per record.
    pub dropped: u64,
    /// Of those, the ones a record at the same or a later turn referenced anyway.
    pub regretted: u64,
    /// regretted / dropped (0.0 when nothing was dropped).
    pub regret_rate: f64,
    /// Records that dropped something / records where a drop was regretted.
    pub records_with_drops: u64,
    pub records_with_regret: u64,
}

impl AbSummary {
    /// Regret: a chunk dropped (in shadow) at turn T that the model or a tool referenced at turn T or later. The
    /// references come from each record's `referenced_ids`.
    pub fn from_records<'a, I: IntoIterator<Item = &'a AbRecord>>(records: I) -> AbSummary {
        let recs: Vec<&AbRecord> = records.into_iter().collect();
        let mut last_ref: BTreeMap<&str, u32> = BTreeMap::new();
        for r in &recs {
            for id in &r.referenced_ids {
                let e = last_ref.entry(id.as_str()).or_insert(r.turn);
                if r.turn > *e {
                    *e = r.turn;
                }
            }
        }
        let mut s = AbSummary {
            records: recs.len() as u64,
            tokens_full: 0,
            tokens_filtered: 0,
            tokens_saved: 0,
            mean_savings: 0.0,
            dropped: 0,
            regretted: 0,
            regret_rate: 0.0,
            records_with_drops: 0,
            records_with_regret: 0,
        };
        let (mut sum, mut n) = (0.0f64, 0u64);
        for r in &recs {
            s.tokens_full += r.tokens_full;
            s.tokens_filtered += r.tokens_filtered;
            s.tokens_saved += r.tokens_saved();
            if r.tokens_full > 0 {
                sum += r.tokens_saved() as f64 / r.tokens_full as f64;
                n += 1;
            }
            let ids: HashSet<&str> = r.dropped_ids.iter().map(String::as_str).collect();
            if ids.is_empty() {
                continue;
            }
            s.records_with_drops += 1;
            s.dropped += ids.len() as u64;
            let bad = ids
                .iter()
                .filter(|id| last_ref.get(*id).map_or(false, |&t| t >= r.turn))
                .count() as u64;
            s.regretted += bad;
            if bad > 0 {
                s.records_with_regret += 1;
            }
        }
        if n > 0 {
            s.mean_savings = sum / n as f64;
        }
        if s.dropped > 0 {
            s.regret_rate = s.regretted as f64 / s.dropped as f64;
        }
        s
    }

    /// The decision the measurement is for: enough saved and little enough regretted.
    pub fn worth_enabling(&self, min_mean_savings: f64, max_regret_rate: f64) -> bool {
        self.records > 0
            && self.mean_savings >= min_mean_savings
            && self.regret_rate <= max_regret_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Params {
        Params {
            current_turn: 10,
            ..Params::default()
        }
    }
    fn q(t: &[&str]) -> Query {
        Query::from_terms(t)
    }

    #[test]
    fn terms_lowercase_dedupe_and_drop_noise() {
        assert_eq!(
            terms("The Parser FAILS in parser_v2, a b"),
            vec!["parser", "fails", "parser_v2"]
        );
        assert!(Query::from_text("").is_empty());
        assert_eq!(Query::from_text("fix the bug bug").len(), 2);
    }

    #[test]
    fn overlap_is_the_share_of_query_terms() {
        let c = Chunk::new("a", Kind::ToolOutput, 10, 5).with_terms(&["Parser", "token"]);
        assert!((q(&["parser", "lexer"]).overlap(&c) - 0.5).abs() < 1e-6);
        assert_eq!(q(&[]).overlap(&c), 0.0);
        assert_eq!(q(&["zzz"]).overlap(&c), 0.0);
    }

    #[test]
    fn score_orders_by_overlap() {
        let hit = Chunk::new("hit", Kind::ToolOutput, 10, 6).with_terms(&["parser", "lexer"]);
        let half = Chunk::new("half", Kind::ToolOutput, 10, 6).with_terms(&["parser"]);
        let miss = Chunk::new("miss", Kind::ToolOutput, 10, 6).with_terms(&["css"]);
        let qq = q(&["parser", "lexer"]);
        assert!(score(&hit, &qq, &p()) > score(&half, &qq, &p()));
        assert!(score(&half, &qq, &p()) > score(&miss, &qq, &p()));
    }

    #[test]
    fn score_orders_by_recency_and_reference() {
        let young = Chunk::new("y", Kind::ToolOutput, 10, 3);
        let old = Chunk::new("o", Kind::ToolOutput, 10, 30);
        assert!(score(&young, &q(&[]), &p()) > score(&old, &q(&[]), &p()));
        let refd = Chunk::new("r", Kind::ToolOutput, 10, 30).referenced(9);
        let stale = Chunk::new("s", Kind::ToolOutput, 10, 30).referenced(1);
        assert!(score(&refd, &q(&[]), &p()) > score(&stale, &q(&[]), &p()));
        assert!(score(&stale, &q(&[]), &p()) > score(&old, &q(&[]), &p()));
    }

    #[test]
    fn kind_weights_apply_and_pinned_scores_max() {
        let u = Chunk::new("u", Kind::UserMessage, 10, 5);
        let t = Chunk::new("t", Kind::ToolOutput, 10, 5);
        assert!(score(&u, &q(&[]), &p()) > score(&t, &q(&[]), &p()));
        assert_eq!(
            score(
                &Chunk::new("p", Kind::Other, 10, 99).pinned(),
                &q(&[]),
                &p()
            ),
            f32::MAX
        );
    }

    #[test]
    fn zero_half_life_does_not_divide_by_zero() {
        let pp = Params {
            half_life_turns: 0.0,
            ..p()
        };
        assert!(score(
            &Chunk::new("a", Kind::Other, 1, 3).referenced(2),
            &q(&["x"]),
            &pp
        )
        .is_finite());
    }

    #[test]
    fn under_budget_keeps_everything() {
        let cs = vec![
            Chunk::new("a", Kind::ToolOutput, 10, 5),
            Chunk::new("b", Kind::FileRead, 10, 6),
        ];
        let s = select(&cs, &q(&["x"]), 100, &p());
        assert_eq!(s.keep, vec!["a", "b"]);
        assert!(s.drop.is_empty());
        assert_eq!(s.tokens_kept, 20);
        assert!(!s.over_budget);
        assert_eq!(s.reasons[0].why, "under budget");
    }

    #[test]
    fn over_budget_drops_the_lowest_scores_and_respects_the_budget() {
        let cs = vec![
            Chunk::new("css", Kind::ToolOutput, 40, 8).with_terms(&["css"]),
            Chunk::new("parser", Kind::ToolOutput, 40, 8).with_terms(&["parser"]),
            Chunk::new("misc", Kind::ToolOutput, 40, 8).with_terms(&["misc"]),
        ];
        let s = select(&cs, &q(&["parser"]), 80, &p());
        assert!(s.keep.contains(&"parser".to_string()));
        assert_eq!(s.keep.len(), 2);
        assert_eq!(s.drop.len(), 1);
        assert!(s.tokens_kept <= 80);
        assert!(!s.over_budget);
        assert!(s.reasons.iter().all(|r| !r.why.is_empty()));
        assert_eq!(s.reasons.len(), 3);
    }

    #[test]
    fn pinned_and_newest_turns_are_always_kept() {
        let cs = vec![
            Chunk::new("sys", Kind::Other, 500, 50).pinned(),
            Chunk::new("old", Kind::ToolOutput, 50, 20),
            Chunk::new("new0", Kind::ToolOutput, 50, 0),
            Chunk::new("new1", Kind::ToolOutput, 50, 1),
            Chunk::new("mid", Kind::ToolOutput, 50, 2),
        ];
        let s = select(&cs, &q(&["old", "mid"]), 200, &p());
        for id in ["sys", "new0", "new1"] {
            assert!(s.keep.contains(&id.to_string()), "{id}");
        }
        assert!(
            s.over_budget,
            "mandatory chunks exceed the budget but are kept"
        );
        assert_eq!(s.drop, vec!["old", "mid"]);
    }

    #[test]
    fn keep_and_drop_keep_the_input_order() {
        let cs: Vec<Chunk> = (0..6)
            .map(|i| Chunk::new(&format!("c{i}"), Kind::ToolOutput, 30, 5 + i))
            .collect();
        let s = select(&cs, &q(&[]), 90, &p());
        let mut sorted = s.keep.clone();
        sorted.sort();
        assert_eq!(s.keep, sorted);
        assert_eq!(s.keep.len() + s.drop.len(), 6);
    }

    #[test]
    fn a_pair_is_never_split() {
        let cs = vec![
            Chunk::new("use", Kind::AssistantMessage, 20, 8).paired("t1"),
            Chunk::new("filler", Kind::ToolOutput, 30, 8).with_terms(&["parser"]),
            Chunk::new("res", Kind::ToolOutput, 30, 8)
                .with_terms(&["parser"])
                .paired("t1"),
            Chunk::new("other", Kind::ToolOutput, 30, 8),
        ];
        for budget in [0u64, 20, 49, 50, 60, 79, 80, 100] {
            let s = select(&cs, &q(&["parser"]), budget, &p());
            assert_eq!(
                s.keep.contains(&"use".to_string()),
                s.keep.contains(&"res".to_string()),
                "budget {budget}"
            );
            if !s.over_budget {
                assert!(s.tokens_kept <= budget || budget >= 110, "budget {budget}");
            }
        }
    }

    #[test]
    fn a_mandatory_pair_member_pulls_its_partner_in() {
        let cs = vec![
            Chunk::new("use", Kind::AssistantMessage, 20, 9).paired("t1"),
            Chunk::new("res", Kind::ToolOutput, 20, 0).paired("t1"),
            Chunk::new("x", Kind::ToolOutput, 50, 9),
        ];
        let s = select(&cs, &q(&[]), 45, &p());
        assert_eq!(s.keep, vec!["use", "res"]);
        assert!(s.reasons[0].why.contains("pair partner"));
    }

    #[test]
    fn pinned_pair_member_keeps_both() {
        let cs = vec![
            Chunk::new("use", Kind::AssistantMessage, 20, 9)
                .pinned()
                .paired("t1"),
            Chunk::new("res", Kind::ToolOutput, 20, 9).paired("t1"),
            Chunk::new("x", Kind::ToolOutput, 50, 9),
        ];
        assert_eq!(select(&cs, &q(&[]), 30, &p()).keep, vec!["use", "res"]);
    }

    #[test]
    fn a_pair_that_does_not_fit_is_dropped_whole_and_smaller_ones_still_fit() {
        let cs = vec![
            Chunk::new("use", Kind::AssistantMessage, 40, 8)
                .paired("t1")
                .with_terms(&["parser"]),
            Chunk::new("res", Kind::ToolOutput, 40, 8)
                .paired("t1")
                .with_terms(&["parser"]),
            Chunk::new("small", Kind::ToolOutput, 10, 8),
        ];
        let s = select(&cs, &q(&["parser"]), 50, &p());
        assert_eq!(s.drop, vec!["use", "res"]);
        assert_eq!(s.keep, vec!["small"]);
    }

    #[test]
    fn config_is_off_by_default() {
        let c = FilterConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.mode, Mode::Off);
        assert_eq!(c.effective(), Mode::Off);
        assert_eq!(
            FilterConfig {
                enabled: false,
                mode: Mode::On
            }
            .effective(),
            Mode::Off
        );
        assert_eq!(
            FilterConfig {
                enabled: true,
                mode: Mode::Shadow
            }
            .effective(),
            Mode::Shadow
        );
        let parsed: FilterConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, c);
        let m: FilterConfig = serde_json::from_str(r#"{"enabled":true,"mode":"shadow"}"#).unwrap();
        assert_eq!(m.mode, Mode::Shadow);
    }

    fn big() -> Vec<Chunk> {
        (0..5)
            .map(|i| Chunk::new(&format!("c{i}"), Kind::ToolOutput, 100, 5 + i))
            .collect()
    }

    #[test]
    fn off_changes_nothing() {
        let a = apply(&FilterConfig::default(), &big(), &q(&["x"]), 100, &p());
        assert_eq!(a.send.len(), 5);
        assert!(a.selection.is_none());
        assert!(a.record.is_none());
        let off = FilterConfig {
            enabled: true,
            mode: Mode::Off,
        };
        assert!(apply(&off, &big(), &q(&["x"]), 100, &p()).record.is_none());
    }

    #[test]
    fn shadow_records_but_keeps_everything() {
        let cfg = FilterConfig {
            enabled: true,
            mode: Mode::Shadow,
        };
        let a = apply(&cfg, &big(), &q(&["x"]), 200, &p());
        assert_eq!(a.send.len(), 5);
        let r = a.record.unwrap();
        assert_eq!(r.turn, 10);
        assert_eq!(r.tokens_full, 500);
        assert_eq!(r.tokens_filtered, 200);
        assert_eq!(r.dropped_ids.len(), 3);
        assert_eq!(r.tokens_saved(), 300);
        assert_eq!(a.selection.unwrap().drop, r.dropped_ids);
    }

    #[test]
    fn on_sends_only_the_kept() {
        let cfg = FilterConfig {
            enabled: true,
            mode: Mode::On,
        };
        let a = apply(&cfg, &big(), &q(&["x"]), 200, &p());
        assert_eq!(a.send.len(), 2);
        assert!(a.record.is_some());
    }

    fn rec(turn: u32, full: u64, filtered: u64, dropped: &[&str], refs: &[&str]) -> AbRecord {
        AbRecord {
            turn,
            tokens_full: full,
            tokens_filtered: filtered,
            dropped_ids: dropped.iter().map(|s| s.to_string()).collect(),
            referenced_ids: refs.iter().map(|s| s.to_string()).collect(),
            sticky: false,
        }
    }

    #[test]
    fn summary_mean_savings_is_exact() {
        let rs = [
            rec(1, 1000, 500, &["a"], &[]),
            rec(2, 1000, 900, &["b"], &[]),
            rec(3, 0, 0, &[], &[]),
        ];
        let s = AbSummary::from_records(&rs);
        assert_eq!(
            (s.records, s.tokens_full, s.tokens_filtered, s.tokens_saved),
            (3, 2000, 1400, 600)
        );
        assert!((s.mean_savings - 0.3).abs() < 1e-12);
    }

    #[test]
    fn regret_counts_dropped_chunks_referenced_later() {
        let rs = [
            rec(1, 100, 50, &["a", "b"], &[]),
            rec(2, 100, 50, &["c"], &["a"]), // a referenced at turn 2 >= 1: regret
            rec(3, 100, 50, &["a"], &["c", "z"]), // c referenced at 3 >= 2: regret; a dropped at 3, last ref 2 < 3: fine
        ];
        let s = AbSummary::from_records(&rs);
        assert_eq!(s.dropped, 4);
        assert_eq!(s.regretted, 2);
        assert!((s.regret_rate - 0.5).abs() < 1e-12);
        assert_eq!((s.records_with_drops, s.records_with_regret), (3, 2));
    }

    #[test]
    fn a_reference_before_the_drop_is_not_regret() {
        let rs = [rec(1, 100, 100, &[], &["a"]), rec(5, 100, 50, &["a"], &[])];
        assert_eq!(AbSummary::from_records(&rs).regretted, 0);
    }

    #[test]
    fn summary_is_zero_safe() {
        let s = AbSummary::from_records(&Vec::<AbRecord>::new());
        assert_eq!((s.records, s.dropped), (0, 0));
        assert_eq!(s.mean_savings, 0.0);
        assert_eq!(s.regret_rate, 0.0);
        assert!(!s.worth_enabling(0.0, 1.0));
        let n = AbSummary::from_records(&[rec(1, 100, 100, &[], &[])]);
        assert_eq!(n.regret_rate, 0.0);
    }

    #[test]
    fn worth_enabling_needs_savings_and_low_regret() {
        let good =
            AbSummary::from_records(&[rec(1, 100, 40, &["a"], &[]), rec(2, 100, 40, &["b"], &[])]);
        assert!(good.worth_enabling(0.3, 0.1));
        assert!(!good.worth_enabling(0.7, 0.1));
        let bad = AbSummary::from_records(&[rec(1, 100, 40, &["a"], &["a"])]);
        assert!(!bad.worth_enabling(0.3, 0.1));
    }

    #[test]
    fn ab_record_old_lines_parse_and_references_attach() {
        let r: AbRecord = serde_json::from_str(
            r#"{"turn":3,"tokens_full":10,"tokens_filtered":4,"dropped_ids":["a"]}"#,
        )
        .unwrap();
        assert!(r.referenced_ids.is_empty());
        assert_eq!(r.with_references(&["a"]).referenced_ids, vec!["a"]);
    }

    #[test]
    fn shadow_record_feeds_the_summary_end_to_end() {
        let cfg = FilterConfig {
            enabled: true,
            mode: Mode::Shadow,
        };
        let r = apply(&cfg, &big(), &q(&["x"]), 200, &p()).record.unwrap();
        let dropped = r.dropped_ids.clone();
        let r = r.with_references(&[dropped[0].as_str()]);
        let s = AbSummary::from_records(&[r]);
        assert_eq!((s.dropped, s.regretted), (3, 1));
    }

    fn corpus(rare_in: &str) -> Vec<Chunk> {
        let mut v: Vec<Chunk> = (0..4)
            .map(|i| {
                Chunk::new(&format!("c{i}"), Kind::ToolOutput, 10, 6)
                    .with_terms(&["error", &format!("filler{i}")])
            })
            .collect();
        v.push(Chunk::new(rare_in, Kind::ToolOutput, 10, 6).with_terms(&["lexer", "filler9"]));
        v
    }

    #[test]
    fn bm25_prefers_the_rare_query_term_over_the_common_one() {
        let cs = corpus("rare");
        let r = Bm25::default().rank(&cs, &q(&["error", "lexer"]));
        // each of c0..c3 matches "error" (in every chunk but one); "rare" matches "lexer" (in one chunk only)
        assert!(r[4] > r[0], "rare {} vs common {}", r[4], r[0]);
        assert!(r.iter().all(|x| (0.0..=1.0).contains(x)));
        // plain overlap cannot tell them apart: each chunk holds exactly one of the two query terms
        let o = OverlapRanker.rank(&cs, &q(&["error", "lexer"]));
        assert_eq!(o[0], o[4]);
        assert_eq!(Bm25::default().name(), "bm25");
    }

    #[test]
    fn bm25_idf_comes_from_the_request_corpus() {
        let alone = vec![
            Chunk::new("a", Kind::ToolOutput, 10, 6).with_terms(&["lexer", "x1"]),
            Chunk::new("b", Kind::ToolOutput, 10, 6).with_terms(&["other", "x2"]),
            Chunk::new("c", Kind::ToolOutput, 10, 6).with_terms(&["thing", "x3"]),
        ];
        let everywhere: Vec<Chunk> = ["a", "b", "c"]
            .iter()
            .map(|id| Chunk::new(id, Kind::ToolOutput, 10, 6).with_terms(&["lexer", "y"]))
            .collect();
        let qq = q(&["lexer", "zzz_absent"]);
        let rare = Bm25::default().rank(&alone, &qq)[0];
        let common = Bm25::default().rank(&everywhere, &qq)[0];
        // "zzz_absent" has df 0 in both corpora; "lexer" is the discriminating term only when rare
        assert!(rare > common, "{rare} vs {common}");
        assert_eq!(Bm25::default().rank(&[], &qq), Vec::<f32>::new());
        assert_eq!(Bm25::default().rank(&alone, &q(&[])), vec![0.0; 3]);
    }

    #[test]
    fn bm25_counts_term_frequency_and_normalises_length() {
        let cs = vec![
            Chunk::new("many", Kind::ToolOutput, 10, 6).with_text("lexer lexer lexer parser"),
            Chunk::new("one", Kind::ToolOutput, 10, 6).with_text("lexer parser"),
            Chunk::new("long", Kind::ToolOutput, 10, 6)
                .with_text("lexer alpha beta gamma delta epsilon zeta theta iota kappa"),
            Chunk::new("none", Kind::ToolOutput, 10, 6).with_text("css grid"),
        ];
        assert_eq!(cs[0].term_counts, vec![3, 1]);
        let r = Bm25::default().rank(&cs, &q(&["lexer"]));
        assert!(r[0] > r[1], "more occurrences score higher");
        assert!(r[1] > r[2], "the same hit in a long chunk scores lower");
        assert_eq!(r[3], 0.0);
        assert_eq!(r, Bm25::default().rank(&cs, &q(&["lexer"])), "deterministic");
    }

    #[test]
    fn select_ranks_with_bm25_by_default_and_overlap_on_request() {
        let mut cs = corpus("rare");
        for c in &mut cs {
            c.tokens = 40;
        }
        // budget fits 3 of 5: the rare-term chunk must survive under BM25, overlap has no way to prefer it
        let s = select(&cs, &q(&["error", "lexer"]), 120, &p());
        assert!(s.keep.contains(&"rare".to_string()), "{:?}", s.keep);
        let off = Params { use_bm25: false, ..p() };
        let a = select(&cs, &q(&["error", "lexer"]), 120, &off);
        let b = select_with(&cs, &q(&["error", "lexer"]), 120, &p(), &OverlapRanker);
        assert_eq!(a, b);
        assert!(Params::default().use_bm25);
        let parsed: Params = serde_json::from_value({
            let mut v = serde_json::to_value(Params::default()).unwrap();
            v.as_object_mut().unwrap().remove("use_bm25");
            v
        })
        .unwrap();
        assert!(parsed.use_bm25, "old params parse as bm25");
    }

    #[test]
    fn select_with_bm25_still_respects_pinned_newest_and_pairs() {
        let cs = vec![
            Chunk::new("sys", Kind::Other, 500, 50).pinned(),
            Chunk::new("use", Kind::AssistantMessage, 20, 8).paired("t1"),
            Chunk::new("res", Kind::ToolOutput, 30, 8)
                .with_text("parser failure")
                .paired("t1"),
            Chunk::new("new0", Kind::ToolOutput, 50, 0),
            Chunk::new("other", Kind::ToolOutput, 300, 9).with_text("parser parser"),
        ];
        for budget in [0u64, 100, 560, 600, 700, 900] {
            let s = select(&cs, &q(&["parser"]), budget, &p());
            for id in ["sys", "new0"] {
                assert!(s.keep.contains(&id.to_string()), "{id} at {budget}");
            }
            assert_eq!(
                s.keep.contains(&"use".to_string()),
                s.keep.contains(&"res".to_string()),
                "pair at {budget}"
            );
        }
    }

    fn five() -> Vec<Chunk> {
        (0..5)
            .map(|i| Chunk::new(&format!("c{i}"), Kind::ToolOutput, 500, 5 + i))
            .collect()
    }

    #[test]
    fn a_sticky_drop_stays_dropped_on_every_later_turn() {
        let (pol, mut led) = (DropPolicy::default(), DropLedger::new());
        let a = apply_sticky(&five(), &q(&["x"]), 1000, &p(), &pol, &mut led, 8);
        assert_eq!(a.send, vec!["c0", "c1"]);
        assert_eq!(led.len(), 3);
        assert_eq!(a.stubs.len(), 3);
        assert!(a.record.as_ref().unwrap().sticky);
        // later turns, a budget that would keep everything: the dropped ones do not come back
        for turn in [9u32, 10, 11, 12, 40] {
            let b = apply_sticky(&five(), &q(&["x"]), 1_000_000, &p(), &pol, &mut led, turn);
            assert_eq!(b.send, vec!["c0", "c1"], "turn {turn}");
            assert_eq!(
                b.record.as_ref().unwrap().dropped_ids,
                vec!["c2", "c3", "c4"]
            );
            assert_eq!(b.stubs, a.stubs, "the stubs are byte-identical every turn");
        }
    }

    #[test]
    fn no_new_drops_on_a_turn_that_is_not_a_multiple() {
        let (pol, mut led) = (DropPolicy::default(), DropLedger::new());
        for turn in [1u32, 2, 3, 5, 6, 7] {
            let a = apply_sticky(&five(), &q(&["x"]), 1000, &p(), &pol, &mut led, turn);
            assert_eq!(a.send.len(), 5, "turn {turn}");
            assert!(a.stubs.is_empty());
        }
        assert!(led.is_empty());
        let a = apply_sticky(&five(), &q(&["x"]), 1000, &p(), &pol, &mut led, 8);
        assert_eq!(a.send.len(), 2);
        assert_eq!(led.get("c4").unwrap().turn, 8);
    }

    #[test]
    fn small_chunks_are_never_dropped_and_pairs_and_newest_stay() {
        let pol = DropPolicy {
            every_n_turns: 1,
            ..DropPolicy::default()
        };
        let mut led = DropLedger::new();
        let small: Vec<Chunk> = (0..6)
            .map(|i| Chunk::new(&format!("s{i}"), Kind::ToolOutput, 399, 5 + i))
            .collect();
        let a = apply_sticky(&small, &q(&[]), 100, &p(), &pol, &mut led, 3);
        assert!(a.record.unwrap().dropped_ids.is_empty(), "under 400 tokens: kept");
        assert!(led.is_empty());
        let reason = &a.selection.unwrap().reasons[5].why;
        assert!(reason.contains("not worth a stub"), "{reason}");
        // a pair is judged as a whole: 20 + 500 drops together; newest (age < 2) and pinned never drop
        let cs = vec![
            Chunk::new("use", Kind::AssistantMessage, 20, 8).paired("t1"),
            Chunk::new("res", Kind::ToolOutput, 500, 8).paired("t1"),
            Chunk::new("fresh", Kind::ToolOutput, 900, 0),
            Chunk::new("sys", Kind::Other, 900, 30).pinned(),
        ];
        let b = apply_sticky(&cs, &q(&[]), 10, &p(), &pol, &mut led, 3);
        assert_eq!(b.send, vec!["fresh", "sys"]);
        assert!(led.is_dropped("use") && led.is_dropped("res"));
    }

    #[test]
    fn a_stub_says_which_chunk_how_big_and_how_to_get_it_back() {
        let t = stub_text("tool_77", 1234, 8);
        assert_eq!(
            t,
            "[context: tool output tool_77 (1234 tokens) set aside at turn 8; context_fetch id=tool_77 brings it back]"
        );
        let (pol, mut led) = (DropPolicy::default(), DropLedger::new());
        let a = apply_sticky(&five(), &q(&["x"]), 1000, &p(), &pol, &mut led, 8);
        assert!(a.stubs[0].text.contains(&a.stubs[0].id) && a.stubs[0].text.contains("500 tokens"));
        // stubs cost something: the record's filtered tokens include them
        assert!(a.record.unwrap().tokens_filtered > 1000);
        let silent = DropPolicy {
            stub: StubStyle::Silent,
            ..DropPolicy::default()
        };
        let mut led2 = DropLedger::new();
        let s = apply_sticky(&five(), &q(&["x"]), 1000, &p(), &silent, &mut led2, 8);
        assert!(s.stubs.is_empty());
        assert_eq!(s.send, vec!["c0", "c1"]);
    }

    #[test]
    fn the_drop_ledger_survives_serde_and_releases() {
        let mut led = DropLedger::new();
        led.record("b", 4, 700);
        led.record("a", 8, 900);
        led.record("a", 9, 1); // the first record wins: when it was dropped does not change
        let j = serde_json::to_string(&led).unwrap();
        assert_eq!(j, r#"{"dropped":{"a":{"turn":8,"tokens":900},"b":{"turn":4,"tokens":700}}}"#);
        let back: DropLedger = serde_json::from_str(&j).unwrap();
        assert_eq!(back, led);
        let mut back = back;
        assert!(back.release("a") && !back.release("a"));
        assert_eq!(back.ids().collect::<Vec<_>>(), vec!["b"]);
    }

    #[test]
    fn old_ab_records_without_sticky_parse() {
        let r: AbRecord = serde_json::from_str(
            r#"{"turn":3,"tokens_full":10,"tokens_filtered":4,"dropped_ids":["a"],"referenced_ids":[]}"#,
        )
        .unwrap();
        assert!(!r.sticky);
        let s: AbRecord = serde_json::from_str(
            r#"{"turn":3,"tokens_full":10,"tokens_filtered":4,"dropped_ids":[],"sticky":true}"#,
        )
        .unwrap();
        assert!(s.sticky);
    }
}
