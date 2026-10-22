//! A small zip writer: deflate where it helps, one central directory, Unix modes (an executable entry gets 755).
use std::io::Write;

/// One file in a zip: its path inside, its bytes, executable or not, binary (deflated less hard) or text.
pub struct Entry { pub name: String, pub data: Vec<u8>, pub exec: bool, pub binary: bool }
/// A text entry (not executable).
pub fn e(name: String, data: impl Into<Vec<u8>>) -> Entry { Entry { name, data: data.into(), exec: false, binary: false } }
/// Windows line endings.
pub fn crlf(s: &str) -> String { s.replace("\r\n", "\n").replace('\n', "\r\n") }

fn crc32(b: &[u8]) -> u32 { let mut c = 0xffff_ffffu32; for &x in b { c ^= x as u32; for _ in 0..8 { c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 }; } } c ^ 0xffff_ffff }
/// The entries as a zip file.
pub fn zip(entries: &[Entry]) -> Vec<u8> {
    use chrono::{Datelike, Timelike};
    let d = chrono::Local::now();
    let time = ((d.hour() << 11) | (d.minute() << 5) | (d.second() >> 1)) as u16; let date = (((d.year() - 1980) as u32) << 9 | d.month() << 5 | d.day()) as u16;
    let (mut out, mut central, mut offset) = (vec![], vec![], 0u32);
    for en in entries {
        let mut z = flate2::write::DeflateEncoder::new(vec![], flate2::Compression::new(if en.binary { 6 } else { 9 })); let _ = z.write_all(&en.data); let def = z.finish().unwrap_or_default();
        let (method, body): (u16, &[u8]) = if def.len() < en.data.len() { (8, &def) } else { (0, &en.data) };
        let crc = crc32(&en.data); let name = en.name.as_bytes();
        let mut l = vec![]; for v in [0x04034b50u32] { l.extend(v.to_le_bytes()); } for v in [20u16, 0x0800, method, time, date] { l.extend(v.to_le_bytes()); }
        for v in [crc, body.len() as u32, en.data.len() as u32] { l.extend(v.to_le_bytes()); } for v in [name.len() as u16, 0] { l.extend(v.to_le_bytes()); }
        let mut c = vec![]; c.extend(0x02014b50u32.to_le_bytes()); for v in [0x031eu16, 20, 0x0800, method, time, date] { c.extend(v.to_le_bytes()); }
        for v in [crc, body.len() as u32, en.data.len() as u32] { c.extend(v.to_le_bytes()); } for v in [name.len() as u16, 0, 0, 0, 0] { c.extend(v.to_le_bytes()); }
        c.extend((((if en.exec { 0o100755u32 } else { 0o100644 }) << 16)).to_le_bytes()); c.extend(offset.to_le_bytes());
        offset += (l.len() + name.len() + body.len()) as u32;
        out.extend(l); out.extend(name); out.extend(body); central.extend(c); central.extend(name);
    }
    let cd_len = central.len() as u32; out.extend(central);
    out.extend(0x06054b50u32.to_le_bytes()); for v in [0u16, 0, entries.len() as u16, entries.len() as u16] { out.extend(v.to_le_bytes()); }
    out.extend(cd_len.to_le_bytes()); out.extend(offset.to_le_bytes()); out.extend(0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_zip_starts_and_ends_right() {
        let z = super::zip(&[super::e("a/b.txt".into(), "hello hello hello hello"), super::Entry { name: "a/run".into(), data: b"#!/bin/sh\n".to_vec(), exec: true, binary: false }]);
        assert_eq!(&z[..4], &[0x50, 0x4b, 0x03, 0x04]);
        assert!(z.windows(4).any(|w| w == [0x50, 0x4b, 0x05, 0x06]));
        assert_eq!(super::crc32(b"123456789"), 0xcbf4_3926);
    }
}
