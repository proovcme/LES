//! Explicit font encodings only. Never interpret a private-use glyph as Unicode text.
pub(super) fn decode(font: &str, value: &str) -> Option<char> {
    let value = u16::from_str_radix(value, 16).ok()?;
    let byte = match value {
        0..=0xff => value,
        0xf000..=0xf0ff => value - 0xf000,
        _ => return None,
    };
    // Adobe Symbol encoding: unicode.org/Public/MAPPINGS/VENDORS/ADOBE/symbol.txt.
    if font.eq_ignore_ascii_case("Symbol") {
        return Some(match byte {
            0x2d => '−',
            0x53 => 'Σ',
            0x67 => 'γ',
            0xb4 => '×',
            0xb7 => '•',
            _ => return None,
        });
    }
    if font.eq_ignore_ascii_case("Wingdings") && byte == 0xa7 {
        return Some('▪');
    }
    // Technic's shipped TrueType cmap maps F025 to the named glyph "percent"
    // (the Macintosh cmap maps 0025 to the same glyph). This is a font encoding,
    // not a guess from the surrounding engineering text.
    if font.eq_ignore_ascii_case("Technic") && byte == 0x25 {
        return Some('%');
    }
    None
}

pub(super) fn bullet(font: &str, text: &str) -> Option<String> {
    let mut result = String::new();
    for ch in text.chars() {
        if (font.eq_ignore_ascii_case("Symbol") && ch == '\u{f0be}')
            || (font.eq_ignore_ascii_case("Wingdings") && ch == '\u{f0d8}')
        {
            // Adobe's horizontal arrow extender is CUS F8E7, not a Unicode dash.
            // Preserve its exact font/code rather than inventing a punctuation mark.
            result.push_str(&format!("[{font}:{:04X}]", ch as u32));
            continue;
        }
        result.push(if ('\u{e000}'..='\u{f8ff}').contains(&ch) {
            decode(font, &format!("{:04X}", ch as u32))?
        } else {
            ch
        });
    }
    Some(result)
}
