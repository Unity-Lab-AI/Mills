//! Text helpers that must match the browser.

/// encodeURIComponent
pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) { out.push(b as char) } else { out.push_str(&format!("%{b:02X}")) }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn urlencode_like_the_browser() { assert_eq!(super::urlencode("a b/ü.md"), "a%20b%2F%C3%BC.md"); }
}
