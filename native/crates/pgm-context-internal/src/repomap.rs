//! Repomap: a source tree as symbols with signatures, not bodies, fitted to a token budget, so a model sees the shape
//! of a repository for a fraction of the cost of its files. The extractor is a deterministic line scanner (no parser,
//! no network): comments and string contents are blanked first, braces are counted, and declarations are read off the
//! lines that sit at the right depth. It is heuristic on purpose and never panics; an exact tree-sitter extractor can
//! sit beside it later. The rendered map is byte-stable for an unchanged tree (files ranked by how many other files
//! mention their symbols, then by path), so it can be cached as a prompt prefix.
use crate::tokens::Counter;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Symbols shown per file unless the caller says otherwise.
pub const MAX_SYMBOLS_PER_FILE: usize = 40;
const MAX_SIGNATURE_CHARS: usize = 160;
const MAX_FILE_BYTES: usize = 512 * 1024;
const HARD_SYMBOL_CAP: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Impl,
    Class,
    Const,
    Static,
    Type,
    Module,
    Heading,
    Key,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    Private,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    Rust,
    JavaScript,
    TypeScript,
    Python,
    Markdown,
    Toml,
    Json,
    Other,
}

impl Language {
    pub fn of_path(path: &str) -> Language {
        let ext = path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "rs" => Language::Rust,
            "js" | "mjs" | "cjs" | "jsx" => Language::JavaScript,
            "ts" | "tsx" | "mts" | "cts" => Language::TypeScript,
            "py" | "pyi" => Language::Python,
            "md" | "markdown" => Language::Markdown,
            "toml" => Language::Toml,
            "json" => Language::Json,
            _ => Language::Other,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::JavaScript => "JavaScript",
            Language::TypeScript => "TypeScript",
            Language::Python => "Python",
            Language::Markdown => "Markdown",
            Language::Toml => "TOML",
            Language::Json => "JSON",
            Language::Other => "other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// One line, whitespace collapsed, at most about 160 characters.
    pub signature: String,
    /// 1-based line of the declaration.
    pub line: usize,
    pub end_line: Option<usize>,
    pub visibility: Visibility,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMap {
    /// Relative, forward slashes.
    pub path: String,
    pub language: Language,
    pub symbols: Vec<Symbol>,
    /// How many other files mention one of this file's symbol names (filled by [`references`]).
    pub references: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    pub files_included: usize,
    /// Files that have at least one symbol (a file with none adds nothing to a map).
    pub files_total: usize,
    pub symbols_included: usize,
    pub tokens: usize,
}

// ---------------------------------------------------------------------------------------------------------------
// ignore rules

/// Whether a path belongs in a map: not build output, vendored or VCS directories, lock files, minified files or
/// binary-looking extensions.
pub fn wanted(path: &str) -> bool {
    let p = path.replace('\\', "/");
    let lower = p.to_ascii_lowercase();
    if p.split('/').any(|seg| {
        matches!(
            seg,
            "target" | "node_modules" | ".git" | "dist" | "build" | "__pycache__"
        )
    }) {
        return false;
    }
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if name.ends_with(".min.js") || name.ends_with(".min.css") || name == "package-lock.json" {
        return false;
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    const SKIP: &[&str] = &[
        "lock", "png", "jpg", "jpeg", "gif", "webp", "ico", "bmp", "pdf", "zip", "gz", "tgz", "xz",
        "bz2", "7z", "rar", "tar", "exe", "dll", "so", "dylib", "a", "o", "rlib", "bin", "wasm",
        "class", "jar", "woff", "woff2", "ttf", "otf", "eot", "mp3", "mp4", "mov", "webm", "wav",
        "ogg", "sqlite", "db", "map",
    ];
    !SKIP.contains(&ext)
}

/// [`wanted`] plus the size rule: files over 512 KiB are not mapped.
pub fn wanted_sized(path: &str, len: usize) -> bool {
    len <= MAX_FILE_BYTES && wanted(path)
}

// ---------------------------------------------------------------------------------------------------------------
// scrubbing: comments and string contents become spaces, one output char per input char, so line numbers and
// columns of the original survive

#[derive(Clone, Copy, PartialEq)]
enum Style {
    Rust,
    Js,
    Py,
}

enum St {
    Normal,
    Block,
    Str { q: char, triple: bool },
}

fn scrub(text: &str, style: Style) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out: Vec<String> = vec![String::new()];
    let mut st = St::Normal;
    let mut line_comment = false;
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c == '\n' {
            out.push(String::new());
            line_comment = false;
            if let St::Str { q, triple } = st {
                if !triple && q != '`' && style != Style::Rust {
                    st = St::Normal;
                }
            }
            i += 1;
            continue;
        }
        let cur = out.last_mut().unwrap();
        if line_comment {
            cur.push(' ');
            i += 1;
            continue;
        }
        let next = chars.get(i + 1).copied();
        match st {
            St::Block => {
                if c == '*' && next == Some('/') {
                    cur.push_str("  ");
                    i += 2;
                    st = St::Normal;
                } else {
                    cur.push(' ');
                    i += 1;
                }
            }
            St::Str { q, triple } => {
                if c == '\\' {
                    cur.push(' ');
                    i += 1;
                    if i < n && chars[i] != '\n' {
                        cur.push(' ');
                        i += 1;
                    }
                } else if c == q {
                    if triple {
                        if next == Some(q) && chars.get(i + 2) == Some(&q) {
                            cur.push_str("   ");
                            i += 3;
                            st = St::Normal;
                        } else {
                            cur.push(' ');
                            i += 1;
                        }
                    } else {
                        cur.push(q);
                        i += 1;
                        st = St::Normal;
                    }
                } else {
                    cur.push(' ');
                    i += 1;
                }
            }
            St::Normal => {
                if style != Style::Py && c == '/' && next == Some('/') {
                    line_comment = true;
                    cur.push(' ');
                    i += 1;
                } else if style != Style::Py && c == '/' && next == Some('*') {
                    st = St::Block;
                    cur.push_str("  ");
                    i += 2;
                } else if style == Style::Py && c == '#' {
                    line_comment = true;
                    cur.push(' ');
                    i += 1;
                } else if style == Style::Rust && c == '\'' {
                    // a char literal is blanked, a lifetime is left alone
                    if next == Some('\\') {
                        let lim = (i + 10).min(n);
                        let mut j = i + 2;
                        while j < lim && chars[j] != '\'' && chars[j] != '\n' {
                            j += 1;
                        }
                        if j < lim && chars[j] == '\'' {
                            for _ in i..=j {
                                cur.push(' ');
                            }
                            i = j + 1;
                            continue;
                        }
                    } else if chars.get(i + 2) == Some(&'\'') && next != Some('\n') {
                        cur.push_str("   ");
                        i += 3;
                        continue;
                    }
                    cur.push(c);
                    i += 1;
                } else if (style == Style::Rust && c == '"')
                    || (style == Style::Js && (c == '"' || c == '\'' || c == '`'))
                {
                    st = St::Str {
                        q: c,
                        triple: false,
                    };
                    cur.push(c);
                    i += 1;
                } else if style == Style::Py && (c == '"' || c == '\'') {
                    if next == Some(c) && chars.get(i + 2) == Some(&c) {
                        st = St::Str { q: c, triple: true };
                        cur.push_str("   ");
                        i += 3;
                    } else {
                        st = St::Str {
                            q: c,
                            triple: false,
                        };
                        cur.push(c);
                        i += 1;
                    }
                } else {
                    cur.push(c);
                    i += 1;
                }
            }
        }
    }
    out
}

fn orig_lines(text: &str) -> Vec<String> {
    text.split('\n').map(|l| l.to_string()).collect()
}

// ---------------------------------------------------------------------------------------------------------------
// shared helpers

fn finish_sig(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX_SIGNATURE_CHARS {
        let mut t: String = collapsed.chars().take(MAX_SIGNATURE_CHARS - 1).collect();
        t.push('…');
        t
    } else {
        collapsed
    }
}

fn ident(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect()
}

fn slice_chars(s: &str, from: usize, to: usize) -> String {
    s.chars().skip(from).take(to.saturating_sub(from)).collect()
}

struct Cut {
    end: usize,
    opens: bool,
    found: bool,
    term: char,
}

/// Where a declaration's signature ends: at the first terminator outside parentheses and brackets; an optional
/// `where` clause is cut from the signature but scanning goes on to the terminator after it.
fn cut_decl(s: &str, terms: &[char], cut_where: bool, arrow: bool) -> Cut {
    let chars: Vec<char> = s.chars().collect();
    let mut depth = 0i32;
    let mut sig_end: Option<usize> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ => {}
        }
        if depth == 0 {
            if terms.contains(&c) {
                return Cut {
                    end: sig_end.unwrap_or(i),
                    opens: c == '{',
                    found: true,
                    term: c,
                };
            }
            if arrow && c == '=' && chars.get(i + 1) == Some(&'>') {
                let rest: String = chars[i + 2..].iter().collect();
                return Cut {
                    end: i + 2,
                    opens: rest.trim_start().starts_with('{'),
                    found: true,
                    term: '>',
                };
            }
            if cut_where && sig_end.is_none() && c == ' ' {
                let w: String = chars[i + 1..].iter().take(6).collect();
                if w == "where " || w == "where" {
                    sig_end = Some(i);
                }
            }
        }
        i += 1;
    }
    Cut {
        end: sig_end.unwrap_or(chars.len()),
        opens: false,
        found: false,
        term: ' ',
    }
}

