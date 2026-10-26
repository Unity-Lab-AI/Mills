//! Templates and conditions: a tiny language of our own, no dependency, and **no code runs in it** (spec 4.3, 5.1).
//!
//! **Templates** are text with `{{ ... }}` holes:
//! - `{{inputs.audience}}`, `{{node.n2.out.summary}}`, `{{state.x}}`, `{{items[0].name}}`: a value read by a path
//! - `{{x|default:"team"}}`: the text to use when the value is missing (a missing value without one is an error)
//! - `{{prompt:release-style}}`: a saved prompt (spec 6) by name or id; the engine fills it in, the library only names it
//! - `\{{` is a literal `{{`
//!
//! **Conditions** are one-line expressions: numbers, `"text"` / `'text'`, `true` `false` `null`, paths, `[a, b]` lists;
//! `== != < <= > >=`, `contains`, `matches` (a small regex, see [`regex_search`]), `in`, `and` / `or` / `not`
//! (also `&&` `||` `!`), `+ - * / %`, and the functions `len lower upper trim abs`.
use serde_json::Value;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct ExprError(pub String);
impl fmt::Display for ExprError { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) } }
impl std::error::Error for ExprError {}
fn err<T>(m: impl Into<String>) -> Result<T, ExprError> { Err(ExprError(m.into())) }

/// Looks a path up (`["node","n2","out","summary"]`); None when there is no such value.
pub type Lookup<'a> = &'a dyn Fn(&[String]) -> Option<Value>;

// ---------------------------------------------------------------- templates

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Text(String),
    Value { path: Vec<String>, default: Option<String>, src: String },
    Prompt(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Template { pub parts: Vec<Part> }

pub fn parse_template(s: &str) -> Result<Template, ExprError> {
    let mut parts = vec![];
    let mut text = String::new();
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("\\{{") { text.push_str("{{"); rest = r; continue; }
        if let Some(r) = rest.strip_prefix("{{") {
            let end = r.find("}}").ok_or_else(|| ExprError("a `{{` is never closed with `}}`".into()))?;
            if !text.is_empty() { parts.push(Part::Text(std::mem::take(&mut text))); }
            parts.push(parse_hole(r[..end].trim())?);
            rest = &r[end + 2..];
            continue;
        }
        let c = rest.chars().next().unwrap();
        text.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if !text.is_empty() { parts.push(Part::Text(text)); }
    Ok(Template { parts })
}

fn parse_hole(inner: &str) -> Result<Part, ExprError> {
    if inner.is_empty() { return err("an empty `{{ }}`"); }
    if let Some(name) = inner.strip_prefix("prompt:") {
        let name = name.trim();
        if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c == '|' || c == '{' || c == '}') { return err(format!("`{{{{{inner}}}}}` needs a prompt name or id after `prompt:`")); }
        return Ok(Part::Prompt(name.to_string()));
    }
    let (path_src, filter) = match inner.split_once('|') { Some((p, f)) => (p.trim(), Some(f.trim())), None => (inner, None) };
    let path = parse_path(path_src)?;
    let default = match filter {
        None => None,
        Some(f) => {
            let q = f.strip_prefix("default:").map(str::trim).ok_or_else(|| ExprError(format!("unknown filter `{f}` in `{{{{{inner}}}}}` (only `default:\"...\"` exists)")))?;
            let b = q.as_bytes();
            if q.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[q.len() - 1] == b[0] { Some(q[1..q.len() - 1].to_string()) } else { return err(format!("the default in `{{{{{inner}}}}}` must be quoted")); }
        }
    };
    Ok(Part::Value { path, default, src: inner.to_string() })
}

