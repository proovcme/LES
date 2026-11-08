//! Presentation order comes from relationships, never ZIP order or slide filenames.
use super::{escape, Result};
use quick_xml::{
    events::{BytesStart, Event},
    Reader, XmlVersion,
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
};
type Archive<'a> = zip::ZipArchive<Cursor<&'a [u8]>>;
const INVALID: &str = "PRESENTATION_XML_INVALID";

fn attrs(node: &BytesStart<'_>) -> Result<HashMap<String, String>> {
    node.attributes()
        .map(|attribute| {
            let a = attribute.map_err(|_| INVALID)?;
            Ok((
                a.key.as_ref().to_owned(),
                a.normalized_value(XmlVersion::Implicit1_0)
                    .map_err(|_| INVALID)?
                    .into_owned(),
            ))
        })
        .collect()
}
fn content(text: &quick_xml::events::BytesText<'_>) -> Result<String> {
    Ok(text.xml_content(XmlVersion::Implicit1_0).into_owned())
}
fn events(text: &str, mut consume: impl FnMut(Event<'_>) -> Result<()>) -> Result<()> {
    let mut reader = Reader::from_str(text);
    let mut depth = 0_usize;
    loop {
        match reader.read_event().map_err(|_| INVALID)? {
            Event::Eof => return if depth == 0 { Ok(()) } else { Err(INVALID) },
            Event::DocType(_) => return Err(INVALID),
            Event::Empty(node) => {
                consume(Event::Start(node.borrow()))?;
                consume(Event::End(node.to_end()))?;
            }
            Event::Start(node) => {
                depth += 1;
                if depth > 256 {
                    return Err(INVALID);
                }
                consume(Event::Start(node))?;
            }
            Event::End(node) => {
                depth = depth.checked_sub(1).ok_or(INVALID)?;
                consume(Event::End(node))?;
            }
            Event::GeneralRef(reference) => {
                let value = if let Some(ch) = reference.resolve_char_ref().map_err(|_| INVALID)? {
                    ch.to_string()
                } else {
                    quick_xml::escape::resolve_predefined_entity(reference.as_ref())
                        .ok_or(INVALID)?
                        .to_string()
                };
                consume(Event::Text(quick_xml::events::BytesText::from_escaped(
                    &value,
                )))?;
            }
            event => consume(event)?,
        }
    }
}
fn target(base: &str, value: &str) -> Result<String> {
    if value.contains(['\\', ':', '?', '#']) || value.starts_with("//") {
        return Err("PRESENTATION_RELATION_INVALID");
    }
    let mut parts = if value.starts_with('/') {
        Vec::new()
    } else {
        base.split('/').map(str::to_owned).collect::<Vec<_>>()
    };
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or("PRESENTATION_RELATION_INVALID")?;
            }
            v => parts.push(v.into()),
        }
    }
    let path = parts.join("/");
    if !path.starts_with("ppt/") || !path.ends_with(".xml") {
        return Err("PRESENTATION_RELATION_INVALID");
    }
    Ok(path)
}
#[derive(Clone)]
struct Relation {
    path: String,
    kind: String,
}
fn relationships(
    archive: &mut Archive<'_>,
    name: &str,
    base: &str,
    required: bool,
) -> Result<HashMap<String, Relation>> {
    if !required && !archive.file_names().any(|n| n.is_ok_and(|n| n == name)) {
        return Ok(HashMap::new());
    }
    let text = super::xml(archive, name)?;
    let mut map = HashMap::new();
    events(&text, |event| {
        if let Event::Empty(node) | Event::Start(node) = event {
            if node.local_name().as_ref() == "Relationship" {
                let a = attrs(&node)?;
                if a.get("TargetMode")
                    .is_some_and(|s| s.eq_ignore_ascii_case("External"))
                {
                    return Ok(());
                }
                let id = a.get("Id").ok_or(INVALID)?;
                let kind = a.get("Type").ok_or(INVALID)?;
                // Images, OLE packages and hyperlinks are not opened as XML.
                if ["/slide", "/notesSlide", "/chart"]
                    .iter()
                    .any(|s| kind.ends_with(s))
                {
                    let path = target(base, a.get("Target").ok_or(INVALID)?)?;
                    if map
                        .insert(
                            id.clone(),
                            Relation {
                                path,
                                kind: kind.clone(),
                            },
                        )
                        .is_some()
                    {
                        return Err("PRESENTATION_RELATION_INVALID");
                    }
                }
            }
        }
        Ok(())
    })?;
    Ok(map)
}
fn table(rows: &[Vec<String>]) -> String {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        out.push(format!(
            "| {} |",
            (0..width)
                .map(|c| escape(row.get(c).map_or("", String::as_str)))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
        if i == 0 {
            out.push(format!(
                "| {} |",
                (0..width).map(|_| "---").collect::<Vec<_>>().join(" | ")
            ));
        }
    }
    out.join("\n")
}
fn slide(text: &str) -> Result<(String, Vec<String>)> {
    let mut out = Vec::new();
    let mut paragraph = String::new();
    let mut in_text = false;
    let mut title = false;
    let mut rows: Option<Vec<Vec<String>>> = None;
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut charts = Vec::new();
    let mut elements = 0_usize;
    events(text, |event| {
        match event {
            Event::Start(node) | Event::Empty(node) => {
                elements += 1;
                if elements > 500_000 {
                    return Err("PRESENTATION_LIMIT");
                }
                match node.local_name().as_ref() {
                    "sp" => title = false,
                    "ph" => {
                        title = attrs(&node)?
                            .get("type")
                            .is_some_and(|s| s == "title" || s == "ctrTitle");
                    }
                    "t" => in_text = true,
                    "br" => paragraph.push('\n'),
                    "ta" => paragraph.push('\t'),
                    "tbl" => {
                        if rows.is_some() {
                            return Err(INVALID);
                        }
                        rows = Some(Vec::new());
                    }
                    "tr" => row.clear(),
                    "tc" => cell.clear(),
                    "cNvPr" => {
                        let a = attrs(&node)?;
                        if let Some(alt) = a.get("descr").filter(|v| !v.trim().is_empty()) {
                            out.push(format!("Изображение: {alt}"));
                        }
                    }
                    "chart" => {
                        let a = attrs(&node)?;
                        charts.push(
                            a.iter()
                                .find(|(k, _)| k.ends_with(":id"))
                                .map(|(_, v)| v.clone())
                                .ok_or(INVALID)?,
                        );
                    }
                    _ => {}
                }
            }
            Event::End(node) => match node.local_name().as_ref() {
                "t" => in_text = false,
                "p" => {
                    let p = paragraph.trim();
                    if !p.is_empty() {
                        if rows.is_some() {
                            if !cell.is_empty() {
                                cell.push('\n');
                            }
                            cell.push_str(p);
                        } else {
                            out.push(format!("{}{p}", if title { "### " } else { "" }));
                        }
                    }
                    paragraph.clear();
                }
                "tc" => row.push(std::mem::take(&mut cell)),
                "tr" => {
                    if let Some(rows) = rows.as_mut() {
                        if row.len() > 512 || rows.len() >= 20_000 {
                            return Err("PRESENTATION_LIMIT");
                        }
                        rows.push(std::mem::take(&mut row));
                    }
                }
                "tbl" => out.push(table(&rows.take().ok_or(INVALID)?)),
                _ => {}
            },
            Event::Text(text) if in_text => paragraph.push_str(&content(&text)?),
            Event::CData(text) if in_text => {
                paragraph.push_str(&text.xml_content(XmlVersion::Implicit1_0))
            }
            _ => {}
        }
        Ok(())
    })?;
    if rows.is_some() || in_text || !paragraph.trim().is_empty() {
        return Err(INVALID);
    }
    Ok((out.join("\n\n"), charts))
}
fn chart(text: &str) -> Result<String> {
    let mut out = Vec::new();
    let mut group = String::new();
    let mut in_value = false;
    let mut value = String::new();
    events(text, |event| {
        match event {
            Event::Start(node) => match node.local_name().as_ref() {
                "ser" => out.push("Серия".into()),
                "cat" | "xVal" => group = "Категории".into(),
                "val" | "yVal" => group = "Значения".into(),
                "tx" => group = "Название".into(),
                "v" | "t" => {
                    in_value = true;
                    value.clear();
                }
                _ => {}
            },
            Event::Text(text) if in_value => value.push_str(&content(&text)?),
            Event::End(node) if matches!(node.local_name().as_ref(), "v" | "t") => {
                in_value = false;
                if !value.is_empty() {
                    out.push(format!("{group}: {value}"));
                }
            }
            _ => {}
        }
        if out.len() > 100_000 {
            return Err("PRESENTATION_LIMIT");
        }
        Ok(())
    })?;
    Ok(format!(
        "### Диаграмма — сохранённые данные\n{}",
        out.join("\n")
    ))
}
pub(super) fn read(bytes: &[u8]) -> Result<Value> {
    let mut archive = super::archive(bytes)?;
    let text = super::xml(&mut archive, "ppt/presentation.xml")?;
    let rels = relationships(&mut archive, "ppt/_rels/presentation.xml.rels", "ppt", true)?;
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    events(&text, |event| {
        if let Event::Start(node) | Event::Empty(node) = event {
            if node.local_name().as_ref() == "sldId" {
                let a = attrs(&node)?;
                let id = a
                    .iter()
                    .find(|(k, _)| k.ends_with(":id"))
                    .map(|(_, v)| v)
                    .ok_or(INVALID)?;
                let relation = rels
                    .get(id)
                    .filter(|r| r.kind.ends_with("/slide"))
                    .ok_or("PRESENTATION_RELATION_INVALID")?;
                if !seen.insert(relation.path.clone()) {
                    return Err("PRESENTATION_RELATION_INVALID");
                }
                order.push(relation.path.clone());
                if order.len() > 10_000 {
                    return Err("PRESENTATION_LIMIT");
                }
            }
        }
        Ok(())
    })?;
    let mut parts = Vec::new();
    let mut sizes = 0;
    for (i, path) in order.iter().enumerate() {
        let xml = super::xml(&mut archive, path)?;
        let (body, charts) = slide(&xml)?;
        let (dir, name) = path.rsplit_once('/').ok_or(INVALID)?;
        let rels = relationships(
            &mut archive,
            &format!("{dir}/_rels/{name}.rels"),
            dir,
            false,
        )?;
        let mut text = format!(
            "<!-- Slide number: {} -->\n## Слайд {}\n\n{body}",
            i + 1,
            i + 1
        );
        for id in charts {
            let relation = rels
                .get(&id)
                .filter(|r| r.kind.ends_with("/chart"))
                .ok_or("PRESENTATION_RELATION_INVALID")?;
            text.push_str("\n\n");
            text.push_str(&chart(&super::xml(&mut archive, &relation.path)?)?);
        }
        let mut notes = rels
            .values()
            .filter(|r| r.kind.ends_with("/notesSlide"))
            .collect::<Vec<_>>();
        notes.sort_by_key(|r| &r.path);
        for note in notes {
            let (body, _) = slide(&super::xml(&mut archive, &note.path)?)?;
            if !body.is_empty() {
                text.push_str("\n\n### Заметки докладчика\n");
                text.push_str(&body);
            }
        }
        sizes += text.len();
        if sizes > super::MAX_TEXT {
            return Err("DOCUMENT_TEXT_LIMIT");
        }
        parts.push(text);
    }
    Ok(json!({"text":parts.join("\n\n"),"slides":order.len(),"extractor":"les.presentation.v1"}))
}