struct Gathered {
    sig: String,
    scr: String,
    cut: Cut,
    term_line: usize,
}

fn gather(
    orig: &[String],
    scr: &[String],
    i: usize,
    max: usize,
    terms: &[char],
    cut_where: bool,
    arrow: bool,
) -> Gathered {
    let mut o = String::new();
    let mut s = String::new();
    let mut last = i;
    for k in 0..max {
        let li = i + k;
        if li >= scr.len() {
            break;
        }
        if k > 0 {
            o.push(' ');
            s.push(' ');
        }
        o.push_str(&orig[li]);
        s.push_str(&scr[li]);
        last = li;
        let lead = s.chars().take_while(|c| c.is_whitespace()).count();
        let st: String = s.chars().skip(lead).collect();
        let cut = cut_decl(&st, terms, cut_where, arrow);
        if cut.found {
            let ot: String = o.chars().skip(lead).collect();
            return Gathered {
                sig: finish_sig(&slice_chars(&ot, 0, cut.end)),
                scr: st,
                cut,
                term_line: li,
            };
        }
    }
    let lead = s.chars().take_while(|c| c.is_whitespace()).count();
    let st: String = s.chars().skip(lead).collect();
    let ot: String = o.chars().skip(lead).collect();
    let cut = cut_decl(&st, terms, cut_where, arrow);
    Gathered {
        sig: finish_sig(&slice_chars(&ot, 0, cut.end)),
        scr: st,
        cut,
        term_line: last,
    }
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap())
}

// ---------------------------------------------------------------------------------------------------------------
// brace languages (Rust, JS, TS)

struct CtxSpec {
    name: String,
    is_type: bool,
    public: bool,
    skip: bool,
}

struct Found {
    kind: SymbolKind,
    name: String,
    public: bool,
    terms: &'static [char],
    arrow: bool,
    ctx: Option<CtxSpec>,
    method_of: Option<String>,
}

struct Ctx {
    inner: i64,
    term_line: usize,
    opened: bool,
    spec: CtxSpec,
}

struct Open {
    idx: Option<usize>,
    depth_before: i64,
    term_line: usize,
    opened: bool,
}

const BODY: &[char] = &['{', ';'];
const EQ: &[char] = &['=', ';'];

fn strip_vis(t: &str) -> (bool, &str) {
    if let Some(r) = t.strip_prefix("pub") {
        if r.starts_with('(') {
            if let Some(end) = r.find(')') {
                return (true, r[end + 1..].trim_start());
            }
        } else if r.starts_with(char::is_whitespace) {
            return (true, r.trim_start());
        }
    }
    (false, t)
}

fn strip_mods(mut r: &str) -> &str {
    loop {
        let before = r;
        for m in ["async ", "unsafe ", "default ", "extern "] {
            if let Some(x) = r.strip_prefix(m) {
                r = x.trim_start();
            }
        }
        if r.starts_with('"') {
            if let Some(end) = r[1..].find('"') {
                r = r[end + 2..].trim_start();
            }
        }
        if let Some(x) = r.strip_prefix("const fn ") {
            r = &r[r.len() - x.len() - 3..];
        }
        if r == before {
            return r;
        }
    }
}

fn skip_generics(s: &str) -> &str {
    if !s.starts_with('<') {
        return s;
    }
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return s[i + 1..].trim_start();
                }
            }
            _ => {}
        }
    }
    s
}

fn type_base(target: &str) -> String {
    let mut t = target.trim();
    loop {
        let before = t;
        for p in ["&", "mut ", "dyn ", "'static "] {
            if let Some(x) = t.strip_prefix(p) {
                t = x.trim_start();
            }
        }
        if t == before {
            break;
        }
    }
    let path: String = t
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
        .collect();
    path.rsplit("::").next().unwrap_or("").to_string()
}

fn rust_item(t: &str, enc: Option<&Ctx>) -> Option<Found> {
    if t.starts_with('#') {
        return None;
    }
    let (public, rest) = strip_vis(t);
    let rest = strip_mods(rest);
    let simple = |kind: SymbolKind,
                  r: &str,
                  terms: &'static [char],
                  ctx: Option<CtxSpec>|
     -> Option<Found> {
        let name = ident(r.trim_start());
        if name.is_empty() || name == "_" {
            return None;
        }
        Some(Found {
            kind,
            name,
            public,
            terms,
            arrow: false,
            ctx,
            method_of: None,
        })
    };
    if let Some(r) = rest.strip_prefix("fn ") {
        let mut f = simple(SymbolKind::Function, r, BODY, None)?;
        if let Some(c) = enc.filter(|c| c.spec.is_type) {
            f.kind = SymbolKind::Method;
            f.method_of = Some(c.spec.name.clone());
            f.public = f.public || c.spec.public;
        }
        return Some(f);
    }
    for (kw, kind) in [
        ("struct ", SymbolKind::Struct),
        ("enum ", SymbolKind::Enum),
        ("union ", SymbolKind::Struct),
    ] {
        if let Some(r) = rest.strip_prefix(kw) {
            return simple(kind, r, BODY, None);
        }
    }
    if let Some(r) = rest.strip_prefix("trait ") {
        let name = ident(r.trim_start());
        return simple(
            SymbolKind::Trait,
            r,
            BODY,
            Some(CtxSpec {
                name,
                is_type: true,
                public: true,
                skip: false,
            }),
        );
    }
    if let Some(r) = rest.strip_prefix("type ") {
        return simple(SymbolKind::Type, r, EQ, None);
    }
    if let Some(r) = rest.strip_prefix("mod ") {
        let name = ident(r.trim_start());
        let skip = name == "tests";
        return simple(
            SymbolKind::Module,
            r,
            BODY,
            Some(CtxSpec {
                name,
                is_type: false,
                public: false,
                skip,
            }),
        );
    }
    if let Some(r) = rest.strip_prefix("const ") {
        return simple(SymbolKind::Const, r, EQ, None);
    }
    if let Some(r) = rest.strip_prefix("static ") {
        return simple(
            SymbolKind::Static,
            r.trim_start().strip_prefix("mut ").unwrap_or(r),
            EQ,
            None,
        );
    }
    if rest == "impl" || rest.starts_with("impl ") || rest.starts_with("impl<") {
        let after = skip_generics(rest[4..].trim_start());
        let txt = after.split('{').next().unwrap_or("");
        let txt = txt.split(" where ").next().unwrap_or(txt);
        let (is_trait, target) = match txt.find(" for ") {
            Some(p) => (true, &txt[p + 5..]),
            None => (false, txt),
        };
        let mut name = type_base(target);
        if name.is_empty() {
            name = "impl".to_string();
        }
        return Some(Found {
            kind: SymbolKind::Impl,
            name: name.clone(),
            public,
            terms: BODY,
            arrow: false,
            ctx: Some(CtxSpec {
                name,
                is_type: true,
                public: is_trait,
                skip: false,
            }),
            method_of: None,
        });
    }
    None
}

