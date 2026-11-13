//! Read Word paragraphs, heading levels and rectangular tables from OOXML.
use quick_xml::{
    escape::resolve_predefined_entity,
    events::{BytesStart, Event},
    Reader, XmlVersion,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Cursor, Read},
    path::Path,
};
use zip::ZipArchive;

mod diagnostic;
mod numbering;
mod package;
mod rich;
mod styles;
mod symbols;
mod table;
pub(super) use diagnostic::checked;
pub(super) use package::convert;
use table::Table;

const MAX_XML: u64 = 8 * 1024 * 1024;
const MAX_MAIN_XML: u64 = 128 * 1024 * 1024;
const MAX_TEXT: usize = 24 * 1024 * 1024;

trait Parts {
    fn reference(&mut self, _: &BytesStart<'_>) -> Result<(), &'static str> {
        Ok(())
    }
    fn render(
        &mut self,
        node: &rich::Node,
        document: &mut Document,
    ) -> Result<String, &'static str>;
}
struct TextParts;
impl Parts for TextParts {
    fn render(
        &mut self,
        node: &rich::Node,
        document: &mut Document,
    ) -> Result<String, &'static str> {
        if node.name.starts_with("m:") {
            node.math()
        } else {
            node.drawing(document)
        }
        .ok_or_else(|| diagnostic::element_code(&node.name))
    }
}

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
    checked(bytes).ok()
}

#[derive(Default)]
struct Document {
    styles: BTreeMap<String, usize>,
    paragraphs: Vec<String>,
    text: String,
    paragraph: bool,
    text_run: bool,
    level: usize,
    body: bool,
    properties: Vec<String>,
    table: Option<Table>,
    tables: Vec<Table>,
    numbering: numbering::Numbering,
    number_id: Option<String>,
    list_level: usize,
    failure: Option<&'static str>,
    output_size: usize,
}

fn attribute<'a>(element: &BytesStart<'a>, name: &str) -> Option<String> {
    Some(
        quick_xml::escape::unescape(&element.try_get_attribute(name).ok()??.value)
            .ok()?
            .into_owned(),
    )
}

impl Document {
    fn emit(&mut self, text: String) -> Option<()> {
        self.output_size = self.output_size.checked_add(text.len() + 2)?;
        if self.output_size > MAX_TEXT {
            self.failure = Some("DOCUMENT_TEXT_LIMIT");
            return None;
        }
        self.paragraphs.push(text);
        Some(())
    }
    fn start(&mut self, element: &BytesStart<'_>) -> Option<()> {
        let name = element.name();
        let name = name.as_ref();
        if matches!(
            name,
            "w:pPr"
                | "w:rPr"
                | "w:tblPr"
                | "w:tcPr"
                | "w:trPr"
                | "w:sectPr"
                | "w:tblPrEx"
                | "w:sdtPr"
                | "w:sdtEndPr"
                | "w:smartTagPr"
        ) {
            self.properties.push(name.to_owned());
            return Some(());
        }
        if let Some(properties) = self.properties.last() {
            match name {
                "w:pStyle" if properties == "w:pPr" => {
                    let style = attribute(element, "w:val")?;
                    self.level = self
                        .styles
                        .get(&style)
                        .copied()
                        .or_else(|| styles::builtin(&style))?;
                }
                "w:outlineLvl" if properties == "w:pPr" => {
                    self.level = styles::outline(&attribute(element, "w:val")?)?
                }
                "w:gridSpan" if properties == "w:tcPr" => self
                    .table
                    .as_mut()?
                    .span(attribute(element, "w:val")?.parse().ok()?)?,
                "w:vMerge" if properties == "w:tcPr" => self
                    .table
                    .as_mut()?
                    .merge(attribute(element, "w:val").as_deref())?,
                "w:numId" if properties == "w:pPr" => {
                    self.number_id = Some(attribute(element, "w:val")?)
                }
                "w:ilvl" if properties == "w:pPr" => {
                    self.list_level = numbering::level(&attribute(element, "w:val")?)?
                }
                "w:gridBefore" | "w:gridAfter" if properties == "w:trPr" => {
                    self.table.as_mut()?.omitted(
                        attribute(element, "w:val")?.parse().ok()?,
                        name == "w:gridBefore",
                    )?;
                }
                // Revisions require their own semantic reader.
                "w:pPrChange" | "w:rPrChange" | "w:tblPrChange" | "w:tcPrChange"
                | "w:trPrChange" => return None,
                _ => {}
            }
            return Some(());
        }
        match element.name().as_ref() {
            "w:document" => {
                let namespace = attribute(element, "xmlns:w")?;
                if namespace != "http://schemas.openxmlformats.org/wordprocessingml/2006/main" {
                    return None;
                }
            }
            "w:body" => self.body = true,
            "w:gridCol" if self.table.is_some() => self.table.as_mut()?.column()?,
            "w:tbl" if self.body && !self.paragraph => {
                if let Some(outer) = self.table.take() {
                    if outer.cell.is_none() || self.tables.len() >= 16 {
                        return None;
                    }
                    self.tables.push(outer);
                }
                self.table = Some(Table::default())
            }
            "w:tr" => {
                self.table.as_mut()?.start_row()?;
            }
            "w:tc" => {
                self.table.as_mut()?.start_cell()?;
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
                if attribute(element, "w:type").is_some_and(|kind| {
                    !matches!(kind.as_str(), "textWrapping" | "page" | "column")
                }) {
                    return None;
                }
                self.text.push('\n');
            }
            "w:tab" if self.paragraph => self.text.push('\t'),
            "w:noBreakHyphen" if self.paragraph => self.text.push('\u{2011}'),
            "w:softHyphen" if self.paragraph => self.text.push('\u{00ad}'),
            "w:sym" if self.paragraph => {
                let font = attribute(element, "w:font")?;
                let value = attribute(element, "w:char")?;
                self.text.push(symbols::decode(&font, &value)?);
            }
            "w:pStyle" if self.paragraph => {
                let raw = attribute(element, "w:val")?;
                self.level = self
                    .styles
                    .get(&raw)
                    .copied()
                    .or_else(|| styles::builtin(&raw))?;
            }
            "w:outlineLvl" if self.paragraph => {
                let raw = attribute(element, "w:val")?;
                self.level = styles::outline(&raw)?;
            }
            "w:r"
            | "w:hyperlink"
            | "w:fldSimple"
            | "w:fldChar"
            | "w:smartTag"
            | "w:sdt"
            | "w:sdtContent"
            | "w:bdo"
            | "w:dir"
            | "w:instrText"
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
            "w:footnoteRef" | "w:endnoteRef" | "w:commentRangeStart" | "w:commentRangeEnd" => {}
            "w:tblPr" | "w:tblW" | "w:tblLook" | "w:tblGrid" | "w:gridCol" | "w:tcPr" | "w:tcW"
            | "w:trPr" | "w:tblStyle" | "w:trHeight"
                if self.table.is_some() => {}
            // Reject unsupported content rather than dropping its semantics.
            _ => return None,
        }
        Some(())
    }