/// `a.b[0].c-d` -> `["a","b","0","c-d"]`.
pub fn parse_path(s: &str) -> Result<Vec<String>, ExprError> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let flush = |cur: &mut String, out: &mut Vec<String>| -> Result<(), ExprError> { if cur.is_empty() { err(format!("a name is missing in `{s}`")) } else { out.push(std::mem::take(cur)); Ok(()) } };
    while let Some(c) = chars.next() {
        match c {
            '.' => flush(&mut cur, &mut out)?,
            '[' => {
                if !cur.is_empty() { out.push(std::mem::take(&mut cur)); }
                let mut idx = String::new();
                loop { match chars.next() { Some(']') => break, Some(c) => idx.push(c), None => return err(format!("a `[` is never closed in `{s}`")) } }
                let idx = idx.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
                if idx.is_empty() { return err(format!("an empty `[]` in `{s}`")); }
                out.push(idx);
                if chars.peek() == Some(&'.') { chars.next(); if chars.peek().is_none() { return err(format!("`{s}` ends with a dot")); } }
            }
            c if c.is_alphanumeric() || c == '_' || c == '-' || c == ':' => cur.push(c),
            c => return err(format!("`{c}` cannot be in a name (in `{s}`)")),
        }
    }
    if !cur.is_empty() { out.push(cur); } else if out.is_empty() || s.ends_with('.') { return err(format!("a name is missing in `{s}`")); }
    Ok(out)
}

impl Template {
    /// Every value path the template reads.
    pub fn paths(&self) -> Vec<&Vec<String>> { self.parts.iter().filter_map(|p| if let Part::Value { path, .. } = p { Some(path) } else { None }).collect() }
    /// Every saved prompt it names.
    pub fn prompt_refs(&self) -> Vec<&str> { self.parts.iter().filter_map(|p| if let Part::Prompt(n) = p { Some(n.as_str()) } else { None }).collect() }
    /// The text with every hole filled. A missing value without a default is an error; a `{{prompt:..}}` is left to
    /// `prompts` (None: it is an error to meet one).
    pub fn render(&self, lookup: Lookup, prompts: Option<&dyn Fn(&str) -> Option<String>>) -> Result<String, ExprError> {
        let mut out = String::new();
        for p in &self.parts {
            match p {
                Part::Text(t) => out.push_str(t),
                Part::Value { path, default, src } => match lookup(path) {
                    Some(Value::String(s)) => out.push_str(&s),
                    Some(Value::Null) => {}
                    Some(v) => out.push_str(&v.to_string()),
                    None => match default { Some(d) => out.push_str(d), None => return err(format!("no value for `{{{{{src}}}}}`")) },
                },
                Part::Prompt(n) => match prompts.and_then(|f| f(n)) { Some(t) => out.push_str(&t), None => return err(format!("no saved prompt `{n}`")) },
            }
        }
        Ok(out)
    }
}

/// Render in one go.
pub fn render(template: &str, lookup: Lookup) -> Result<String, ExprError> { parse_template(template)?.render(lookup, None) }

