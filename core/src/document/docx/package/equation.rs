//! Read bounded MTEF, using the published Design Science format specification.
//! No Equation Editor / OLE activation, and no OCR substitution for operators.
use super::Result;
const BAD: &str = "DOCX_EQUATION_UNSUPPORTED";
pub(super) fn read(raw: &[u8]) -> Result<String> {
    if raw.len() < 33 || raw.len() > 1024 * 1024 {
        return Err(BAD);
    }
    let header = u16::from_le_bytes(raw[..2].try_into().unwrap()) as usize;
    let size = u32::from_le_bytes(raw[8..12].try_into().unwrap()) as usize;
    if header != 28 || header.checked_add(size) != Some(raw.len()) {
        return Err(BAD);
    }
    let bytes = &raw[header..];
    if !matches!(bytes[0], 3 | 5) {
        eprintln!(
            "{}",
            serde_json::json!({"event":"mtef_version_unsupported","version":bytes[0]})
        );
        return Err(BAD);
    }
    let mut reader = Mtef {
        bytes,
        at: 5,
        records: 0,
        version: bytes[0],
    };
    if reader.version == 5 {
        reader.string()?;
        if reader.byte()? > 1 {
            return Err(BAD);
        }
    }
    let parsed = reader.list(0);
    if parsed.is_err() {
        eprintln!(
            "{}",
            serde_json::json!({"event":"mtef_parse_failed","offset":reader.at})
        );
    }
    let text = parsed?.concat();
    if reader.at != bytes.len() || text.trim().is_empty() {
        return Err(BAD);
    }
    Ok(text)
}
struct Mtef<'a> {
    bytes: &'a [u8],
    at: usize,
    records: usize,
    version: u8,
}
impl Mtef<'_> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self.bytes.get(self.at).ok_or(BAD)?;
        self.at += 1;
        Ok(b)
    }
    fn word(&mut self) -> Result<u16> {
        let a = self.byte()?;
        Ok(u16::from_le_bytes([a, self.byte()?]))
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.at = self
            .at
            .checked_add(n)
            .filter(|n| *n <= self.bytes.len())
            .ok_or(BAD)?;
        Ok(())
    }
    fn string(&mut self) -> Result<()> {
        for _ in 0..256 {
            if self.byte()? == 0 {
                return Ok(());
            }
        }
        Err(BAD)
    }
    fn unsigned(&mut self) -> Result<u16> {
        let n = self.byte()?;
        if n == 255 {
            self.word()
        } else {
            Ok(n as u16)
        }
    }
    fn dimensions(&mut self) -> Result<()> {
        let count = self.byte()? as usize;
        let mut ended = 0;
        for _ in 0..4096 {
            if ended == count {
                return Ok(());
            }
            let byte = self.byte()?;
            for nibble in [byte >> 4, byte & 15] {
                if nibble == 15 {
                    ended += 1;
                }
                if ended == count {
                    break;
                }
            }
        }
        Err(BAD)
    }
    fn list(&mut self, depth: usize) -> Result<Vec<String>> {
        if depth > 48 {
            return Err(BAD);
        }
        let mut values = Vec::<String>::new();
        loop {
            self.records += 1;
            if self.records > 100000 {
                return Err(BAD);
            }
            let tag = self.byte()?;
            let kind = if self.version == 5 { tag } else { tag & 15 };
            let flags = if self.version == 5 {
                if matches!(kind, 1..=6) {
                    self.byte()?
                } else {
                    0
                }
            } else {
                tag >> 4
            };
            if kind == 0 {
                if flags != 0 {
                    return Err(BAD);
                }
                return Ok(values);
            }
            if flags & 8 != 0 {
                let x = self.byte()?;
                let y = self.byte()?;
                if x == 128 && y == 128 {
                    self.skip(4)?;
                }
            }
            let value = match kind {
                1 => {
                    if flags & 4 != 0 {
                        self.word()?;
                    }
                    if flags & 2 != 0 {
                        self.ruler()?;
                    }
                    if flags & 1 != 0 {
                        String::new()
                    } else {
                        self.list(depth + 1)?.concat()
                    }
                }
                2 => {
                    let face = self.byte()?;
                    if self.version == 5
                        && (face == 255 || flags & 0x20 != 0 || flags & 0x14 == 0x14)
                    {
                        return Err(BAD);
                    }
                    let ch = self.word()?;
                    if self.version == 5 {
                        if flags & 4 != 0 {
                            self.byte()?;
                        }
                        if flags & 16 != 0 {
                            self.word()?;
                        }
                    }
                    let mut text = character(face, ch)?.to_string();
                    if flags & if self.version == 5 { 1 } else { 2 } != 0 {
                        for embell in self.embellishments()? {
                            text = embellish(embell, &text)?;
                        }
                    }
                    text
                }
                3 => {
                    let selector = self.byte()?;
                    let mut variation = self.byte()? as u16;
                    if self.version == 5 && variation & 128 != 0 {
                        variation = (variation & 127) | ((self.byte()? as u16) << 8);
                    }
                    let options = self.byte()?;
                    let args = self.list(depth + 1)?;
                    if self.version == 5 {
                        template5(selector, variation, options, &args)?
                    } else {
                        template(selector, variation as u8, options, &args)?
                    }
                }
                4 => {
                    self.skip(2)?;
                    if flags & 2 != 0 {
                        self.ruler()?;
                    }
                    self.list(depth + 1)?.join("\n")
                }
                5 => {
                    self.skip(3)?;
                    let rows = self.byte()? as usize;
                    let cols = self.byte()? as usize;
                    if rows == 0 || cols == 0 || rows * cols > 4096 {
                        return Err(BAD);
                    }
                    self.skip((rows + 4) / 4 + (cols + 4) / 4)?;
                    let cells = self.list(depth + 1)?;
                    if cells.len() != rows * cols {
                        return Err(BAD);
                    }
                    format!(
                        "matrix[{}]",
                        cells
                            .chunks(cols)
                            .map(|r| r.join(", "))
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                }
                8 => {
                    if self.version == 5 {
                        self.unsigned()?;
                        self.byte()?;
                    } else {
                        self.skip(2)?;
                        self.string()?;
                    }
                    continue;
                }
                9 => {
                    match self.byte()? {
                        101 => self.skip(2)?,
                        100 => self.skip(3)?,
                        _ => self.skip(1)?,
                    };
                    continue;
                }
                10..=14 => continue,
                15 if self.version == 5 => {
                    self.unsigned()?;
                    continue;
                }
                16 if self.version == 5 => {
                    let options = self.byte()?;
                    if options & !7 != 0 {
                        return Err(BAD);
                    }
                    self.skip(if options & 1 != 0 { 8 } else { 6 })?;
                    if options & 4 != 0 {
                        self.string()?;
                    }
                    continue;
                }
                17 if self.version == 5 => {
                    self.unsigned()?;
                    self.string()?;
                    continue;
                }
                18 if self.version == 5 => {
                    if self.byte()? != 0 {
                        return Err(BAD);
                    }
                    self.dimensions()?;
                    self.dimensions()?;
                    let count = self.byte()?;
                    for _ in 0..count {
                        if self.unsigned()? != 0 {
                            self.byte()?;
                        }
                    }
                    continue;
                }
                19 if self.version == 5 => {
                    self.string()?;
                    continue;
                }
                _ => return Err(BAD),
            };
            values.push(value);
        }
    }
    fn ruler(&mut self) -> Result<()> {
        if self.byte()? != 7 {
            return Err(BAD);
        }
        let count = self.byte()? as usize;
        self.skip(count * 3)
    }
    fn embellishments(&mut self) -> Result<Vec<u8>> {
        let mut result = Vec::new();
        loop {
            let tag = self.byte()?;
            if tag == 0 {
                return Ok(result);
            }
            if (if self.version == 5 { tag } else { tag & 15 }) != 6 || result.len() > 16 {
                return Err(BAD);
            }
            let flags = if self.version == 5 {
                self.byte()?
            } else {
                tag >> 4
            };
            if flags & 8 != 0 {
                let a = self.byte()?;
                let b = self.byte()?;
                if a == 128 && b == 128 {
                    self.skip(4)?;
                }
            }
            result.push(self.byte()?);
        }
    }
}
fn character(face: u8, ch: u16) -> Result<char> {
    if face < 128 {
        eprintln!(
            "{}",
            serde_json::json!({"event":"mtef_explicit_font","face":face,"code":ch})
        );
        return Err(BAD);
    }
    if ch < 32 && ch != 9 {
        return Err(BAD);
    }
    if (0xe000..=0xf8ff).contains(&ch) {
        eprintln!(
            "{}",
            serde_json::json!({"event":"mtef_private_character","face":face,"code":ch})
        );
        return Err(BAD);
    }
    // MTEF's Windows Unicode characters are retained; explicit symbolic-font
    // positions whose encoding is not established must not be guessed.
    char::from_u32(ch as u32).ok_or(BAD)
}
fn embellish(kind: u8, text: &str) -> Result<String> {
    let s = match kind {
        2 => format!("dot({text})"),
        3 => format!("ddot({text})"),
        4 => format!("tdot({text})"),
        5 => format!("{text}′"),
        6 => format!("{text}″"),
        7 => format!("‵{text}"),
        8 => format!("tilde({text})"),
        9 => format!("hat({text})"),
        10 => format!("not({text})"),
        11 => format!("vector_right({text})"),
        12 => format!("vector_left({text})"),
        13 => format!("vector_both({text})"),
        16 => format!("midbar({text})"),
        17 => format!("overline({text})"),
        18 => format!("{text}‴"),
        _ => return Err(BAD),
    };
    Ok(s)
}
fn template(s: u8, v: u8, o: u8, a: &[String]) -> Result<String> {
    let get = |n: usize| a.get(n).map(String::as_str).ok_or(BAD);
    let result = match s {
        13 if o == 0 && a.len() == 2 => match v {
            0 if get(1)?.is_empty() => format!("sqrt({})", get(0)?),
            1 => format!("root({};{})", get(1)?, get(0)?),
            _ => return Err(BAD),
        },
        14 | 41 if o == 0 && v <= 2 && a.len() == 2 => format!("({})/({})", get(0)?, get(1)?),
        15 if o == 0 && a.len() == 2 => match v {
            0 if get(0)?.is_empty() => format!("^({})", get(1)?),
            1 if get(1)?.is_empty() => format!("_({})", get(0)?),
            2 => format!("_({})^({})", get(0)?, get(1)?),
            _ => return Err(BAD),
        },
        0..=12 if o <= 2 && !a.is_empty() => {
            // Fence glyphs are explicit subobjects, not assumed parentheses.
            if a.len() > 3 {
                return Err(BAD);
            }
            format!(
                "{}{}{}",
                a.get(1).map(String::as_str).unwrap_or(""),
                get(0)?,
                a.get(2).map(String::as_str).unwrap_or("")
            )
        }
        16 | 17 if o == 0 && v <= 1 && a.len() == 1 => format!(
            "{}{}({})",
            if v == 1 { "double_" } else { "" },
            if s == 16 { "underline" } else { "overline" },
            get(0)?
        ),
        27..=38 | 42 | 43 if a.len() == 4 => {
            format!("{}_({})^({})({})", get(3)?, get(2)?, get(1)?, get(0)?)
        }
        _ => {
            eprintln!(
                "{}",
                serde_json::json!({"event":"mtef_unsupported_template","selector":s,"variation":v,"options":o})
            );
            return Err(BAD);
        }
    };
    Ok(result)
}