fn js_method_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"^((?:(?:public|private|protected|static|async|readonly|override|abstract|get|set)\s+)*)\*?\s*([#\w$]+)\s*(?:<[^>]*>)?\s*\(",
    )
}

fn js_prop_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"^((?:(?:public|private|protected|static|readonly|override)\s+)*)([#\w$]+)\s*(?::[^=]+)?=\s*(?:async\s+)?(?:\(|[\w$]+\s*=>|function)",
    )
}

fn js_item(t: &str, enc: Option<&Ctx>) -> Option<Found> {
    if let Some(c) = enc.filter(|c| c.spec.is_type) {
        const NOT: &[&str] = &[
            "if", "for", "while", "switch", "return", "catch", "function", "super", "new",
            "typeof", "await", "else", "do", "try", "with", "delete", "void",
        ];
        let (caps, arrow) = match js_method_re().captures(t) {
            Some(c) => (c, false),
            None => (js_prop_re().captures(t)?, true),
        };
        let mods = caps.get(1).map_or("", |m| m.as_str());
        let name = caps.get(2)?.as_str().to_string();
        if NOT.contains(&name.as_str()) {
            return None;
        }
        let private =
            mods.contains("private") || mods.contains("protected") || name.starts_with('#');
        return Some(Found {
            kind: SymbolKind::Method,
            name,
            public: !private,
            terms: BODY,
            arrow,
            ctx: None,
            method_of: Some(c.spec.name.clone()),
        });
    }
    let mut r = t;
    let mut exported = false;
    let mut default = false;
    if let Some(x) = r.strip_prefix("export ") {
        exported = true;
        r = x.trim_start();
        if let Some(y) = r.strip_prefix("default ") {
            default = true;
            r = y.trim_start();
        }
    }
    for p in ["declare ", "abstract "] {
        if let Some(x) = r.strip_prefix(p) {
            r = x.trim_start();
        }
    }
    if let Some(x) = r.strip_prefix("async ") {
        if x.trim_start().starts_with("function") {
            r = x.trim_start();
        }
    }
    let mk = |kind: SymbolKind,
              name: String,
              terms: &'static [char],
              arrow: bool,
              ctx: Option<CtxSpec>| Found {
        kind,
        name,
        public: exported,
        terms,
        arrow,
        ctx,
        method_of: None,
    };
    if let Some(x) = r.strip_prefix("function") {
        if x.starts_with([' ', '*', '(']) {
            let name = ident(x.trim_start().trim_start_matches('*').trim_start());
            let name = if name.is_empty() {
                "default".to_string()
            } else {
                name
            };
            return Some(mk(SymbolKind::Function, name, BODY, false, None));
        }
    }
    if let Some(x) = r.strip_prefix("class ") {
        let mut name = ident(x.trim_start());
        if name.is_empty() || name == "extends" {
            name = "default".into();
        }
        return Some(mk(
            SymbolKind::Class,
            name.clone(),
            BODY,
            false,
            Some(CtxSpec {
                name,
                is_type: true,
                public: true,
                skip: false,
            }),
        ));
    }
    if let Some(x) = r.strip_prefix("interface ") {
        return Some(mk(
            SymbolKind::Trait,
            ident(x.trim_start()),
            BODY,
            false,
            None,
        ))
        .filter(|f| !f.name.is_empty());
    }
    if let Some(x) = r
        .strip_prefix("const enum ")
        .or_else(|| r.strip_prefix("enum "))
    {
        return Some(mk(
            SymbolKind::Enum,
            ident(x.trim_start()),
            BODY,
            false,
            None,
        ))
        .filter(|f| !f.name.is_empty());
    }
    if let Some(x) = r.strip_prefix("type ") {
        return Some(mk(SymbolKind::Type, ident(x.trim_start()), EQ, false, None))
            .filter(|f| !f.name.is_empty());
    }
    if let Some(x) = r
        .strip_prefix("namespace ")
        .or_else(|| r.strip_prefix("module "))
    {
        let name = ident(x.trim_start());
        if name.is_empty() {
            return None;
        }
        return Some(mk(
            SymbolKind::Module,
            name.clone(),
            BODY,
            false,
            Some(CtxSpec {
                name,
                is_type: false,
                public: false,
                skip: false,
            }),
        ));
    }
    for kw in ["const ", "let ", "var "] {
        if let Some(x) = r.strip_prefix(kw) {
            let name = ident(x.trim_start());
            if name.is_empty() {
                return None;
            }
            let after = &x.trim_start()[name.len()..];
            if let Some(eq) = after.find('=') {
                let value = after[eq + 1..].trim_start();
                let fn_like = value.starts_with('(')
                    || value.starts_with("function")
                    || (value.starts_with("async")
                        && !value[5..].starts_with(|c: char| c.is_alphanumeric() && false))
                    || value.starts_with('<')
                    || (ident(value).len() > 0
                        && value[ident(value).len()..].trim_start().starts_with("=>"));
                if fn_like && !after[..eq].contains(['(', ')']) {
                    return Some(mk(SymbolKind::Function, name, BODY, true, None));
                }
            }
            if exported {
                return Some(mk(SymbolKind::Const, name, EQ, false, None));
            }
            return None;
        }
    }
    if default {
        return Some(mk(SymbolKind::Const, "default".into(), EQ, false, None));
    }
    None
}

fn method_sig_rust(sig: &str, target: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = re(
        &R,
        r#"^(pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const|default|extern(?:\s+"[^"]*")?)\s+)*fn\s+"#,
    );
    r.replace(sig, |c: &regex::Captures| {
        format!("{}{}::", c.get(1).map_or("", |m| m.as_str()), target)
    })
    .into_owned()
}

fn method_sig_js(sig: &str, scr: &str, class: &str) -> String {
    match js_method_re()
        .captures(scr)
        .or_else(|| js_prop_re().captures(scr))
        .and_then(|c| c.get(2))
    {
        Some(m) => {
            let from = scr[..m.start()].chars().count();
            let rest: String = sig.chars().skip(from).collect();
            format!("{class}.{rest}")
        }
        None => sig.to_string(),
    }
}