// ---------------------------------------------------------------- conditions

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op { Or, And, Eq, Ne, Lt, Le, Gt, Ge, Contains, Matches, In, Add, Sub, Mul, Div, Rem }

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Value),
    Path(Vec<String>),
    List(Vec<Expr>),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok { Num(f64), Str(String), Id(String), Sym(&'static str), Open, Close }

fn lex(s: &str) -> Result<Vec<Tok>, ExprError> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() { i += 1; continue; }
        if ch.is_ascii_digit() || (ch == '.' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let st = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') { i += 1; }
            let t: String = c[st..i].iter().collect();
            out.push(Tok::Num(t.parse().map_err(|_| ExprError(format!("`{t}` is not a number")))?));
            continue;
        }
        if ch.is_alphabetic() || ch == '_' {
            let st = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') { i += 1; }
            out.push(Tok::Id(c[st..i].iter().collect()));
            continue;
        }
        if ch == '"' || ch == '\'' {
            let mut t = String::new();
            i += 1;
            loop {
                match c.get(i) {
                    None => return err("a quote is never closed"),
                    Some('\\') if i + 1 < c.len() => { t.push(match c[i + 1] { 'n' => '\n', 't' => '\t', o => o }); i += 2; }
                    Some(&q) if q == ch => { i += 1; break; }
                    Some(&o) => { t.push(o); i += 1; }
                }
            }
            out.push(Tok::Str(t));
            continue;
        }
        let two: String = c[i..(i + 2).min(c.len())].iter().collect();
        let sym2 = match two.as_str() { "==" => Some("=="), "!=" => Some("!="), "<=" => Some("<="), ">=" => Some(">="), "&&" => Some("and"), "||" => Some("or"), _ => None };
        if let Some(s2) = sym2 { out.push(Tok::Sym(s2)); i += 2; continue; }
        let sym1 = match ch { '<' => "<", '>' => ">", '+' => "+", '-' => "-", '*' => "*", '/' => "/", '%' => "%", ',' => ",", '!' => "not", '[' => "[", ']' => "]", '.' => ".", _ => "" };
        if !sym1.is_empty() { out.push(Tok::Sym(sym1)); i += 1; continue; }
        if ch == '(' { out.push(Tok::Open); i += 1; continue; }
        if ch == ')' { out.push(Tok::Close); i += 1; continue; }
        if ch == '=' { return err("use `==` to compare (there is no assignment)"); }
        return err(format!("`{ch}` is not allowed here"));
    }
    Ok(out)
}