fn template5(s: u8, v: u16, o: u8, a: &[String]) -> Result<String> {
    match s {
        0..=9 if v <= 0x33 => template(s, 0, o, a),
        10 if v <= 1 => template(13, v as u8, o, a),
        11 if v <= 7 => template(14, 0, o, a),
        12 | 13 if v <= 1 => template(s + 4, v as u8, o, a),
        27..=29 if v <= 1 && o == 0 && a.len() == 2 => {
            let sub = &a[0];
            let sup = &a[1];
            if (s == 27 && !sup.is_empty()) || (s == 28 && !sub.is_empty()) {
                return Err(BAD);
            }
            Ok(format!(
                "{}{}{}",
                if v == 1 { "prescript" } else { "" },
                if sub.is_empty() {
                    String::new()
                } else {
                    format!("_({sub})")
                },
                if sup.is_empty() {
                    String::new()
                } else {
                    format!("^({sup})")
                }
            ))
        }
        _ => {
            eprintln!(
                "{}",
                serde_json::json!({"event":"mtef5_unsupported_template","selector":s,"variation":v,"options":o})
            );
            Err(BAD)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn native(bytes: &[u8]) -> Vec<u8> {
        let mut raw = vec![0; 28];
        raw[..2].copy_from_slice(&28u16.to_le_bytes());
        raw[8..12].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        raw.extend_from_slice(bytes);
        raw
    }
    #[test]
    fn mtef3_and_5_keep_fraction_and_script_semantics_and_refuse_truncation() {
        let old = native(&[
            3, 1, 1, 3, 0, 10, 1, 2, 131, b'Q', 0, 2, 134, b'=', 0, 3, 14, 0, 0, 1, 2, 131, b'a',
            0, 0, 1, 2, 131, b'b', 0, 0, 0, 0, 0,
        ]);
        assert_eq!(read(&old).unwrap(), "Q=(a)/(b)");
        let new = native(&[
            5, 1, 0, 4, 0, b'D', b'S', b'M', b'T', b'4', 0, 0, 10, 1, 0, 2, 0, 131, b'x', 0, 3, 0,
            27, 0, 0, 1, 0, 2, 0, 136, b'1', 0, 0, 1, 1, 0, 0, 0,
        ]);
        assert_eq!(read(&new).unwrap(), "x_(1)");
        for end in 0..new.len() {
            assert!(read(&new[..end]).is_err());
        }
        assert!(read(&native(&[5, 1, 0, 4, 0, 0, 0, 1, 0, 250, 0])).is_err());
    }
}
