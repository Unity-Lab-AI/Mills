//! What the engine works out about one graph (the top level or one sub-graph) before it runs anything.
//!
//! - **Loop bodies.** The body of a `loop` is every node reachable from its `again` port and not from its other ports
//!   (`done`, `limit`, `round`, `error`): those run once per round. A node reachable both ways comes after the loop
//!   and gets the last round's values.
//! - **Lines.** A `parallel` in `copies` or `split` mode makes n copies of the nodes between its `out` port and its
//!   `join` (config `join`, else the first `join` reached), one per line. Nodes after the join run once.
//! - Loop bodies and line regions are **regions**; they nest but must not overlap. Every node belongs to the innermost
//!   region that holds it (or to the graph itself), and runs in an instance of it (a round, a line).
use pgm_workflow_internal::kinds::KindRegistry;
use pgm_workflow_internal::types::{split_endpoint, PortSpec, PortType, Ports};
use pgm_workflow_internal::{Document, Node, PortDecl};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone)]
pub(crate) struct PEdge {
    pub id: String,
    pub src: usize,
    pub sport: String,
    pub dst: usize,
    pub dport: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RKind {
    Loop,
    Lines { join: usize },
}

#[derive(Debug, Clone)]
pub(crate) struct Region {
    pub owner: usize,
    pub kind: RKind,
    pub members: HashSet<usize>,
    pub parent: Option<usize>,
    /// Edges from inside to outside (not back to the owner): a loop's last round, a line's way into its join.
    pub exits: Vec<usize>,
    /// A loop's edges into its own `back` port.
    pub back: Vec<usize>,
}

#[derive(Debug)]
pub(crate) struct Plan {
    pub key: Option<String>,
    pub nodes: Vec<Node>,
    pub ports: Vec<Ports>,
    pub edges: Vec<PEdge>,
    pub ins: Vec<Vec<usize>>,
    pub outs: Vec<Vec<usize>>,
    pub regions: Vec<Region>,
    pub owned: HashMap<usize, usize>,
    pub innermost: Vec<Option<usize>>,
    pub direct: HashMap<Option<usize>, Vec<usize>>,
    /// The edges a node waits for (a loop: not its `back` edges).
    pub wait: Vec<Vec<usize>>,
    /// For a region's owner: the edges from outside into its region, which must have arrived before it starts.
    pub implicit: Vec<Vec<usize>>,
    /// For each join: the nodes whose every way forward leads into it (cancelled when it is satisfied early).
    pub feeds_only: HashMap<usize, Vec<usize>>,
}

fn decl_specs(ds: &[PortDecl], input: bool) -> Vec<PortSpec> {
    ds.iter().map(|d| {
        let mut p = PortSpec::new(&d.name, PortType::parse(&d.ty).unwrap_or(PortType::Json));
        if input && d.required.unwrap_or(false) && d.default.is_none() { p = p.required(); }
        p.default = d.default.clone();
        p
    }).collect()
}

/// A node's ports: the kind's (with what its config adds), or for a `subgraph` node those its sub-graph declares.
pub(crate) fn node_ports(doc: &Document, reg: &dyn KindRegistry, n: &Node) -> Option<Ports> {
    if n.kind != "subgraph" { return reg.ports(n); }
    let s = doc.subgraph(n.cfg_str("ref")?)?;
    let from_nodes = |kind: &str| -> Vec<PortDecl> {
        s.nodes.iter().filter(|x| x.kind == kind).map(|x| PortDecl { name: x.cfg_str("name").unwrap_or(&x.id).to_string(), ty: x.cfg_str("type").unwrap_or("json").to_string(), required: None, default: None, extra: Default::default() }).collect()
    };
    let ins = s.inputs.clone().unwrap_or_else(|| from_nodes("input"));
    let outs = s.outputs.clone().unwrap_or_else(|| from_nodes("output"));
    let mut outputs = decl_specs(&outs, false);
    outputs.push(PortSpec::new("error", PortType::Error));
    Some(Ports { inputs: decl_specs(&ins, true), outputs })
}

impl Plan {
    pub fn build(doc: &Document, key: Option<&str>, reg: &dyn KindRegistry) -> Result<Plan, Vec<(String, String)>> {
        let (nodes, edges) = match key {
            None => (doc.nodes.clone(), doc.edges.clone()),
            Some(k) => { let s = doc.subgraph(k).ok_or_else(|| vec![("subgraph.missing".to_string(), format!("no sub-graph `{k}`"))])?; (s.nodes.clone(), s.edges.clone()) }
        };
        let idx: HashMap<String, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id.clone(), i)).collect();
        let ports: Vec<Ports> = nodes.iter().map(|n| node_ports(doc, reg, n).unwrap_or_default()).collect();
        let mut pedges = vec![];
        for e in &edges {
            let (Some((a, ap)), Some((b, bp))) = (split_endpoint(&e.from), split_endpoint(&e.to)) else { continue };
            let (Some(&s), Some(&d)) = (idx.get(a), idx.get(b)) else { continue };
            pedges.push(PEdge { id: e.id.clone(), src: s, sport: ap.to_string(), dst: d, dport: bp.to_string() });
        }
        let n = nodes.len();
        let mut ins = vec![vec![]; n];
        let mut outs = vec![vec![]; n];
        for (i, e) in pedges.iter().enumerate() { ins[e.dst].push(i); outs[e.src].push(i); }
        let mut plan = Plan {
            key: key.map(str::to_string), nodes, ports, edges: pedges, ins, outs, regions: vec![], owned: HashMap::new(), innermost: vec![None; n],
            direct: HashMap::new(), wait: vec![vec![]; n], implicit: vec![vec![]; n], feeds_only: HashMap::new(),
        };
        let mut problems = vec![];
        plan.regions(&mut problems);
        if !problems.is_empty() { return Err(problems); }
        Ok(plan)
    }

    /// Every node reachable from `from` (included), never entering a node `stop` says no to.
    fn fwd(&self, from: impl IntoIterator<Item = usize>, stop: &dyn Fn(usize) -> bool) -> HashSet<usize> {
        let mut seen = HashSet::new();
        let mut todo: VecDeque<usize> = VecDeque::new();
        for s in from { if !stop(s) && seen.insert(s) { todo.push_back(s); } }
        while let Some(x) = todo.pop_front() {
            for &e in &self.outs[x] { let y = self.edges[e].dst; if !stop(y) && seen.insert(y) { todo.push_back(y); } }
        }
        seen
    }

    fn bwd(&self, from: impl IntoIterator<Item = usize>, stop: &dyn Fn(usize) -> bool) -> HashSet<usize> {
        let mut seen = HashSet::new();
        let mut todo: VecDeque<usize> = VecDeque::new();
        for s in from { if !stop(s) && seen.insert(s) { todo.push_back(s); } }
        while let Some(x) = todo.pop_front() {
            for &e in &self.ins[x] { let y = self.edges[e].src; if !stop(y) && seen.insert(y) { todo.push_back(y); } }
        }
        seen
    }

    fn port_dsts(&self, n: usize, keep: &dyn Fn(&str) -> bool) -> Vec<usize> {
        self.outs[n].iter().filter(|&&e| keep(&self.edges[e].sport)).map(|&e| self.edges[e].dst).collect()
    }

    fn regions(&mut self, problems: &mut Vec<(String, String)>) {
        let n = self.nodes.len();
        for o in 0..n {
            let node = &self.nodes[o];
            let kind = match node.kind.as_str() {
                "loop" => {
                    let body = self.fwd(self.port_dsts(o, &|p| p == "again"), &|x| x == o);
                    let after = self.fwd(self.port_dsts(o, &|p| p != "again"), &|x| x == o);
                    let members: HashSet<usize> = body.difference(&after).copied().collect();
                    self.regions.push(Region { owner: o, kind: RKind::Loop, members, parent: None, exits: vec![], back: vec![] });
                    continue;
                }
                "parallel" if node.cfg_str("mode").unwrap_or("copies") != "fixed" => {
                    let starts = self.port_dsts(o, &|p| p == "out");
                    let join = match node.cfg_str("join") {
                        Some(j) => self.nodes.iter().position(|x| x.id == j && x.kind == "join"),
                        None => {
                            // the first join reached, breadth first
                            let mut seen = HashSet::new();
                            let mut todo: VecDeque<usize> = starts.iter().copied().collect();
                            let mut found = None;
                            while let Some(x) = todo.pop_front() {
                                if x == o || !seen.insert(x) { continue; }
                                if self.nodes[x].kind == "join" { found = Some(x); break; }
                                for &e in &self.outs[x] { todo.push_back(self.edges[e].dst); }
                            }
                            found
                        }
                    };
                    let Some(j) = join else {
                        problems.push(("engine.parallel-join".into(), format!("parallel `{}` (mode {}) needs a `join` after its lines (or `join` in its config naming one)", node.id, node.cfg_str("mode").unwrap_or("copies"))));
                        continue;
                    };
                    RKind::Lines { join: j }
                }
                _ => continue,
            };
            let RKind::Lines { join } = kind else { continue };
            let members = self.fwd(self.port_dsts(o, &|p| p == "out"), &|x| x == o || x == join);
            self.regions.push(Region { owner: o, kind, members, parent: None, exits: vec![], back: vec![] });
        }
        // nesting: a region's parent is the smallest other region holding it and its owner; overlap is refused
        let r = self.regions.len();
        for a in 0..r {
            let mut best: Option<usize> = None;
            for b in 0..r {
                if a == b { continue; }
                let (ra, rb) = (&self.regions[a], &self.regions[b]);
                if ra.members.is_disjoint(&rb.members) && !rb.members.contains(&ra.owner) && !ra.members.contains(&rb.owner) { continue; }
                let a_in_b = ra.members.is_subset(&rb.members) && rb.members.contains(&ra.owner);
                let b_in_a = rb.members.is_subset(&ra.members) && ra.members.contains(&rb.owner);
                if a_in_b {
                    if best.is_none_or(|x| self.regions[x].members.len() > rb.members.len()) { best = Some(b); }
                } else if !b_in_a && a < b {
                    problems.push(("engine.region-overlap".into(), format!("`{}` and `{}` overlap: a loop body or a set of parallel lines must hold another one whole, or not at all", self.nodes[ra.owner].id, self.nodes[rb.owner].id)));
                }
            }
            self.regions[a].parent = best;
        }
        for (i, reg) in self.regions.iter().enumerate() { self.owned.insert(reg.owner, i); }
        for x in 0..n {
            self.innermost[x] = (0..r).filter(|&i| self.regions[i].members.contains(&x)).min_by_key(|&i| self.regions[i].members.len());
        }
        for x in 0..n { self.direct.entry(self.innermost[x]).or_default().push(x); }
        for i in 0..r {
            let (owner, members) = (self.regions[i].owner, self.regions[i].members.clone());
            let mut exits = vec![];
            let mut back = vec![];
            for (ei, e) in self.edges.iter().enumerate() {
                let src_in = members.contains(&e.src) || e.src == owner;
                if e.dst == owner && self.regions[i].kind == RKind::Loop && e.dport == "back" && src_in { back.push(ei); continue; }
                if members.contains(&e.src) && !members.contains(&e.dst) && e.dst != owner { exits.push(ei); }
                if members.contains(&e.dst) && !members.contains(&e.src) && e.src != owner { self.implicit[owner].push(ei); }
            }
            if let RKind::Lines { join } = self.regions[i].kind {
                for &ei in &exits {
                    if self.edges[ei].dst != join {
                        problems.push(("engine.parallel-escape".into(), format!("edge `{}` leaves the lines of parallel `{}` other than through its join `{}`", self.edges[ei].id, self.nodes[owner].id, self.nodes[join].id)));
                    }
                }
            }
            self.regions[i].exits = exits;
            self.regions[i].back = back;
        }
        for x in 0..n {
            self.wait[x] = self.ins[x].iter().copied().filter(|&e| !(self.nodes[x].kind == "loop" && self.edges[e].dport == "back")).collect();
        }
        for j in 0..n {
            if self.nodes[j].kind != "join" { continue; }
            let srcs: Vec<usize> = self.ins[j].iter().map(|&e| self.edges[e].src).collect();
            let anc = self.bwd(srcs, &|x| x == j);
            let only: Vec<usize> = anc.iter().copied().filter(|&x| self.fwd([x], &|y| y == j).is_subset(&anc)).collect();
            self.feeds_only.insert(j, only);
        }
    }

    pub fn within(&self, n: usize, r: Option<usize>) -> bool {
        match r { None => true, Some(r) => self.regions[r].members.contains(&n) }
    }

    /// The outermost region inside `r` that holds `n` (None: `n` is one of `r`'s own nodes).
    pub fn child_region_containing(&self, r: Option<usize>, n: usize) -> Option<usize> {
        let mut c = self.innermost[n];
        let mut last = None;
        while c != r {
            let Some(ci) = c else { return last };
            last = Some(ci);
            c = self.regions[ci].parent;
        }
        last
    }

    pub fn has_edge_from(&self, n: usize, port: &str) -> bool { self.outs[n].iter().any(|&e| self.edges[e].sport == port) }
}
