//! Read Word paragraphs, heading levels and rectangular tables from OOXML.
use quick_xml::{
    escape::resolve_predefined_entity,
    events::{BytesStart, Event},
    Reader, XmlVersion,
};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
};
use zip::ZipArchive;

const MAX_XML: u64 = 8 * 1024 * 1024;

pub fn read(path: &Path) -> Value {
    match extract(path) {
        Some(text) => {
            json!({"status":"completed","text":text,"extractor":"les.docx-paragraphs.v1"})
        }
        None => json!({"status":"compatibility"}),
    }
}

fn extract(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > 64 * 1024 * 1024 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    read_bytes(&bytes)
}

pub(super) fn read_bytes(bytes: &[u8]) -> Option<String> {
    if bytes.len() > 64 * 1024 * 1024 {
        return None;
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes)).ok()?;
    let member = archive.by_name("word/document.xml").ok()?;
    if member.size() > MAX_XML {
        return None;
    }
    let mut xml = Vec::new();
    member.take(MAX_XML + 1).read_to_end(&mut xml).ok()?;
    if xml.len() as u64 > MAX_XML || std::str::from_utf8(&xml).is_err() {
        return None;
    }
    parse(&xml)
}

#[derive(Default)]
struct Document {
    paragraphs: Vec<String>,
    text: String,
    paragraph: bool,
    text_run: bool,
    level: usize,
    body: bool,
    table: Option<Table>,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    row: Option<Vec<String>>,
    cell: Option<Vec<String>>,
}

impl Table {
    fn markdown(self) -> Option<String> {
        let width = self.rows.first()?.len();
        if width == 0
            || width > 512
            || self.rows.len().saturating_mul(width) > 100_000
            || self.rows.iter().any(|row| row.len() != width)
        {
            return None;
        }
        let mut out = format!("{}|\n{}|", "|  ".repeat(width), "| --- ".repeat(width));
        for row in self.rows {
            out.push_str("\n|");
            for cell in row {
                out.push(' ');
                out.push_str(&cell.replace('|', "\\|").replace('\n', "<br>"));
                out.push_str(" |");
            }
        }
        Some(out)
    }
}

fn attribute<'a>(element: &BytesStart<'a>, name: &str) -> Option<String> {
    Some(element.try_get_attribute(name).ok()??.value.into_owned())
}

impl Document {
    fn start(&mut self, element: &BytesStart<'_>) -> Option<()> {
        match element.name().as_ref() {
            "w:document" => {
                let namespace = attribute(element, "xmlns:w")?;
                if namespace != "http://schemas.openxmlformats.org/wordprocessingml/2006/main" {
                    return None;
                }
            }
            "w:body" => self.body = true,
            "w:tbl" if self.body && !self.paragraph && self.table.is_none() => {
                self.table = Some(Table::default())
            }
            "w:tr" => {
                let table = self.table.as_mut()?;
                if table.row.is_some() {
                    return None;
                }
                table.row = Some(Vec::new());
            }
            "w:tc" => {
                let table = self.table.as_mut()?;
                if table.row.is_none() || table.cell.is_some() {
                    return None;
                }
                table.cell = Some(Vec::new());
            }
            "w:p"
                if self.body
                    && !self.paragraph
                    && self.table.as_ref().is_none_or(|table| table.cell.is_some()) =>
            {
                self.paragraph = true
            }
            "w:t" if self.paragraph => self.text_run = true,
            "w:br" | "w:cr" if self.paragraph => {
                if attribute(element, "w:type")
                    .is_some_and(|kind| kind != "textWrapping" && kind != "page")
                {
                    return None;
                }
                self.text.push('\n');
            }
            "w:tab" if self.paragraph => self.text.push('\t'),
            "w:noBreakHyphen" if self.paragraph => self.text.push('\u{2011}'),
            "w:softHyphen" if self.paragraph => self.text.push('\u{00ad}'),
            "w:pStyle" if self.paragraph => {
                let raw = attribute(element, "w:val")?;
                let style = raw.as_str();
                self.level = match style {
                    "Normal" | "BodyText" => 0,
                    "Title" => 1,
                    _ => style
                        .strip_prefix("Heading")?
                        .parse::<usize>()
                        .ok()
                        .filter(|n| (1..=6).contains(n))?,
                };
            }
            "w:outlineLvl" if self.paragraph => {
                let raw = attribute(element, "w:val")?;
                self.level = raw.as_str().parse::<usize>().ok()?.checked_add(1)?.min(6);
            }
            "w:r"
            | "w:pPr"
            | "w:rPr"
            | "w:sectPr"
            | "w:pgSz"
            | "w:pgMar"
            | "w:cols"
            | "w:docGrid"
            | "w:spacing"
            | "w:jc"
            | "w:ind"
            | "w:keepNext"
            | "w:keepLines"
            | "w:widowControl"
            | "w:rFonts"
            | "w:color"
            | "w:sz"
            | "w:szCs"
            | "w:lang"
            | "w:proofErr"
            | "w:bookmarkStart"
            | "w:bookmarkEnd"
            | "w:lastRenderedPageBreak" => {}
            "w:tblPr" | "w:tblW" | "w:tblLook" | "w:tblGrid" | "w:gridCol" | "w:tcPr" | "w:tcW"
            | "w:trPr" | "w:tblStyle" | "w:trHeight"
                if self.table.is_some() => {}
            // Preserve the legacy renderer for content with relationships,
            // list numbering, merged cells, revisions or unimplemented formatting.
            _ => return None,
        }
        Some(())
    }

    fn end(&mut self, name: &str) -> Option<()> {
        match name {
            "w:body" => self.body = false,
            "w:t" => self.text_run = false,
            "w:p" => {
                let text = self.text.trim();
                if !text.is_empty() {
                    if let Some(table) = &mut self.table {
                        table.cell.as_mut()?.push(text.to_owned());
                    } else {
                        self.paragraphs.push(if self.level > 0 {
                            format!("{} {text}", "#".repeat(self.level))
                        } else {
                            text.to_owned()
                        });
                    }
                }
                self.text.clear();
                self.paragraph = false;
                self.level = 0;
            }
            "w:tc" => {
                let table = self.table.as_mut()?;
                table.row.as_mut()?.push(table.cell.take()?.join("\n"));
            }
            "w:tr" => {
                let table = self.table.as_mut()?;
                table.rows.push(table.row.take()?);
            }
            "w:tbl" => self.paragraphs.push(self.table.take()?.markdown()?),
            _ => {}
        }
        Some(())
    }
}

fn parse(xml: &[u8]) -> Option<String> {
    let mut reader = Reader::from_reader(xml);
    let mut document = Document::default();
    loop {
        match reader.read_event().ok()? {
            Event::Start(element) => document.start(&element)?,
            Event::Empty(element) => {
                document.start(&element)?;
                document.end(element.name().as_ref())?;
            }
            Event::End(element) => document.end(element.name().as_ref())?,
            Event::Text(text) if document.text_run => document
                .text
                .push_str(&text.xml_content(XmlVersion::Implicit1_0)),
            Event::GeneralRef(reference) if document.text_run => {
                if let Some(value) = reference.resolve_char_ref().ok()? {
                    document.text.push(value);
                } else {
                    document
                        .text
                        .push_str(resolve_predefined_entity(reference.as_ref())?);
                }
            }
            Event::DocType(_) | Event::CData(_) => return None,
            Event::Eof => break,
            _ => {}
        }
    }
    if document.body
        || document.paragraph
        || document.table.is_some()
        || document.paragraphs.is_empty()
    {
        return None;
    }
    Some(document.paragraphs.join("\n\n"))
}
