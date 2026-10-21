//! Unity's clock and calculator (from bridge/assistant.js): the person's own time zone, dates as people write them
//! ("in 20 minutes", "18:30", "2026-10-20 09:00"), and a small math evaluator.
use chrono::{TimeZone, Utc};
use chrono_tz::Tz;

/// A time zone name that exists (IANA), else None.
pub fn tz_ok(z: &str) -> Option<String> { if z.is_empty() || z.len() > 64 { return None; } z.parse::<Tz>().ok().map(|_| z.to_string()) }
fn tz(z: &str) -> Tz { z.parse::<Tz>().unwrap_or(chrono_tz::UTC) }
/// "Tuesday, October 21, 2026 at 2:30 AM" in their zone (as Node's en-US formatting writes it).
pub fn local_now(zone: &str) -> String { Utc::now().with_timezone(&tz(zone)).format("%A, %B %-d, %Y at %-I:%M\u{202f}%p").to_string() }
/// "Tue, Oct 21, 2:30 AM" in their zone.
pub fn fmt(ms: i64, zone: &str) -> String { match Utc.timestamp_millis_opt(ms).single() { Some(d) => d.with_timezone(&tz(zone)).format("%a, %b %-d, %-I:%M\u{202f}%p").to_string(), None => String::new() } }
/// Their local day, YYYY-MM-DD.
pub fn day_key(ms: i64, zone: &str) -> String { Utc.timestamp_millis_opt(ms).single().map(|d| d.with_timezone(&tz(zone)).format("%Y-%m-%d").to_string()).unwrap_or_default() }
fn now_ms() -> i64 { vr_core::now_ms() as i64 }

/// "in 20 minutes", an ISO time with its zone, "2026-10-16 18:00", "18:00", "6:30 pm", "2026-10-16" → epoch ms, read in
/// their time zone; a time of day already past today means tomorrow.
pub fn parse_when(text: &str, zone: &str) -> Option<i64> {
    let t = text.trim().to_lowercase();
    if let Some(c) = regex::Regex::new(r"^in\s+(\d+)\s*(second|sec|minute|min|hour|hr|day)s?$").unwrap().captures(&t) {
        let n: i64 = c[1].parse().ok()?;
        let unit = match &c[2] { "second" | "sec" => 1000, "minute" | "min" => 60_000, "hour" | "hr" => 3_600_000, _ => 86_400_000 };
        return Some(now_ms() + n * unit);
    }
    if regex::Regex::new(r"[zZ]|[+-]\d\d:?\d\d$").unwrap().is_match(text.trim()) { return any_date(text.trim()); }
    let z = tz(zone);
    let today = Utc::now().with_timezone(&z).format("%Y-%m-%d").to_string();
    let re = regex::Regex::new(r"^(\d{4}-\d{2}-\d{2})?[ t]?(\d{1,2}):(\d{2})\s*(am|pm)?$").unwrap();
    let (date, mut hh, mm, ap, dated) = if let Some(c) = re.captures(&t) {
        (c.get(1).map(|m| m.as_str().to_string()).unwrap_or(today), c[2].parse::<u32>().ok()?, c[3].parse::<u32>().ok()?, c.get(4).map(|m| m.as_str().to_string()), c.get(1).is_some())
    } else if regex::Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap().is_match(&t) { (t.clone(), 9, 0, None, true) } else { return any_date(text.trim()); };
    if ap.as_deref() == Some("pm") && hh < 12 { hh += 12; }
    if ap.as_deref() == Some("am") && hh == 12 { hh = 0; }
    let naive = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok()?.and_hms_opt(hh, mm, 0)?;
    let mut when = z.from_local_datetime(&naive).earliest()?.timestamp_millis();
    if !dated && when < now_ms() { when += 86_400_000; }
    Some(when)
}
fn any_date(t: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(t).ok().map(|d| d.timestamp_millis())
        .or_else(|| chrono::DateTime::parse_from_rfc2822(t).ok().map(|d| d.timestamp_millis()))
        .or_else(|| chrono::DateTime::parse_from_str(t, "%Y-%m-%d %H:%M%z").ok().map(|d| d.timestamp_millis()))
        .or_else(|| chrono::DateTime::parse_from_str(t, "%Y-%m-%dT%H:%M%z").ok().map(|d| d.timestamp_millis()))
}