    fn end(&mut self, name: &str) -> Option<()> {
        if !self.properties.is_empty() {
            if self
                .properties
                .last()
                .is_some_and(|property| property == name)
            {
                self.properties.pop();
            }
            return Some(());
        }
        match name {
            "w:body" => self.body = false,
            "w:t" => self.text_run = false,
            "w:p" => {
                // Continuation cells of a vertical merge are not displayed by Word.
                // Their empty inherited numPr must not create or advance a list label.
                let number = if self.text.trim().is_empty()
                    && self.table.as_ref().is_some_and(Table::continuation)
                {
                    None
                } else {
                    self.number_id.as_deref()
                };
                let prefix = match number {
                    Some(id) => Some(match self.numbering.next(id, self.list_level) {
                        Some(prefix) => prefix,
                        None => {
                            eprintln!(
                                "{}",
                                json!({"event":"docx_numbering_failed","id":id,"level":self.list_level})
                            );
                            self.failure = Some("DOCX_NUMBERING_UNSUPPORTED");
                            return None;
                        }
                    }),
                    None => None,
                };
                let text = self.text.trim();
                let text = match prefix.filter(|s| !s.is_empty()) {
                    Some(prefix) => format!("{prefix} {text}"),
                    None => text.to_owned(),
                };
                if !text.is_empty() {
                    if let Some(table) = &mut self.table {
                        table.cell.as_mut()?.push(text.clone());
                    } else {
                        self.emit(if self.level > 0 {
                            format!("{} {text}", "#".repeat(self.level))
                        } else {
                            text.to_owned()
                        })?;
                    }
                }
                self.text.clear();
                self.paragraph = false;
                self.level = 0;
                self.number_id = None;
                self.list_level = 0;
            }
            "w:tc" => {
                self.table.as_mut()?.finish_cell()?;
            }
            "w:tr" => {
                self.table.as_mut()?.finish_row()?;
            }
            "w:tbl" => {
                let text = self.table.take()?.markdown()?;
                if let Some(mut outer) = self.tables.pop() {
                    outer.cell.as_mut()?.push(text);
                    self.table = Some(outer);
                } else {
                    self.emit(text)?;
                }
            }
            _ => {}
        }
        Some(())
    }
}

fn parse_stream<R: BufRead>(
    input: R,
    styles: BTreeMap<String, usize>,
    numbering: numbering::Numbering,
    parts: &mut dyn Parts,
) -> Result<String, &'static str> {
    let mut document = Document {
        styles,
        numbering,
        ..Document::default()
    };
    parse_events(&mut Reader::from_reader(input), &mut document, parts)?;
    if document.body
        || document.paragraph
        || document.table.is_some()
        || !document.tables.is_empty()
        || !document.properties.is_empty()
        || document.paragraphs.is_empty()
    {
        return Err("DOCX_READ_FAILED");
    }
    let result = document.paragraphs.join("\n\n");
    if result.len() > MAX_TEXT {
        return Err("DOCUMENT_TEXT_LIMIT");
    }
    Ok(result)
}