struct P { t: Vec<Tok>, i: usize }
impl P {
    fn peek(&self) -> Option<&Tok> { self.t.get(self.i) }
    fn kw(&self, w: &str) -> bool { matches!(self.peek(), Some(Tok::Id(x)) if x == w) || matches!(self.peek(), Some(Tok::Sym(x)) if *x == w) }
    fn eat_kw(&mut self, w: &str) -> bool { if self.kw(w) { self.i += 1; true } else { false } }
    fn or(&mut self) -> Result<Expr, ExprError> {
        let mut l = self.and()?;
        while self.eat_kw("or") { let r = self.and()?; l = Expr::Bin(Op::Or, Box::new(l), Box::new(r)); }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr, ExprError> {
        let mut l = self.not()?;
        while self.eat_kw("and") { let r = self.not()?; l = Expr::Bin(Op::And, Box::new(l), Box::new(r)); }
        Ok(l)
    }
    fn not(&mut self) -> Result<Expr, ExprError> { if self.eat_kw("not") { Ok(Expr::Not(Box::new(self.not()?))) } else { self.cmp() } }
    fn cmp(&mut self) -> Result<Expr, ExprError> {
        let mut l = self.add()?;
        loop {
            let op = if self.eat_kw("==") { Op::Eq } else if self.eat_kw("!=") { Op::Ne } else if self.eat_kw("<=") { Op::Le } else if self.eat_kw(">=") { Op::Ge }
                else if self.eat_kw("<") { Op::Lt } else if self.eat_kw(">") { Op::Gt } else if self.eat_kw("contains") { Op::Contains }
                else if self.eat_kw("matches") { Op::Matches } else if self.eat_kw("in") { Op::In } else { return Ok(l) };
            let r = self.add()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn add(&mut self) -> Result<Expr, ExprError> {
        let mut l = self.mul()?;
        loop {
            let op = if self.eat_kw("+") { Op::Add } else if self.eat_kw("-") { Op::Sub } else { return Ok(l) };
            let r = self.mul()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn mul(&mut self) -> Result<Expr, ExprError> {
        let mut l = self.unary()?;
        loop {
            let op = if self.eat_kw("*") { Op::Mul } else if self.eat_kw("/") { Op::Div } else if self.eat_kw("%") { Op::Rem } else { return Ok(l) };
            let r = self.unary()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn unary(&mut self) -> Result<Expr, ExprError> { if self.eat_kw("-") { Ok(Expr::Neg(Box::new(self.unary()?))) } else { self.primary() } }
    fn primary(&mut self) -> Result<Expr, ExprError> {
        let t = self.t.get(self.i).cloned().ok_or_else(|| ExprError("the expression ends too soon".into()))?;
        self.i += 1;
        match t {
            Tok::Num(n) => Ok(Expr::Lit(num(n)?)),
            Tok::Str(s) => Ok(Expr::Lit(Value::String(s))),
            Tok::Open => { let e = self.or()?; if self.peek() != Some(&Tok::Close) { return err("a `(` is never closed"); } self.i += 1; Ok(e) }
            Tok::Sym("[") => {
                let mut items = vec![];
                if matches!(self.peek(), Some(Tok::Sym("]"))) { self.i += 1; return Ok(Expr::List(items)); }
                loop {
                    items.push(self.or()?);
                    if self.eat_kw(",") { continue; }
                    if matches!(self.peek(), Some(Tok::Sym("]"))) { self.i += 1; return Ok(Expr::List(items)); }
                    return err("a list needs `,` or `]`");
                }
            }
            Tok::Id(w) => match w.as_str() {
                "true" => Ok(Expr::Lit(Value::Bool(true))),
                "false" => Ok(Expr::Lit(Value::Bool(false))),
                "null" => Ok(Expr::Lit(Value::Null)),
                "and" | "or" | "not" | "contains" | "matches" | "in" => err(format!("`{w}` is an operator and needs a value on each side")),
                _ if self.peek() == Some(&Tok::Open) => {
                    if !["len", "lower", "upper", "trim", "abs"].contains(&w.as_str()) { return err(format!("`{w}(...)` is not a function (there are len, lower, upper, trim, abs)")); }
                    self.i += 1;
                    let mut args = vec![];
                    if self.peek() == Some(&Tok::Close) { self.i += 1; } else {
                        loop {
                            args.push(self.or()?);
                            if self.eat_kw(",") { continue; }
                            if self.peek() == Some(&Tok::Close) { self.i += 1; break; }
                            return err(format!("`{w}(` is never closed"));
                        }
                    }
                    if args.len() != 1 { return err(format!("`{w}` takes one value")); }
                    Ok(Expr::Call(w, args))
                }
                _ => {
                    let mut path = vec![w];
                    loop {
                        if matches!(self.peek(), Some(Tok::Sym("."))) {
                            self.i += 1;
                            match self.t.get(self.i).cloned() {
                                Some(Tok::Id(n)) => { path.push(n); self.i += 1; }
                                Some(Tok::Num(n)) if n.fract() == 0.0 => { path.push(format!("{}", n as i64)); self.i += 1; }
                                _ => return err("a name is missing after `.`"),
                            }
                        } else if matches!(self.peek(), Some(Tok::Sym("["))) {
                            self.i += 1;
                            match self.t.get(self.i).cloned() {
                                Some(Tok::Str(s)) => path.push(s),
                                Some(Tok::Num(n)) if n.fract() == 0.0 => path.push(format!("{}", n as i64)),
                                _ => return err("`[` needs a number or a quoted name"),
                            }
                            self.i += 1;
                            if !matches!(self.peek(), Some(Tok::Sym("]"))) { return err("a `[` is never closed"); }
                            self.i += 1;
                        } else { break; }
                    }
                    Ok(Expr::Path(path))
                }
            },
            Tok::Sym(s) => err(format!("`{s}` is not allowed here")),
            Tok::Close => err("a `)` has no `(`"),
        }
    }
}

fn num(n: f64) -> Result<Value, ExprError> {
    if n.fract() == 0.0 && n.abs() < 9.0e15 { Ok(Value::from(n as i64)) } else { serde_json::Number::from_f64(n).map(Value::Number).ok_or_else(|| ExprError("that number is not finite".into())) }
}

/// Parse a condition (syntax only: nothing is evaluated, no value is read).
pub fn parse_condition(s: &str) -> Result<Expr, ExprError> {
    if s.trim().is_empty() { return err("the condition is empty"); }
    let mut p = P { t: lex(s)?, i: 0 };
    let e = p.or()?;
    if p.i < p.t.len() { return err(format!("unexpected {} after the end of the expression", match &p.t[p.i] { Tok::Id(x) => format!("`{x}`"), Tok::Sym(x) => format!("`{x}`"), Tok::Num(n) => n.to_string(), Tok::Str(x) => format!("\"{x}\""), Tok::Open => "`(`".into(), Tok::Close => "`)`".into() })); }
    Ok(e)
}

pub fn truthy(v: &Value) -> bool {
    match v { Value::Null => false, Value::Bool(b) => *b, Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0, Value::String(s) => !s.is_empty(), Value::Array(a) => !a.is_empty(), Value::Object(o) => !o.is_empty() }
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) { (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(), _ => a == b }
}

impl Expr {
    /// Every path the condition reads.
    pub fn paths(&self) -> Vec<&Vec<String>> {
        let mut out = vec![];
        self.collect(&mut out);
        out
    }
    fn collect<'a>(&'a self, out: &mut Vec<&'a Vec<String>>) {
        match self {
            Expr::Path(p) => out.push(p),
            Expr::List(v) => v.iter().for_each(|e| e.collect(out)),
            Expr::Not(e) | Expr::Neg(e) => e.collect(out),
            Expr::Bin(_, a, b) => { a.collect(out); b.collect(out); }
            Expr::Call(_, v) => v.iter().for_each(|e| e.collect(out)),
            Expr::Lit(_) => {}
        }
    }

    pub fn eval(&self, lookup: Lookup) -> Result<Value, ExprError> {
        Ok(match self {
            Expr::Lit(v) => v.clone(),
            Expr::Path(p) => lookup(p).ok_or_else(|| ExprError(format!("no value for `{}`", p.join("."))))?,
            Expr::List(v) => Value::Array(v.iter().map(|e| e.eval(lookup)).collect::<Result<_, _>>()?),
            Expr::Not(e) => Value::Bool(!truthy(&e.eval(lookup)?)),
            Expr::Neg(e) => num(-as_num(&e.eval(lookup)?)?)?,
            Expr::Call(f, a) => call(f, a[0].eval(lookup)?)?,
            Expr::Bin(Op::Or, a, b) => Value::Bool(truthy(&a.eval(lookup)?) || truthy(&b.eval(lookup)?)),
            Expr::Bin(Op::And, a, b) => Value::Bool(truthy(&a.eval(lookup)?) && truthy(&b.eval(lookup)?)),
            Expr::Bin(op, a, b) => bin(*op, a.eval(lookup)?, b.eval(lookup)?)?,
        })
    }
    /// The condition's answer: its value, as true or false.
    pub fn test(&self, lookup: Lookup) -> Result<bool, ExprError> { Ok(truthy(&self.eval(lookup)?)) }
}

fn as_num(v: &Value) -> Result<f64, ExprError> { v.as_f64().ok_or_else(|| ExprError(format!("{v} is not a number"))) }

fn call(f: &str, v: Value) -> Result<Value, ExprError> {
    Ok(match (f, &v) {
        ("len", Value::String(s)) => Value::from(s.chars().count()),
        ("len", Value::Array(a)) => Value::from(a.len()),
        ("len", Value::Object(o)) => Value::from(o.len()),
        ("lower", Value::String(s)) => Value::String(s.to_lowercase()),
        ("upper", Value::String(s)) => Value::String(s.to_uppercase()),
        ("trim", Value::String(s)) => Value::String(s.trim().to_string()),
        ("abs", Value::Number(n)) => num(n.as_f64().unwrap_or(0.0).abs())?,
        _ => return err(format!("{f}({v}) does not work on that kind of value")),
    })
}

fn bin(op: Op, a: Value, b: Value) -> Result<Value, ExprError> {
    use std::cmp::Ordering;
    let ord = |a: &Value, b: &Value| -> Result<Ordering, ExprError> {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.as_f64().partial_cmp(&y.as_f64()).ok_or_else(|| ExprError("cannot compare those numbers".into())),
            (Value::String(x), Value::String(y)) => Ok(x.cmp(y)),
            _ => err(format!("cannot compare {a} with {b}")),
        }
    };
    Ok(match op {
        Op::Eq => Value::Bool(same(&a, &b)),
        Op::Ne => Value::Bool(!same(&a, &b)),
        Op::Lt => Value::Bool(ord(&a, &b)? == Ordering::Less),
        Op::Le => Value::Bool(ord(&a, &b)? != Ordering::Greater),
        Op::Gt => Value::Bool(ord(&a, &b)? == Ordering::Greater),
        Op::Ge => Value::Bool(ord(&a, &b)? != Ordering::Less),
        Op::Contains => Value::Bool(contains(&a, &b)?),
        Op::In => Value::Bool(contains(&b, &a)?),
        Op::Matches => match (&a, &b) { (Value::String(t), Value::String(p)) => Value::Bool(regex_search(p, t)?), _ => return err(format!("`matches` needs text on both sides, not {a} and {b}")) },
        Op::Add => match (&a, &b) { (Value::String(x), Value::String(y)) => Value::String(format!("{x}{y}")), _ => num(as_num(&a)? + as_num(&b)?)? },
        Op::Sub => num(as_num(&a)? - as_num(&b)?)?,
        Op::Mul => num(as_num(&a)? * as_num(&b)?)?,
        Op::Div | Op::Rem => {
            let d = as_num(&b)?;
            if d == 0.0 { return err("division by zero"); }
            num(if op == Op::Div { as_num(&a)? / d } else { as_num(&a)? % d })?
        }
        Op::Or | Op::And => unreachable!(),
    })
}

fn contains(hay: &Value, needle: &Value) -> Result<bool, ExprError> {
    Ok(match (hay, needle) {
        (Value::String(h), Value::String(n)) => h.contains(n.as_str()),
        (Value::Array(h), n) => h.iter().any(|x| same(x, n)),
        (Value::Object(h), Value::String(n)) => h.contains_key(n),
        _ => return err(format!("{hay} cannot contain {needle}")),
    })
}

// ---------------------------------------------------------------- a small regex

#[derive(Debug, Clone)]
enum Atom { Any, Char(char), Class(bool, Vec<(char, char)>), Start, End }
#[derive(Debug, Clone)]
struct Item { atom: Atom, min: usize, max: usize }

fn class_escape(c: char) -> Option<(bool, Vec<(char, char)>)> {
    let digits = vec![('0', '9')];
    let word = vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')];
    let space = vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')];
    Some(match c { 'd' => (false, digits), 'D' => (true, digits), 'w' => (false, word), 'W' => (true, word), 's' => (false, space), 'S' => (true, space), _ => return None })
}

fn compile_alt(p: &[char]) -> Result<Vec<Item>, ExprError> {
    let mut items: Vec<Item> = vec![];
    let mut i = 0;
    while i < p.len() {
        let atom = match p[i] {
            '^' => Atom::Start,
            '$' => Atom::End,
            '.' => Atom::Any,
            '(' | ')' => return err("a regex here has no groups `( )`; use `|` between whole alternatives"),
            '*' | '+' | '?' => {
                let last = items.last_mut().ok_or_else(|| ExprError(format!("`{}` has nothing before it in the regex", p[i])))?;
                if matches!(last.atom, Atom::Start | Atom::End) || last.min != 1 || last.max != 1 { return err(format!("`{}` cannot follow that in the regex", p[i])); }
                match p[i] { '*' => { last.min = 0; last.max = usize::MAX; } '+' => { last.max = usize::MAX; } _ => { last.min = 0; } }
                i += 1;
                continue;
            }
            '\\' => {
                i += 1;
                let c = *p.get(i).ok_or_else(|| ExprError("a regex cannot end with `\\`".into()))?;
                match class_escape(c) { Some((neg, r)) => Atom::Class(neg, r), None => Atom::Char(match c { 'n' => '\n', 't' => '\t', o => o }) }
            }
            '[' => {
                i += 1;
                let neg = p.get(i) == Some(&'^');
                if neg { i += 1; }
                let mut ranges = vec![];
                loop {
                    let c = *p.get(i).ok_or_else(|| ExprError("a `[` is never closed in the regex".into()))?;
                    if c == ']' && !ranges.is_empty() { break; }
                    let c = if c == '\\' { i += 1; let e = *p.get(i).ok_or_else(|| ExprError("a regex cannot end with `\\`".into()))?; if let Some((n, r)) = class_escape(e) { if n { return err("`\\D \\W \\S` cannot be used inside `[ ]`"); } ranges.extend(r); i += 1; continue; } e } else { c };
                    if p.get(i + 1) == Some(&'-') && p.get(i + 2).is_some_and(|x| *x != ']') { ranges.push((c, p[i + 2])); i += 3; } else { ranges.push((c, c)); i += 1; }
                }
                Atom::Class(neg, ranges)
            }
            c => Atom::Char(c),
        };
        items.push(Item { atom, min: 1, max: 1 });
        i += 1;
    }
    Ok(items)
}

fn atom_ok(a: &Atom, c: char) -> bool {
    match a { Atom::Any => c != '\n', Atom::Char(x) => *x == c, Atom::Class(neg, r) => r.iter().any(|(lo, hi)| *lo <= c && c <= *hi) != *neg, _ => false }
}

fn here(items: &[Item], t: &[char], pos: usize) -> bool {
    let Some(it) = items.first() else { return true };
    match it.atom {
        Atom::Start => pos == 0 && here(&items[1..], t, pos),
        Atom::End => pos == t.len() && here(&items[1..], t, pos),
        _ => {
            let mut n = 0;
            while n < it.max && pos + n < t.len() && atom_ok(&it.atom, t[pos + n]) { n += 1; }
            loop {
                if n >= it.min && here(&items[1..], t, pos + n) { return true; }
                if n == 0 || n <= it.min { return false; }
                n -= 1;
            }
        }
    }
}

/// Does the text contain a match for the pattern? The subset: literals, `.`, `[a-z]` / `[^..]`, `\d \w \s` (and
/// `\D \W \S`), `^` `$`, `* + ?` after one of those, and `|` between whole alternatives. No groups, no backtracking
/// blow-up (the pattern is small and the text is searched once per start).
pub fn regex_search(pattern: &str, text: &str) -> Result<bool, ExprError> {
    let p: Vec<char> = pattern.chars().collect();
    let mut alts: Vec<Vec<char>> = vec![vec![]];
    let (mut i, mut in_class) = (0, false);
    while i < p.len() {
        match p[i] {
            '\\' if i + 1 < p.len() => { alts.last_mut().unwrap().extend([p[i], p[i + 1]]); i += 2; continue; }
            '[' => in_class = true,
            ']' => in_class = false,
            '|' if !in_class => { alts.push(vec![]); i += 1; continue; }
            _ => {}
        }
        alts.last_mut().unwrap().push(p[i]);
        i += 1;
    }
    let compiled: Vec<Vec<Item>> = alts.iter().map(|a| compile_alt(a)).collect::<Result<_, _>>()?;
    let t: Vec<char> = text.chars().collect();
    Ok(compiled.iter().any(|items| (0..=t.len()).any(|s| here(items, &t, s))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(p: &[String]) -> Option<Value> {
        let root = json!({ "inputs": { "audience": "team", "n": 7, "tags": ["a", "b"] }, "node": { "n2": { "out": { "summary": "short" } } }, "state": { "round": 2 } });
        p.iter().try_fold(&root, |v, k| match v { Value::Array(a) => k.parse::<usize>().ok().and_then(|i| a.get(i)), o => o.get(k) }).cloned()
    }

    #[test]
    fn templates() {
        let t = parse_template("Audience: {{inputs.audience}}\n{{ node.n2.out.summary }} {{state.x|default:\"none\"}} \\{{raw}} {{prompt:release-style}}").unwrap();
        assert_eq!(t.paths().len(), 3);
        assert_eq!(t.prompt_refs(), vec!["release-style"]);
        let out = t.render(&env, Some(&|n: &str| if n == "release-style" { Some("STYLE".into()) } else { None })).unwrap();
        assert_eq!(out, "Audience: team\nshort none {{raw}} STYLE");
        assert!(render("{{inputs.nope}}", &env).unwrap_err().0.contains("inputs.nope"));
        assert_eq!(render("{{inputs.tags[1]}} {{inputs.n}}", &env).unwrap(), "b 7");
        for bad in ["{{a", "{{}}", "{{a b}}", "{{a|upper}}", "{{a|default:x}}", "{{prompt:}}"] { assert!(parse_template(bad).is_err(), "{bad}"); }
        assert_eq!(parse_template("plain, no holes").unwrap().parts.len(), 1);
    }

    fn cond(s: &str) -> Result<bool, ExprError> { parse_condition(s)?.test(&env) }

    #[test]
    fn conditions() {
        assert_eq!(cond("inputs.n > 5 and inputs.audience == 'team'"), Ok(true));
        assert_eq!(cond("inputs.n * 2 + 1 == 15"), Ok(true));
        assert_eq!(cond("not (inputs.n < 5) && len(inputs.tags) == 2"), Ok(true));
        assert_eq!(cond("inputs.tags contains 'b' and 'a' in inputs.tags"), Ok(true));
        assert_eq!(cond("node.n2.out.summary matches '^sh.*t$'"), Ok(true));
        assert_eq!(cond("inputs.audience matches 'x|tea'"), Ok(true));
        assert_eq!(cond("lower('ABC') == 'abc' or false"), Ok(true));
        assert_eq!(cond("state.round % 2 == 0"), Ok(true));
        assert_eq!(cond("inputs.n in [1, 7]"), Ok(true));
        assert_eq!(cond("inputs.n == 8 or !true"), Ok(false));
        assert_eq!(cond("inputs.audience + '!' == 'team!'"), Ok(true));
        assert!(cond("state.missing > 1").unwrap_err().0.contains("state.missing"));
        assert!(cond("1 / 0").is_err() && cond("'a' < 1").is_err());
        for bad in ["", "a ==", "a = 1", "foo(1)", "(a", "a b", "a and", "1 +", "len(a, b)", "x $ y", "a[", "'open"] { assert!(parse_condition(bad).is_err(), "{bad}"); }
        assert_eq!(parse_condition("a.b[0] > c").unwrap().paths().len(), 2);
    }

    #[test]
    fn regex() {
        for (p, t, want) in [("abc", "xabcx", true), ("^abc$", "xabcx", false), ("a.c", "abc", true), ("a+b", "caaab", true), ("colou?r", "color", true), ("\\d+", "ab12", true), ("[a-c]+z", "abcz", true), ("[^a-c]", "abc", false), ("^$", "", true), ("cat|dog", "hotdog", true), ("\\w\\s\\w", "a b", true), ("a*", "", true)] {
            assert_eq!(regex_search(p, t), Ok(want), "{p} on {t}");
        }
        for bad in ["(a)", "*a", "a**", "[a", "\\"] { assert!(regex_search(bad, "a").is_err(), "{bad}"); }
    }
}