/// numbers, + - * / % ^, parentheses, sqrt sin cos tan log ln abs round floor ceil, pi, e. Err(why).
pub fn calculate(expr: &str) -> Result<f64, String> {
    if !expr.chars().all(|c| c.is_ascii_digit() || c.is_whitespace() || "+-*/%^().,".contains(c) || c.is_ascii_alphabetic()) { return Err("Only numbers and math functions are allowed.".into()); }
    const FNS: [&str; 11] = ["sqrt", "sin", "cos", "tan", "log", "ln", "abs", "round", "floor", "ceil", "pi"];
    for w in regex::Regex::new(r"[A-Za-z]+").unwrap().find_iter(expr) { let w = w.as_str().to_lowercase(); if !(FNS.contains(&w.as_str()) || w == "e") { return Err(format!("Unknown word: {}", w)); } }
    let toks: Vec<char> = expr.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    let mut p = Calc { t: toks, i: 0 };
    let v = p.seq().ok_or("Bad expression.")?;
    if p.i != p.t.len() { return Err("Bad expression.".into()); }
    if v.is_finite() { Ok(v) } else { Err("Not a finite number.".into()) }
}
struct Calc { t: Vec<char>, i: usize }
impl Calc {
    fn peek(&self) -> Option<char> { self.t.get(self.i).copied() }
    fn eat(&mut self, c: char) -> bool { if self.peek() == Some(c) { self.i += 1; true } else { false } }
    fn seq(&mut self) -> Option<f64> { let mut v = self.add()?; while self.eat(',') { v = self.add()?; } Some(v) }
    fn add(&mut self) -> Option<f64> { let mut v = self.mul()?; loop { if self.eat('+') { v += self.mul()?; } else if self.eat('-') { v -= self.mul()?; } else { return Some(v); } } }
    fn mul(&mut self) -> Option<f64> { let mut v = self.unary()?; loop { if self.eat('*') { v *= self.unary()?; } else if self.eat('/') { v /= self.unary()?; } else if self.eat('%') { v %= self.unary()?; } else { return Some(v); } } }
    fn unary(&mut self) -> Option<f64> { if self.eat('-') { return Some(-self.unary()?); } if self.eat('+') { return self.unary(); } self.pow() }
    fn pow(&mut self) -> Option<f64> { let b = self.atom()?; if self.eat('^') { let e = self.unary()?; return Some(b.powf(e)); } Some(b) }
    fn atom(&mut self) -> Option<f64> {
        if self.eat('(') { let v = self.seq()?; return if self.eat(')') { Some(v) } else { None }; }
        let c = self.peek()?;
        if c.is_ascii_digit() || c == '.' { let s = self.i; while self.peek().map(|c| c.is_ascii_digit() || c == '.').unwrap_or(false) { self.i += 1; } return self.t[s..self.i].iter().collect::<String>().parse().ok(); }
        if c.is_ascii_alphabetic() {
            let s = self.i; while self.peek().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) { self.i += 1; }
            let w: String = self.t[s..self.i].iter().collect();
            match w.as_str() { "pi" => return Some(std::f64::consts::PI), "e" => return Some(std::f64::consts::E), _ => {} }
            if !self.eat('(') { return Some(f64::NAN); } // a function without its argument
            let x = self.seq()?; if !self.eat(')') { return None; }
            return Some(match w.as_str() { "sqrt" => x.sqrt(), "sin" => x.sin(), "cos" => x.cos(), "tan" => x.tan(), "log" => x.log10(), "ln" => x.ln(), "abs" => x.abs(), "round" => (x + 0.5).floor(), "floor" => x.floor(), "ceil" => x.ceil(), _ => f64::NAN });
        }
        None
    }
}
/// A number as JavaScript prints it in JSON (whole numbers without ".0").
pub fn num(v: f64) -> serde_json::Value { if v.fract() == 0.0 && v.abs() < 9e15 { serde_json::json!(v as i64) } else { serde_json::json!(v) } }

#[cfg(test)]
mod tests {
    #[test]
    fn calc() { assert_eq!(super::calculate("2^3*2+sqrt(16)").unwrap(), 20.0); assert!(super::calculate("2+x").is_err()); assert_eq!(super::calculate("-(1+2)%2").unwrap(), -1.0); }
    #[test]
    fn when() { let a = super::parse_when("in 2 minutes", "UTC").unwrap(); assert!((a - vr_core::now_ms() as i64 - 120_000).abs() < 2000); assert!(super::parse_when("2026-10-20 18:00", "America/Chicago").is_some()); assert!(super::parse_when("nonsense", "UTC").is_none()); }
}
