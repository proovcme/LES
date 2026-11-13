//! Resolve displayed OOXML list labels without executing fields or guessing numbers.
use super::{attribute, BTreeMap, Cursor, Read, Reader, ZipArchive, MAX_XML};
use quick_xml::events::Event;

#[derive(Clone, Default)]
struct Level {
    start: u32,
    format: String,
    template: String,
    restart: Option<usize>,
    legal: bool,
    picture: bool,
    font: String,
}
#[derive(Default)]
pub(super) struct Numbering {
    levels: BTreeMap<String, BTreeMap<usize, Level>>,
    counters: BTreeMap<String, [Option<u32>; 9]>,
}
impl Numbering {
    pub(super) fn fresh(&self) -> Self {
        Self {
            levels: self.levels.clone(),
            counters: BTreeMap::new(),
        }
    }
    pub(super) fn read(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Option<Self> {
        let member = match archive.by_name("word/numbering.xml") {
            Ok(member) => member,
            Err(zip::result::ZipError::FileNotFound) => return Some(Self::default()),
            Err(_) => return None,
        };
        if member.size() > MAX_XML {
            return None;
        }
        let mut xml = Vec::new();
        member.take(MAX_XML + 1).read_to_end(&mut xml).ok()?;
        if xml.len() as u64 > MAX_XML {
            return None;
        }
        let mut definitions = BTreeMap::<String, BTreeMap<usize, Level>>::new();
        let mut instances = BTreeMap::<String, String>::new();
        let mut overrides = BTreeMap::<String, BTreeMap<usize, Level>>::new();
        let mut starts = BTreeMap::<(String, usize), u32>::new();
        let mut abstract_id = None::<String>;
        let mut num_id = None::<String>;
        let mut override_level = None;
        let mut current = None::<(usize, Level)>;
        let mut reader = Reader::from_reader(xml.as_slice());
        reader.config_mut().expand_empty_elements = true;
        loop {
            match reader.read_event().ok()? {
                Event::Start(e) => match e.name().as_ref() {
                    "w:abstractNum" => {
                        if definitions.len() >= 4096 {
                            return None;
                        }
                        abstract_id = Some(attribute(&e, "w:abstractNumId")?);
                    }
                    "w:num" => {
                        if instances.len() >= 4096 {
                            return None;
                        }
                        num_id = Some(attribute(&e, "w:numId")?);
                    }
                    "w:abstractNumId" if num_id.is_some() => {
                        instances.insert(num_id.clone()?, attribute(&e, "w:val")?);
                    }
                    "w:lvlOverride" => override_level = Some(level(&attribute(&e, "w:ilvl")?)?),
                    "w:startOverride" => {
                        starts.insert(
                            (num_id.clone()?, override_level?),
                            attribute(&e, "w:val")?.parse().ok()?,
                        );
                    }
                    "w:lvl" => {
                        if current.is_some() {
                            return None;
                        }
                        current = Some((
                            level(&attribute(&e, "w:ilvl")?)?,
                            Level {
                                start: 1,
                                ..Level::default()
                            },
                        ));
                    }
                    "w:start" if current.is_some() => {
                        current.as_mut()?.1.start = attribute(&e, "w:val")?.parse().ok()?
                    }
                    "w:numFmt" if current.is_some() => {
                        current.as_mut()?.1.format = attribute(&e, "w:val")?
                    }
                    "w:isLgl" if current.is_some() => {
                        current.as_mut()?.1.legal = !matches!(
                            attribute(&e, "w:val").as_deref(),
                            Some("0" | "false" | "off")
                        )
                    }
                    "w:lvlPicBulletId" if current.is_some() => current.as_mut()?.1.picture = true,
                    "w:lvlText" if current.is_some() => {
                        current.as_mut()?.1.template = attribute(&e, "w:val")?
                    }
                    "w:rFonts" if current.is_some() => {
                        current.as_mut()?.1.font = attribute(&e, "w:ascii")
                            .or_else(|| attribute(&e, "w:hAnsi"))
                            .unwrap_or_default();
                    }
                    "w:lvlRestart" if current.is_some() => {
                        current.as_mut()?.1.restart = Some(attribute(&e, "w:val")?.parse().ok()?)
                    }
                    _ => {}
                },
                Event::End(e) => match e.name().as_ref() {
                    "w:lvl" => {
                        let (index, value) = current.take()?;
                        let target = if let Some(id) = &num_id {
                            overrides.entry(id.clone()).or_default()
                        } else {
                            definitions.entry(abstract_id.clone()?).or_default()
                        };
                        if target.insert(index, value).is_some() {
                            return None;
                        }
                    }
                    "w:abstractNum" => abstract_id = None,
                    "w:num" => num_id = None,
                    "w:lvlOverride" => override_level = None,
                    _ => {}
                },
                Event::DocType(_) => return None,
                Event::Eof => break,
                _ => {}
            }
        }
        let mut result = Self::default();
        for (id, abstract_id) in instances {
            let Some(mut levels) = definitions.get(&abstract_id).cloned() else {
                continue;
            };
            if let Some(override_values) = overrides.remove(&id) {
                levels.extend(override_values);
            }
            for ((num, index), start) in &starts {
                if num == &id {
                    levels.get_mut(index)?.start = *start;
                }
            }
            result.levels.insert(id, levels);
        }
        Some(result)
    }

    pub(super) fn next(&mut self, id: &str, index: usize) -> Option<String> {
        if id == "0" {
            return Some(String::new());
        }
        let levels = self.levels.get(id)?;
        let current = levels.get(&index)?;
        if current.picture {
            return None;
        }
        let counters = self.counters.entry(id.into()).or_insert([None; 9]);
        counters[index] = Some(counters[index].map_or(Some(current.start), |v| v.checked_add(1))?);
        for (&child, definition) in levels.range(index + 1..) {
            let restart = definition.restart.unwrap_or(child);
            if restart > 0 && index < restart {
                counters[child] = None;
            }
        }
        if current.format == "bullet" {
            return super::symbols::bullet(&current.font, &current.template);
        }
        let mut label = current.template.clone();
        for n in 0..9 {
            let token = format!("%{}", n + 1);
            if label.contains(&token) {
                let definition = levels.get(&n)?;
                label = label.replace(
                    &token,
                    &number(
                        counters[n].unwrap_or(definition.start),
                        if current.legal {
                            "decimal"
                        } else {
                            &definition.format
                        },
                    )?,
                );
            }
        }
        Some(label)
    }
}
pub(super) fn level(value: &str) -> Option<usize> {
    value.parse::<usize>().ok().filter(|n| *n < 9)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legal_labels_use_decimal_ancestors_and_picture_markers_are_not_dropped() {
        let mut lists = Numbering::default();
        lists.levels.insert(
            "n".into(),
            BTreeMap::from([
                (
                    0,
                    Level {
                        start: 5,
                        format: "upperLetter".into(),
                        template: "%1.".into(),
                        ..Default::default()
                    },
                ),
                (
                    1,
                    Level {
                        start: 1,
                        format: "lowerLetter".into(),
                        template: "%1.%2".into(),
                        legal: true,
                        ..Default::default()
                    },
                ),
                (
                    2,
                    Level {
                        picture: true,
                        ..Default::default()
                    },
                ),
            ]),
        );
        assert_eq!(lists.next("n", 0).as_deref(), Some("E."));
        assert_eq!(lists.next("n", 1).as_deref(), Some("5.1"));
        assert_eq!(lists.next("n", 2), None);
        assert_eq!(lists.next("n", 0).as_deref(), Some("F."));
        assert_eq!(lists.next("n", 1).as_deref(), Some("6.1"));
    }
}
fn number(value: u32, format: &str) -> Option<String> {
    match format {
        "decimal" => Some(value.to_string()),
        "decimalZero" => Some(format!("{value:02}")),
        "none" => Some(String::new()),
        "russianLower" | "russianUpper" if (1..=29 * 1024).contains(&value) => {
            // ISO/IEC 29500-1 17.18.59: a 29-letter set; repeat the SAME
            // letter after 29, rather than using positional base-29 notation.
            let alphabet = "абвгдежзиклмнопрстуфхцчшщыэюя";
            let ch = alphabet.chars().nth(((value - 1) % 29) as usize)?;
            let label = ch.to_string().repeat(((value - 1) / 29 + 1) as usize);
            Some(if format == "russianUpper" {
                label.to_uppercase()
            } else {
                label
            })
        }
        "upperLetter" | "lowerLetter" if value > 0 && value <= 1_000_000 => {
            let mut value = value;
            let mut result = String::new();
            while value > 0 {
                value -= 1;
                result.insert(0, (b'A' + (value % 26) as u8) as char);
                value /= 26;
            }
            Some(if format == "lowerLetter" {
                result.to_lowercase()
            } else {
                result
            })
        }
        "upperRoman" | "lowerRoman" if (1..4000).contains(&value) => {
            let mut value = value;
            let mut result = String::new();
            for (n, symbol) in [
                (1000, "M"),
                (900, "CM"),
                (500, "D"),
                (400, "CD"),
                (100, "C"),
                (90, "XC"),
                (50, "L"),
                (40, "XL"),
                (10, "X"),
                (9, "IX"),
                (5, "V"),
                (4, "IV"),
                (1, "I"),
            ] {
                while value >= n {
                    value -= n;
                    result.push_str(symbol);
                }
            }
            Some(if format == "lowerRoman" {
                result.to_lowercase()
            } else {
                result
            })
        }
        _ => None,
    }
}