fn parse_into(xml: &[u8], document: &mut Document) -> Result<(), &'static str> {
    parse_events(&mut Reader::from_reader(xml), document, &mut TextParts)
}

fn parse_events<R: BufRead>(
    reader: &mut Reader<R>,
    document: &mut Document,
    parts: &mut dyn Parts,
) -> Result<(), &'static str> {
    let mut buffer = Vec::new();
    let mut text_bytes = 0usize;
    loop {
        buffer.clear();
        match reader
            .read_event_into(&mut buffer)
            .map_err(|_| "DOCUMENT_XML_INVALID")?
        {
            Event::Start(element) | Event::Empty(element)
                if matches!(
                    element.name().as_ref(),
                    "w:headerReference" | "w:footerReference"
                ) =>
            {
                parts.reference(&element)?;
                document.start(&element).ok_or("DOCX_READ_FAILED")?;
            }
            Event::Empty(element)
                if document.properties.is_empty()
                    && matches!(
                        element.name().as_ref(),
                        "w:footnoteReference" | "w:endnoteReference" | "w:commentReference"
                    ) =>
            {
                let node = rich::Node::empty(&element).ok_or("DOCX_PART_INVALID")?;
                let text = parts.render(&node, document)?;
                text_bytes = text_bytes
                    .checked_add(text.len())
                    .ok_or("DOCUMENT_TEXT_LIMIT")?;
                if !document.paragraph {
                    return Err("DOCX_READ_FAILED");
                }
                document.text.push_str(&format!("\n\n{text}\n\n"));
            }
            Event::Start(element)
                if document.properties.is_empty()
                    && matches!(
                        element.name().as_ref(),
                        "m:oMath"
                            | "m:oMathPara"
                            | "w:pict"
                            | "w:drawing"
                            | "w:object"
                            | "mc:AlternateContent"
                            | "w:footnoteReference"
                            | "w:endnoteReference"
                            | "w:commentReference"
                    ) =>
            {
                let code = diagnostic::element_code(element.name().as_ref());
                let node = rich::Node::read(reader, &element).ok_or(code)?;
                let is_math = element.name().as_ref().starts_with("m:");
                let text = parts.render(&node, document)?;
                text_bytes = text_bytes
                    .checked_add(text.len())
                    .ok_or("DOCUMENT_TEXT_LIMIT")?;
                if document.paragraph {
                    if !is_math {
                        document.text.push_str("\n\n");
                    }
                    document.text.push_str(&text);
                    if !is_math {
                        document.text.push_str("\n\n");
                    }
                } else if document.body && document.table.is_none() {
                    document.emit(text).ok_or("DOCUMENT_TEXT_LIMIT")?;
                } else {
                    return Err("DOCX_READ_FAILED");
                }
            }
            Event::Start(element) => document
                .start(&element)
                .ok_or_else(|| diagnostic::unsupported(element.name().as_ref()))?,
            Event::Empty(element) => {
                document
                    .start(&element)
                    .ok_or_else(|| diagnostic::unsupported(element.name().as_ref()))?;
                document.end(element.name().as_ref()).ok_or(
                    document
                        .failure
                        .unwrap_or_else(|| diagnostic::element_code(element.name().as_ref())),
                )?;
            }
            Event::End(element) => document.end(element.name().as_ref()).ok_or_else(|| {
                document
                    .failure
                    .unwrap_or_else(|| diagnostic::unsupported(element.name().as_ref()))
            })?,
            Event::Text(text) if document.text_run => {
                let text = text.xml_content(XmlVersion::Implicit1_0);
                text_bytes = text_bytes
                    .checked_add(text.len())
                    .ok_or("DOCUMENT_TEXT_LIMIT")?;
                document.text.push_str(&text);
            }
            Event::GeneralRef(reference) if document.text_run => {
                if let Some(value) = reference
                    .resolve_char_ref()
                    .map_err(|_| "DOCUMENT_XML_INVALID")?
                {
                    text_bytes = text_bytes
                        .checked_add(value.len_utf8())
                        .ok_or("DOCUMENT_TEXT_LIMIT")?;
                    document.text.push(value);
                } else {
                    let value = resolve_predefined_entity(reference.as_ref())
                        .ok_or("DOCUMENT_XML_INVALID")?;
                    text_bytes = text_bytes
                        .checked_add(value.len())
                        .ok_or("DOCUMENT_TEXT_LIMIT")?;
                    document.text.push_str(value);
                }
            }
            Event::DocType(_) | Event::CData(_) => return Err("DOCUMENT_XML_INVALID"),
            Event::Eof => break,
            _ => {}
        }
        if text_bytes > MAX_TEXT {
            return Err("DOCUMENT_TEXT_LIMIT");
        }
    }
    Ok(())
}
