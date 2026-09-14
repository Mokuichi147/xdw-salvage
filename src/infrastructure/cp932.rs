//! Decode the Windows Japanese code page used by XDW text records.
//!
//! `encoding_rs` exposes this encoding as `SHIFT_JIS`. Its Shift_JIS
//! implementation is the WHATWG name for the Windows-31J / code page 932
//! mapping used by the records handled here.

use encoding_rs::SHIFT_JIS;

/// Decode a CP932 byte string, replacing malformed or unassigned input with
/// U+FFFD rather than failing. A page with one bad byte should still give up
/// the rest of its text.
pub fn decode(bytes: &[u8]) -> Vec<char> {
    let (text, _had_errors) = SHIFT_JIS.decode_without_bom_handling(bytes);
    text.chars().collect()
}

/// How many source bytes `decode` consumed for this character: one for ASCII,
/// U+0080, half-width katakana and the replacement character, two otherwise.
pub fn byte_len(ch: char) -> usize {
    match ch as u32 {
        0x00..=0x80 | 0xFF61..=0xFF9F | 0xFFFD => 1,
        _ => 2,
    }
}
