//! Resolve paragraph outline levels from the document's own style definitions.
use super::{attribute, BTreeMap, Cursor, Read, Reader, ZipArchive, MAX_XML};
use quick_xml::events::Event;

#[derive(Default)]
struct Style {
    parent: Option<String>,
    level: Option<usize>,
}

pub(super) fn outline(value: &str) -> Option<usize> {
    match value.parse::<usize>().ok()? {
        9 => Some(0),
        n @ 0..=8 => Some((n + 1).min(6)),
        _ => None,
    }
}

pub(super) fn builtin(value: &str) -> Option<usize> {
    match value {
        "Normal" | "BodyText" => Some(0),
        "Title" => Some(1),
        _ => value
            .strip_prefix("Heading")?
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=9).contains(n))
            .map(|n| n.min(6)),
    }
}

pub(super) fn read(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Option<BTreeMap<String, usize>> {
    let member = match archive.by_name("word/styles.xml") {
        Ok(member) => member,
        Err(zip::result::ZipError::FileNotFound) => return Some(BTreeMap::new()),
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
    let mut reader = Reader::from_reader(xml.as_slice());
    let mut styles = BTreeMap::<String, Style>::new();
    let mut current = None;
    let mut paragraph_properties = false;
    loop {
        match reader.read_event().ok()? {
            Event::Start(element) | Event::Empty(element) => match element.name().as_ref() {
                "w:style" => {
                    current = if attribute(&element, "w:type").as_deref() == Some("paragraph") {
                        let id = attribute(&element, "w:styleId")?;
                        if styles.len() >= 4096 || styles.contains_key(&id) {
                            return None;
                        }
                        styles.insert(id.clone(), Style::default());
                        Some(id)
                    } else {
                        None
                    };
                }
                "w:pPr" => paragraph_properties = true,
                "w:outlineLvl" if paragraph_properties && current.is_some() => {
                    styles.get_mut(current.as_ref()?)?.level =
                        Some(outline(&attribute(&element, "w:val")?)?);
                }
                "w:basedOn" if current.is_some() => {
                    styles.get_mut(current.as_ref()?)?.parent = Some(attribute(&element, "w:val")?);
                }
                _ => {}
            },
            Event::End(element) => match element.name().as_ref() {
                "w:style" => {
                    current = None;
                    paragraph_properties = false;
                }
                "w:pPr" => paragraph_properties = false,
                _ => {}
            },
            Event::DocType(_) => return None,
            Event::Eof => break,
            _ => {}
        }
    }
    let resolve = |id: &str| -> Option<usize> {
        let mut current = id;
        for _ in 0..64 {
            let Some(style) = styles.get(current) else {
                return builtin(current);
            };
            if let Some(level) = style.level {
                return Some(level);
            }
            let Some(parent) = style.parent.as_deref() else {
                return Some(builtin(current).unwrap_or(0));
            };
            current = parent;
        }
        None
    };
    Some(
        styles
            .keys()
            .filter_map(|id| resolve(id).map(|level| (id.clone(), level)))
            .collect(),
    )
}