fn scan_braced(lang: Language, orig: &[String], scr: &[String]) -> Vec<Symbol> {
    let rust = lang == Language::Rust;
    let mut out: Vec<Symbol> = Vec::new();
    let mut ctxs: Vec<Ctx> = Vec::new();
    let mut pends: Vec<Open> = Vec::new();
    let mut depth: i64 = 0;
    let mut cfg_test = false;
    for i in 0..scr.len() {
        let s = &scr[i];
        let t = s.trim();
        let opens = s.matches('{').count() as i64;
        let closes = s.matches('}').count() as i64;
        let cur_inner = ctxs.last().map(|c| c.inner).unwrap_or(0);
        let in_skip = ctxs.iter().any(|c| c.spec.skip);
        if rust && t.starts_with("#[") {
            if t.contains("cfg(test)") {
                cfg_test = true;
            }
        } else if !t.is_empty() {
            if depth == cur_inner && !in_skip && out.len() < HARD_SYMBOL_CAP {
                let found = if rust {
                    rust_item(t, ctxs.last())
                } else {
                    js_item(t, ctxs.last())
                };
                if let Some(mut f) = found {
                    let g = gather(orig, scr, i, 8, f.terms, rust, f.arrow);
                    let mut skip_ctx = false;
                    if let Some(c) = f.ctx.as_mut() {
                        if cfg_test {
                            c.skip = true;
                        }
                        skip_ctx = c.skip;
                    }
                    let record = !(skip_ctx && f.kind == SymbolKind::Module);
                    let sig = match (&f.method_of, rust) {
                        (Some(target), true) => finish_sig(&method_sig_rust(&g.sig, target)),
                        (Some(class), false) => finish_sig(&method_sig_js(&g.sig, &g.scr, class)),
                        _ => g.sig.clone(),
                    };
                    let mut idx = None;
                    if record {
                        let visibility = if f.public {
                            Visibility::Public
                        } else {
                            Visibility::Private
                        };
                        let mut end_line = None;
                        if g.cut.found && !g.cut.opens && g.cut.term == ';' {
                            end_line = Some(g.term_line + 1);
                        }
                        out.push(Symbol {
                            name: f.name.clone(),
                            kind: f.kind,
                            signature: sig,
                            line: i + 1,
                            end_line,
                            visibility,
                        });
                        idx = Some(out.len() - 1);
                    }
                    if g.cut.found && g.cut.opens {
                        pends.push(Open {
                            idx,
                            depth_before: depth,
                            term_line: g.term_line,
                            opened: false,
                        });
                        if let Some(spec) = f.ctx.take() {
                            ctxs.push(Ctx {
                                inner: depth + 1,
                                term_line: g.term_line,
                                opened: false,
                                spec,
                            });
                        }
                    }
                }
            }
            cfg_test = false;
        }
        depth = (depth + opens - closes).max(0);
        for p in pends.iter_mut() {
            if !p.opened && i >= p.term_line && opens > 0 {
                p.opened = true;
            }
        }
        pends.retain(|p| {
            if p.opened && depth <= p.depth_before {
                if let Some(ix) = p.idx {
                    out[ix].end_line = Some(i + 1);
                }
                false
            } else {
                true
            }
        });
        for c in ctxs.iter_mut() {
            if !c.opened && i >= c.term_line && opens > 0 {
                c.opened = true;
            }
        }
        while let Some(top) = ctxs.last() {
            if top.opened && depth < top.inner {
                ctxs.pop();
            } else {
                break;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// Python

fn py_symbols(orig: &[String], scr: &[String]) -> Vec<Symbol> {
    static CONST: OnceLock<Regex> = OnceLock::new();
    let const_re = re(&CONST, r"^([A-Z][A-Z0-9_]*)\s*(?::[^=]+)?=");
    let mut out: Vec<Symbol> = Vec::new();
    // (indent, is_class, name, symbol index)
    let mut stack: Vec<(usize, bool, String, Option<usize>)> = Vec::new();
    let mut last = 0usize;
    let mut skip_to = 0usize;
    for i in 0..scr.len() {
        let s = &scr[i];
        let t = s.trim_start();
        if t.trim().is_empty() {
            continue;
        }
        if i < skip_to {
            last = i + 1;
            continue;
        }
        let indent = s.chars().take_while(|c| c.is_whitespace()).count();
        while let Some(top) = stack.last() {
            if indent <= top.0 {
                if let Some(ix) = top.3 {
                    out[ix].end_line = Some(last);
                }
                stack.pop();
            } else {
                break;
            }
        }
        last = i + 1;
        if t.starts_with('@') {
            continue;
        }
        let t2 = match t.strip_prefix("async ") {
            Some(x) if x.trim_start().starts_with("def ") => x.trim_start(),
            _ => t,
        };
        let (is_class, rest) = if let Some(r) = t2.strip_prefix("def ") {
            (false, r)
        } else if let Some(r) = t.strip_prefix("class ") {
            (true, r)
        } else {
            if indent == 0 && stack.is_empty() && out.len() < HARD_SYMBOL_CAP {
                if let Some(c) = const_re.captures(t) {
                    let g = gather(orig, scr, i, 1, &['='], false, false);
                    out.push(Symbol {
                        name: c[1].to_string(),
                        kind: SymbolKind::Const,
                        signature: g.sig,
                        line: i + 1,
                        end_line: None,
                        visibility: Visibility::Public,
                    });
                }
            }
            continue;
        };
        let name = ident(rest.trim_start());
        if name.is_empty() {
            continue;
        }
        let g = gather(orig, scr, i, 8, &[':'], false, false);
        skip_to = if g.cut.found { g.term_line + 1 } else { i + 1 };
        let enc = stack.last();
        let recordable = enc.map_or(true, |e| e.1);
        let mut idx = None;
        if recordable && out.len() < HARD_SYMBOL_CAP {
            let sig = if is_class {
                g.sig.clone()
            } else {
                let body = g.sig.strip_prefix("async ").unwrap_or(&g.sig);
                let body = body.strip_prefix("def ").unwrap_or(body);
                match enc {
                    Some(e) => format!("{}.{}", e.2, body),
                    None => format!("def {body}"),
                }
            };
            let kind = if is_class {
                SymbolKind::Class
            } else if enc.is_some() {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            };
            let private =
                name.starts_with('_') && !(name.starts_with("__") && name.ends_with("__"));
            out.push(Symbol {
                name: name.clone(),
                kind,
                signature: sig,
                line: i + 1,
                end_line: None,
                visibility: if private {
                    Visibility::Private
                } else {
                    Visibility::Public
                },
            });
            idx = Some(out.len() - 1);
        }
        stack.push((indent, is_class, name, idx));
    }
    while let Some(top) = stack.pop() {
        if let Some(ix) = top.3 {
            out[ix].end_line = Some(last);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// Markdown, TOML, JSON

fn md_symbols(text: &str) -> Vec<Symbol> {
    static H: OnceLock<Regex> = OnceLock::new();
    let h = re(&H, r"^ {0,3}(#{1,6})\s+(.+?)\s*#*\s*$");
    let mut out = Vec::new();
    let mut fenced = false;
    for (i, l) in text.split('\n').enumerate() {
        let l = l.trim_end_matches('\r');
        let tl = l.trim_start();
        if tl.starts_with("```") || tl.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || out.len() >= HARD_SYMBOL_CAP {
            continue;
        }
        if let Some(c) = h.captures(l) {
            out.push(Symbol {
                name: c[2].to_string(),
                kind: SymbolKind::Heading,
                signature: finish_sig(&format!("{} {}", &c[1], &c[2])),
                line: i + 1,
                end_line: None,
                visibility: Visibility::Unknown,
            });
        }
    }
    out
}

fn toml_symbols(text: &str) -> Vec<Symbol> {
    static K: OnceLock<Regex> = OnceLock::new();
    let k = re(&K, r#"^([A-Za-z0-9_.\-"']+)\s*="#);
    let mut out = Vec::new();
    let mut in_table = false;
    for (i, l) in text.split('\n').enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') || out.len() >= HARD_SYMBOL_CAP {
            continue;
        }
        if t.starts_with('[') {
            in_table = true;
            let name = t
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            out.push(Symbol {
                name,
                kind: SymbolKind::Key,
                signature: finish_sig(t),
                line: i + 1,
                end_line: None,
                visibility: Visibility::Unknown,
            });
        } else if !in_table {
            if let Some(c) = k.captures(t) {
                out.push(Symbol {
                    name: c[1].trim_matches(['"', '\'']).to_string(),
                    kind: SymbolKind::Key,
                    signature: finish_sig(t),
                    line: i + 1,
                    end_line: None,
                    visibility: Visibility::Unknown,
                });
            }
        }
    }
    out
}

fn json_symbols(text: &str) -> Vec<Symbol> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut stack: Vec<char> = Vec::new();
    let mut line = 1usize;
    let mut i = 0;
    while i < n && out.len() < HARD_SYMBOL_CAP {
        let c = chars[i];
        match c {
            '\n' => line += 1,
            '{' | '[' => stack.push(c),
            '}' | ']' => {
                stack.pop();
            }
            '"' => {
                let start_line = line;
                let mut name = String::new();
                i += 1;
                while i < n && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < n {
                        i += 1;
                    }
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    if stack.len() == 1 {
                        name.push(chars[i]);
                    }
                    i += 1;
                }
                if stack.len() == 1 && stack[0] == '{' {
                    let mut j = i + 1;
                    while j < n && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j < n && chars[j] == ':' {
                        j += 1;
                        while j < n && chars[j].is_whitespace() {
                            j += 1;
                        }
                        let ty = match chars.get(j) {
                            Some('{') => "object",
                            Some('[') => "array",
                            Some('"') => "string",
                            Some(c) if c.is_ascii_digit() || *c == '-' => "number",
                            Some('t') | Some('f') => "boolean",
                            Some('n') => "null",
                            _ => "value",
                        };
                        out.push(Symbol {
                            signature: finish_sig(&format!("{name}: {ty}")),
                            name,
                            kind: SymbolKind::Key,
                            line: start_line,
                            end_line: None,
                            visibility: Visibility::Unknown,
                        });
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// public extraction

fn norm_path(path: &str) -> String {
    let p = path.replace('\\', "/");
    p.strip_prefix("./").unwrap_or(&p).to_string()
}

/// The heuristic extractor: always available, no parser. Never panics, whatever the text.
pub fn extract(path: &str, text: &str) -> FileMap {
    let path = norm_path(path);
    let language = Language::of_path(&path);
    let symbols = match language {
        Language::Rust => scan_braced(language, &orig_lines(text), &scrub(text, Style::Rust)),
        Language::JavaScript | Language::TypeScript => {
            scan_braced(language, &orig_lines(text), &scrub(text, Style::Js))
        }
        Language::Python => py_symbols(&orig_lines(text), &scrub(text, Style::Py)),
        Language::Markdown => md_symbols(text),
        Language::Toml => toml_symbols(text),
        Language::Json => json_symbols(text),
        Language::Other => Vec::new(),
    };
    FileMap {
        path,
        language,
        symbols,
        references: 0,
    }
}

fn is_code_kind(k: SymbolKind) -> bool {
    !matches!(k, SymbolKind::Heading | SymbolKind::Key | SymbolKind::Impl)
}

/// Fill `references`: for each file, how many OTHER files mention one of its symbol names (whole-word matches, names
/// of at least 4 characters, headings and keys excluded). `sources` are the (path, text) pairs the maps came from.
pub fn references(files: &mut [FileMap], sources: &[(String, String)]) {
    let texts: HashMap<String, &str> = sources
        .iter()
        .map(|(p, t)| (norm_path(p), t.as_str()))
        .collect();
    let mut owners: HashMap<&str, Vec<usize>> = HashMap::new();
    for (fi, f) in files.iter().enumerate() {
        let mut seen: HashSet<&str> = HashSet::new();
        for s in &f.symbols {
            if is_code_kind(s.kind) && s.name.chars().count() >= 4 && seen.insert(s.name.as_str()) {
                owners.entry(s.name.as_str()).or_default().push(fi);
            }
        }
    }
    let mut mentioned_by: Vec<HashSet<usize>> = vec![HashSet::new(); files.len()];
    for (gi, g) in files.iter().enumerate() {
        let Some(text) = texts.get(&g.path) else {
            continue;
        };
        let words: HashSet<&str> = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|w| !w.is_empty())
            .collect();
        for w in words {
            if let Some(fs) = owners.get(w) {
                for &fi in fs {
                    if fi != gi {
                        mentioned_by[fi].insert(gi);
                    }
                }
            }
        }
    }
    for (f, m) in files.iter_mut().zip(mentioned_by) {
        f.references = m.len() as u32;
    }
}

/// Order files by references descending, then path ascending, so the output is stable.
pub fn rank(files: &mut [FileMap]) {
    files.sort_by(|a, b| {
        b.references
            .cmp(&a.references)
            .then_with(|| a.path.cmp(&b.path))
    });
}

pub fn render(files: &[FileMap], budget: usize, counter: &dyn Counter) -> Rendered {
    render_with(files, budget, counter, MAX_SYMBOLS_PER_FILE)
}

/// Fill the budget most-referenced first (`files` should be ranked). A file that fits whole is written whole (up to
/// `max_per_file` symbols); one that does not gets its public symbols first, then the rest while room remains, ending
/// with `  … K more`. The last line is always the `[repomap: ...]` summary, and the whole text stays within `budget`
/// tokens whenever the budget can hold the summary at all.
pub fn render_with(
    files: &[FileMap],
    budget: usize,
    counter: &dyn Counter,
    max_per_file: usize,
) -> Rendered {
    let max_per_file = max_per_file.max(1);
    let reserve = counter.count("[repomap: 99999 of 99999 files, 999999 symbols, ~9999999 tokens]");
    let more_reserve = counter.count("  … 99999 more\n");
    let with_syms: Vec<&FileMap> = files.iter().filter(|f| !f.symbols.is_empty()).collect();
    let total = with_syms.len();
    let mut blocks: Vec<(String, usize)> = Vec::new();
    let mut used = 0usize;
    for f in with_syms {
        let shown = &f.symbols[..f.symbols.len().min(max_per_file)];
        let header = format!(
            "{} ({}, {} symbols)\n",
            f.path,
            f.language.name(),
            f.symbols.len()
        );
        let lines: Vec<String> = shown
            .iter()
            .map(|s| format!("  {}: {}\n", s.line, s.signature))
            .collect();
        let mut block = header.clone();
        for l in &lines {
            block.push_str(l);
        }
        let hidden = f.symbols.len() - shown.len();
        if hidden > 0 {
            block.push_str(&format!("  … {hidden} more\n"));
        }
        let cost = counter.count(&block);
        if used + cost + reserve <= budget {
            used += cost;
            blocks.push((block, shown.len()));
            continue;
        }
        let remaining = budget.saturating_sub(used + reserve);
        let hcost = counter.count(&header);
        if hcost + more_reserve >= remaining {
            continue;
        }
        let mut order: Vec<usize> = (0..shown.len())
            .filter(|&k| shown[k].visibility != Visibility::Private)
            .collect();
        order.extend((0..shown.len()).filter(|&k| shown[k].visibility == Visibility::Private));
        let mut chosen: Vec<usize> = Vec::new();
        let mut spent = hcost + more_reserve;
        for k in order {
            let lc = counter.count(&lines[k]);
            if spent + lc <= remaining {
                chosen.push(k);
                spent += lc;
            }
        }
        if chosen.is_empty() {
            continue;
        }
        chosen.sort_unstable();
        let mut pb = header.clone();
        for &k in &chosen {
            pb.push_str(&lines[k]);
        }
        pb.push_str(&format!("  … {} more\n", f.symbols.len() - chosen.len()));
        let pc = counter.count(&pb);
        if used + pc + reserve <= budget {
            used += pc;
            blocks.push((pb, chosen.len()));
        }
    }
    // the sum of per-block counts can differ from the count of the whole text: trim until the whole fits
    loop {
        let body: String = blocks.iter().map(|b| b.0.as_str()).collect();
        let syms: usize = blocks.iter().map(|b| b.1).sum();
        let body_tokens = counter.count(&body);
        let draft = format!(
            "[repomap: {} of {} files, {} symbols, ~{} tokens]",
            blocks.len(),
            total,
            syms,
            body_tokens
        );
        let tokens = body_tokens + counter.count(&draft);
        if tokens > budget && !blocks.is_empty() {
            blocks.pop();
            continue;
        }
        let line = format!(
            "[repomap: {} of {} files, {} symbols, ~{} tokens]",
            blocks.len(),
            total,
            syms,
            tokens
        );
        return Rendered {
            text: format!("{body}{line}"),
            files_included: blocks.len(),
            files_total: total,
            symbols_included: syms,
            tokens,
        };
    }
}

/// The exact extractor (feature `tree-sitter`): real grammars for Rust, Python, JavaScript and TypeScript. `None` for
/// any other language or when the text does not parse at all; callers fall back to [`extract`].
#[cfg(feature = "tree-sitter")]
pub fn extract_exact(path: &str, text: &str) -> Option<FileMap> {
    let path = norm_path(path);
    let language = Language::of_path(&path);
    let lang: tree_sitter::Language = match language {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::TypeScript if path.to_ascii_lowercase().ends_with("x") => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        _ => return None,
    };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).ok()?;
    let tree = parser.parse(text, None)?;
    let mut symbols = Vec::new();
    ts_walk(tree.root_node(), text, language, None, 0, &mut symbols);
    Some(FileMap { path, language, symbols, references: 0 })
}

#[cfg(feature = "tree-sitter")]
fn ts_walk(node: tree_sitter::Node, src: &str, lang: Language, owner: Option<&str>, depth: usize, out: &mut Vec<Symbol>) {
    if depth > 64 || out.len() >= HARD_SYMBOL_CAP {
        return;
    }
    let text_of = |n: tree_sitter::Node| src.get(n.start_byte()..n.end_byte()).unwrap_or("").to_string();
    let sig_of = |n: tree_sitter::Node| {
        let end = n.child_by_field_name("body").map(|b| b.start_byte()).unwrap_or(n.end_byte());
        let raw = src.get(n.start_byte()..end).unwrap_or("");
        finish_sig(raw.trim_end().trim_end_matches([':', '{']).trim_end())
    };
    let name_of = |n: tree_sitter::Node| n.child_by_field_name("name").map(text_of).unwrap_or_default();
    let push = |n: tree_sitter::Node, name: String, kind: SymbolKind, public: bool, sig: String, out: &mut Vec<Symbol>| {
        if name.is_empty() {
            return;
        }
        out.push(Symbol {
            name,
            kind,
            signature: sig,
            line: n.start_position().row + 1,
            end_line: Some(n.end_position().row + 1),
            visibility: if public { Visibility::Public } else { Visibility::Private },
        });
    };
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let k = child.kind();
        match lang {
            Language::Rust => {
                let public = child.children(&mut child.walk()).any(|c| c.kind() == "visibility_modifier");
                match k {
                    "function_item" | "function_signature_item" => {
                        let name = name_of(child);
                        let mut sig = sig_of(child);
                        let kind = if let Some(o) = owner {
                            sig = method_sig_rust(&sig, o);
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        };
                        push(child, name, kind, public || owner.is_some(), sig, out);
                    }
                    "struct_item" | "union_item" => push(child, name_of(child), SymbolKind::Struct, public, sig_of(child), out),
                    "enum_item" => push(child, name_of(child), SymbolKind::Enum, public, sig_of(child), out),
                    "type_item" => push(child, name_of(child), SymbolKind::Type, public, finish_sig(&text_of(child).split('=').next().unwrap_or("")), out),
                    "const_item" => push(child, name_of(child), SymbolKind::Const, public, finish_sig(&text_of(child).split('=').next().unwrap_or("").trim_end_matches(';')), out),
                    "static_item" => push(child, name_of(child), SymbolKind::Static, public, finish_sig(&text_of(child).split('=').next().unwrap_or("").trim_end_matches(';')), out),
                    "trait_item" => {
                        let name = name_of(child);
                        push(child, name.clone(), SymbolKind::Trait, public, sig_of(child), out);
                        if let Some(b) = child.child_by_field_name("body") {
                            ts_walk(b, src, lang, Some(&name), depth + 1, out);
                        }
                    }
                    "impl_item" => {
                        let name = child.child_by_field_name("type").map(|t| type_base(&text_of(t))).unwrap_or_default();
                        push(child, name.clone(), SymbolKind::Impl, false, sig_of(child), out);
                        if let Some(b) = child.child_by_field_name("body") {
                            ts_walk(b, src, lang, Some(&name), depth + 1, out);
                        }
                    }
                    "mod_item" => {
                        let name = name_of(child);
                        if name != "tests" {
                            push(child, name, SymbolKind::Module, public, sig_of(child), out);
                            if let Some(b) = child.child_by_field_name("body") {
                                ts_walk(b, src, lang, None, depth + 1, out);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Language::Python => match k {
                "decorated_definition" => ts_walk(child, src, lang, owner, depth + 1, out),
                "function_definition" => {
                    let name = name_of(child);
                    let sig = sig_of(child);
                    let sig = sig.strip_prefix("async ").unwrap_or(&sig).strip_prefix("def ").unwrap_or(&sig).to_string();
                    let private = name.starts_with('_') && !(name.starts_with("__") && name.ends_with("__"));
                    let (kind, sig) = match owner {
                        Some(o) => (SymbolKind::Method, format!("{o}.{sig}")),
                        None => (SymbolKind::Function, format!("def {sig}")),
                    };
                    push(child, name, kind, !private, sig, out);
                }
                "class_definition" => {
                    let name = name_of(child);
                    push(child, name.clone(), SymbolKind::Class, !name.starts_with('_'), sig_of(child), out);
                    if let Some(b) = child.child_by_field_name("body") {
                        ts_walk(b, src, lang, Some(&name), depth + 1, out);
                    }
                }
                _ => {}
            },
            _ => match k {
                "export_statement" => ts_walk(child, src, lang, owner, depth + 1, out),
                "function_declaration" | "generator_function_declaration" => push(child, name_of(child), SymbolKind::Function, true, sig_of(child), out),
                "class_declaration" | "abstract_class_declaration" => {
                    let name = name_of(child);
                    push(child, name.clone(), SymbolKind::Class, true, sig_of(child), out);
                    if let Some(b) = child.child_by_field_name("body") {
                        ts_walk(b, src, lang, Some(&name), depth + 1, out);
                    }
                }
                "method_definition" => {
                    let name = name_of(child);
                    let private = name.starts_with('#') || text_of(child).trim_start().starts_with("private");
                    let sig = format!("{}.{}", owner.unwrap_or(""), sig_of(child));
                    push(child, name, SymbolKind::Method, !private, sig, out);
                }
                "interface_declaration" => push(child, name_of(child), SymbolKind::Trait, true, sig_of(child), out),
                "type_alias_declaration" => push(child, name_of(child), SymbolKind::Type, true, finish_sig(text_of(child).split('=').next().unwrap_or("")), out),
                "enum_declaration" => push(child, name_of(child), SymbolKind::Enum, true, sig_of(child), out),
                "lexical_declaration" | "variable_declaration" => {
                    let mut c2 = child.walk();
                    for d in child.named_children(&mut c2) {
                        if d.kind() != "variable_declarator" {
                            continue;
                        }
                        let is_fn = d.child_by_field_name("value").map_or(false, |v| matches!(v.kind(), "arrow_function" | "function_expression" | "function"));
                        if is_fn {
                            let name = name_of(d);
                            let sig = finish_sig(text_of(child).split("=>").next().unwrap_or("").split('{').next().unwrap_or(""));
                            push(child, name, SymbolKind::Function, node.kind() == "export_statement", sig, out);
                        }
                    }
                }
                _ => {}
            },
        }
    }
}

/// Extract, rank and render in one call. Unwanted paths (see [`wanted_sized`]) are skipped.
pub fn build(
    sources: &[(String, String)],
    budget_tokens: usize,
    counter: &dyn Counter,
) -> Rendered {
    let kept: Vec<(String, String)> = sources
        .iter()
        .filter(|(p, t)| wanted_sized(p, t.len()))
        .cloned()
        .collect();
    let mut files: Vec<FileMap> = kept.iter().map(|(p, t)| extract(p, t)).collect();
    references(&mut files, &kept);
    rank(&mut files);
    render(&files, budget_tokens, counter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::Estimator;

    const RUST: &str = r#"
//! module docs with fn fake() {}
use std::fmt;

/// A shaper.
pub struct Shaper {
    cap: usize,
}

pub enum Mode { A, B }

pub trait Fold {
    fn fold(&self, x: &str) -> String;
}

impl<T: Clone> Shaper
where
    T: Send,
{
    pub fn shape(&mut self, text: &str) -> String {
        let s = "fn not_a_symbol() {";
        helper(text)
    }
    fn private_one(&self) {}
}

impl Fold for Shaper {
    fn fold(&self, x: &str) -> String { x.to_string() }
}

pub(crate) fn helper<T>(x: T) -> T
where
    T: Clone,
{
    x
}

pub const LIMIT: usize = 10;
static NAME: &str = "x";
type Alias = Vec<u8>;
pub mod inner {
    pub fn deep() {}
}

#[cfg(test)]
mod tests {
    fn hidden_test_fn() {}
}
"#;

    fn find<'a>(m: &'a FileMap, name: &str) -> &'a Symbol {
        m.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "no {name} in {:?}",
                    m.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
                )
            })
    }

    #[cfg(feature = "tree-sitter")]
    #[test]
    fn exact_extraction_agrees_with_the_heuristic_on_the_main_symbols() {
        let exact = extract_exact("src/lib.rs", RUST).unwrap();
        let shape = find(&exact, "shape");
        assert_eq!(shape.kind, SymbolKind::Method);
        assert!(shape.signature.contains("Shaper::shape(&mut self, text: &str) -> String"), "{}", shape.signature);
        assert_eq!(find(&exact, "helper").kind, SymbolKind::Function);
        assert!(exact.symbols.iter().all(|s| s.name != "hidden_test_fn"));
        let py = extract_exact("a.py", "class A:\n    def m(self, x):\n        return x\n\ndef top(a):\n    pass\n").unwrap();
        assert_eq!(find(&py, "m").signature, "A.m(self, x)");
        let ts = extract_exact("a.ts", "export class S { get(id: string): number { return 1; } }\nexport const f = (a: number) => a;\ninterface I { x: number }\n").unwrap();
        assert_eq!(find(&ts, "get").kind, SymbolKind::Method);
        assert_eq!(find(&ts, "f").kind, SymbolKind::Function);
        assert_eq!(find(&ts, "I").kind, SymbolKind::Trait);
        let js = extract_exact("a.js", "function a() {}\n").unwrap();
        assert_eq!(find(&js, "a").kind, SymbolKind::Function);
        assert!(extract_exact("a.md", "# x").is_none());
        assert!(extract_exact("a.rs", "\u{0}}}{{ fn").is_some());
    }

    #[test]
    fn rust_extraction() {
        let m = extract("src/lib.rs", RUST);
        assert_eq!(m.language, Language::Rust);
        assert_eq!(find(&m, "Shaper").kind, SymbolKind::Struct);
        assert_eq!(find(&m, "Mode").kind, SymbolKind::Enum);
        assert_eq!(find(&m, "Fold").kind, SymbolKind::Trait);
        let shape = find(&m, "shape");
        assert_eq!(shape.kind, SymbolKind::Method);
        assert_eq!(
            shape.signature,
            "pub Shaper::shape(&mut self, text: &str) -> String"
        );
        assert_eq!(shape.visibility, Visibility::Public);
        assert_eq!(find(&m, "private_one").visibility, Visibility::Private);
        assert_eq!(find(&m, "fold").kind, SymbolKind::Method);
        let helper = find(&m, "helper");
        assert_eq!(helper.kind, SymbolKind::Function);
        assert_eq!(helper.signature, "pub(crate) fn helper<T>(x: T) -> T");
        assert!(shape.end_line.unwrap() > shape.line);
        assert_eq!(find(&m, "LIMIT").signature, "pub const LIMIT: usize");
        assert_eq!(find(&m, "NAME").kind, SymbolKind::Static);
        assert_eq!(find(&m, "Alias").kind, SymbolKind::Type);
        assert_eq!(find(&m, "deep").kind, SymbolKind::Function);
        assert!(m
            .symbols
            .iter()
            .any(|s| s.kind == SymbolKind::Impl && s.name == "Shaper"));
        assert!(m.symbols.iter().all(|s| s.name != "not_a_symbol"
            && s.name != "fake"
            && s.name != "hidden_test_fn"
            && s.name != "tests"));
    }

    #[test]
    fn typescript_extraction() {
        let src = r#"
export interface Opts { a: number }
export type Id = string;
export enum Color { Red, Green }
export function load(path: string, opts?: Opts): Promise<string> {
  return fetch(path);
}
export const add = (a: number, b: number): number => a + b;
const local = async (x: string) => {
  return x;
};
export class Store<T> extends Base {
  private items: T[] = [];
  constructor(private name: string) { super(); }
  async get(id: Id): Promise<T> {
    if (id) { return this.items[0]; }
  }
  private secret(): void {}
  handler = (e: Event) => { console.log(e); };
}
export default function () {}
"#;
        let m = extract("a/store.ts", src);
        assert_eq!(m.language, Language::TypeScript);
        assert_eq!(find(&m, "Opts").kind, SymbolKind::Trait);
        assert_eq!(find(&m, "Id").kind, SymbolKind::Type);
        assert_eq!(find(&m, "Color").kind, SymbolKind::Enum);
        assert_eq!(
            find(&m, "load").signature,
            "export function load(path: string, opts?: Opts): Promise<string>"
        );
        assert_eq!(find(&m, "add").kind, SymbolKind::Function);
        assert_eq!(find(&m, "local").visibility, Visibility::Private);
        assert_eq!(find(&m, "Store").kind, SymbolKind::Class);
        let get = find(&m, "get");
        assert_eq!(get.kind, SymbolKind::Method);
        assert!(
            get.signature.starts_with("Store.get(id: Id)"),
            "{}",
            get.signature
        );
        assert_eq!(find(&m, "secret").visibility, Visibility::Private);
        assert_eq!(find(&m, "handler").kind, SymbolKind::Method);
        assert_eq!(find(&m, "constructor").kind, SymbolKind::Method);
        assert!(m
            .symbols
            .iter()
            .all(|s| s.name != "if" && s.name != "console"));
        assert!(m.symbols.iter().any(|s| s.name == "default"));
    }

    #[test]
    fn javascript_extraction() {
        let m = extract("x.js", "function a(x) {\n  return 1;\n}\nclass B {\n  run() {}\n}\nconst f = function () {};\n");
        assert_eq!(m.language, Language::JavaScript);
        assert_eq!(find(&m, "a").end_line, Some(3));
        assert_eq!(find(&m, "run").signature, "B.run()");
        assert_eq!(find(&m, "f").kind, SymbolKind::Function);
    }

    #[test]
    fn python_extraction() {
        let src = r#"
import os

MAX_SIZE = 10

@dataclass
class Account(Base):
    """Doc with def fake(self): inside."""
    def __init__(self, name: str):
        self.name = name

    @property
    def balance(self) -> int:
        def inner():
            pass
        return 1

    async def _refresh(self,
                       force: bool = False) -> None:
        pass

def top(a, b="x: y"):
    return a
"#;
        let m = extract("acct.py", src);
        assert_eq!(find(&m, "MAX_SIZE").kind, SymbolKind::Const);
        assert_eq!(find(&m, "Account").signature, "class Account(Base)");
        let init = find(&m, "__init__");
        assert_eq!(init.kind, SymbolKind::Method);
        assert_eq!(init.signature, "Account.__init__(self, name: str)");
        assert_eq!(init.visibility, Visibility::Public);
        assert_eq!(
            find(&m, "balance").signature,
            "Account.balance(self) -> int"
        );
        let r = find(&m, "_refresh");
        assert_eq!(r.visibility, Visibility::Private);
        assert_eq!(
            r.signature,
            "Account._refresh(self, force: bool = False) -> None"
        );
        assert_eq!(find(&m, "top").signature, "def top(a, b=\"x: y\")");
        assert!(m
            .symbols
            .iter()
            .all(|s| s.name != "inner" && s.name != "fake"));
        assert!(find(&m, "Account").end_line.unwrap() > find(&m, "_refresh").line);
    }

    #[test]
    fn markdown_toml_json_extraction() {
        let md = extract(
            "README.md",
            "# Title\n\ntext\n```\n# not a heading\n```\n## Part two ##\n",
        );
        assert_eq!(
            md.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Title", "Part two"]
        );
        assert_eq!(md.symbols[1].line, 7);
        let toml = extract("Cargo.toml", "name = \"x\"\nversion = \"1\"\n\n[dependencies]\nserde = \"1\"\n\n[[bin]]\nname = \"b\"\n");
        assert_eq!(
            toml.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["name", "version", "dependencies", "bin"]
        );
        let json = extract("package.json", "{\n  \"name\": \"x\",\n  \"scripts\": { \"build\": \"tsc\" },\n  \"files\": [1, 2],\n  \"n\": 3\n}\n");
        assert_eq!(
            json.symbols
                .iter()
                .map(|s| s.signature.as_str())
                .collect::<Vec<_>>(),
            vec![
                "name: string",
                "scripts: object",
                "files: array",
                "n: number"
            ]
        );
        assert_eq!(json.symbols[1].line, 3);
        assert!(extract("data.json", "[{\"a\":1}]").symbols.is_empty());
        assert!(extract("notes.txt", "fn x() {}").symbols.is_empty());
    }

    #[test]
    fn garbage_never_panics() {
        let huge_line = "x".repeat(300_000);
        let huge_sig = format!("fn {}(", "a".repeat(100_000));
        let samples: Vec<String> = vec![
            String::new(),
            "\u{0}\u{1}\u{2}\u{ff}".into(),
            "}}}}{{{{ )))) (((( \"\"\" ''' ``` /* // #".into(),
            huge_line,
            huge_sig,
            "fn ".into(),
            "impl".into(),
            "impl<".into(),
            "pub(".into(),
            "class \n def \n @".into(),
            "{\"a".into(),
            "{\"a\\".into(),
            "[[[[".into(),
            "'\\".into(),
            "é\u{1F600}fn é() {é}".into(),
            "\r\n\r\n#\r\n".into(),
        ];
        for s in &samples {
            for p in [
                "a.rs", "a.ts", "a.js", "a.py", "a.md", "a.toml", "a.json", "a.bin",
            ] {
                let m = extract(p, s);
                for sym in &m.symbols {
                    assert!(sym.signature.chars().count() <= MAX_SIGNATURE_CHARS);
                }
            }
        }
    }

    #[test]
    fn long_signatures_are_cut() {
        let m = extract(
            "a.rs",
            &format!("pub fn f({}) {{}}\n", "a: u8, ".repeat(100)),
        );
        let s = &m.symbols[0].signature;
        assert_eq!(s.chars().count(), MAX_SIGNATURE_CHARS);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn wanted_rules() {
        assert!(wanted("src/main.rs"));
        assert!(wanted("README.md"));
        assert!(!wanted("target/debug/x.rs"));
        assert!(!wanted("a/node_modules/x/index.js"));
        assert!(!wanted(".git/config"));
        assert!(!wanted("dist/app.js"));
        assert!(!wanted("web\\build\\app.js"));
        assert!(!wanted("Cargo.lock"));
        assert!(!wanted("static/app.min.js"));
        assert!(!wanted("logo.PNG"));
        assert!(wanted_sized("a.rs", 512 * 1024));
        assert!(!wanted_sized("a.rs", 512 * 1024 + 1));
    }

    fn sources() -> Vec<(String, String)> {
        vec![
            (
                "src/shape.rs".into(),
                "pub fn shape_output(x: &str) -> String { x.into() }\npub fn other_thing() {}\n"
                    .into(),
            ),
            (
                "src/main.rs".into(),
                "fn main() { shape_output(\"a\"); }\nfn unused_entry() {}\n".into(),
            ),
            (
                "src/lib.rs".into(),
                "pub fn lib_entry() { shape_output(\"b\"); other_thing(); }\n".into(),
            ),
            (
                "README.md".into(),
                "# Readme\n\nUses shape_output.\n".into(),
            ),
            ("target/x.rs".into(), "fn ignored_thing() {}".into()),
            ("assets/logo.png".into(), "binary".into()),
        ]
    }

    #[test]
    fn references_rank_files() {
        let src = sources();
        let mut files: Vec<FileMap> = src
            .iter()
            .filter(|(p, _)| wanted(p))
            .map(|(p, t)| extract(p, t))
            .collect();
        references(&mut files, &src);
        rank(&mut files);
        assert_eq!(files[0].path, "src/shape.rs");
        assert_eq!(files[0].references, 3);
        let lib = files.iter().find(|f| f.path == "src/lib.rs").unwrap();
        assert_eq!(lib.references, 0);
    }

    #[test]
    fn build_fits_the_budget_and_summarises() {
        let c = Estimator::generic();
        let full = build(&sources(), 10_000, &c);
        assert_eq!(full.files_included, full.files_total);
        assert_eq!(full.files_total, 4);
        assert!(full.text.starts_with(
            "src/shape.rs (Rust, 2 symbols)\n  1: pub fn shape_output(x: &str) -> String\n"
        ));
        let last = full.text.lines().last().unwrap();
        assert_eq!(
            last,
            format!(
                "[repomap: 4 of 4 files, {} symbols, ~{} tokens]",
                full.symbols_included, full.tokens
            )
        );
        let small = build(&sources(), 60, &c);
        assert!(small.files_included < full.files_included, "{}", small.text);
        assert!(small.tokens <= 60, "{} {}", small.tokens, small.text);
        assert_eq!(
            c.count(&small.text),
            small
                .tokens
                .max(c.count(&small.text))
                .min(c.count(&small.text))
        );
        assert!(small.text.lines().last().unwrap().starts_with("[repomap: "));
        assert!(small.text.starts_with("src/shape.rs"));
        let tiny = build(&sources(), 1, &c);
        assert_eq!(tiny.files_included, 0);
        assert!(tiny.text.starts_with("[repomap: 0 of 4 files"));
    }

    #[test]
    fn a_file_that_does_not_fit_shows_public_symbols_then_more() {
        let mut body = String::new();
        for k in 0..30 {
            body.push_str(&format!("fn private_{k}_with_a_long_name(arg: u32) {{}}\n"));
        }
        body.push_str("pub fn the_public_one(x: u8) {}\n");
        let c = Estimator::generic();
        let r = build(&[("a.rs".to_string(), body)], 120, &c);
        assert_eq!(r.files_included, 1, "{}", r.text);
        assert!(r.text.contains("pub fn the_public_one"), "{}", r.text);
        assert!(
            r.text.contains("… ") && r.text.contains(" more\n"),
            "{}",
            r.text
        );
        assert!(r.tokens <= 120);
    }

    #[test]
    fn output_is_stable_and_order_independent() {
        let c = Estimator::generic();
        let a = build(&sources(), 400, &c);
        let b = build(&sources(), 400, &c);
        assert_eq!(a, b);
        let mut rev = sources();
        rev.reverse();
        assert_eq!(a.text, build(&rev, 400, &c).text);
        let mut rot = sources();
        rot.rotate_left(2);
        assert_eq!(a.text, build(&rot, 400, &c).text);
    }

    #[test]
    fn per_file_cap_reports_the_rest() {
        let body: String = (0..60)
            .map(|k| format!("pub fn f_{k:02}_name() {{}}\n"))
            .collect();
        let c = Estimator::generic();
        let r = build(&[("a.rs".to_string(), body)], 100_000, &c);
        assert!(r.text.contains("(Rust, 60 symbols)"));
        assert!(r.text.contains("  … 20 more\n"));
        assert_eq!(r.symbols_included, MAX_SYMBOLS_PER_FILE);
    }
}
