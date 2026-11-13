//! Make OOXML cell defaults explicit before Docling's descendant property lookup.
//! In 2.82.1 a nested table's gridSpan/vMerge otherwise leaks into its outer cell.
use super::*;

#[derive(Default)]
struct Cell {
    properties: bool,
    span: bool,
    merge: bool,
}
fn defaults(out: &mut Writer<Vec<u8>>, cell: &Cell) {
    if !cell.span {
        out.get_mut()
            .extend_from_slice(br#"<w:gridSpan w:val="1"/>"#);
    }
    if !cell.merge {
        out.get_mut()
            .extend_from_slice(br#"<w:vMerge w:val="restart"/>"#);
    }
}
fn absent_properties(out: &mut Writer<Vec<u8>>, cell: &mut Cell) {
    out.get_mut().extend_from_slice(b"<w:tcPr>");
    defaults(out, cell);
    out.get_mut().extend_from_slice(b"</w:tcPr>");
    cell.properties = true;
}
pub(super) fn normalize(xml: &[u8]) -> Result<Vec<u8>> {
    let mut reader = Reader::from_reader(xml);
    let mut out = Writer::new(Vec::new());
    let mut stack = Vec::<String>::new();
    let mut cells = Vec::<Cell>::new();
    loop {
        let event = reader.read_event().map_err(|_| "DOCUMENT_XML_INVALID")?;
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let name = e.name().as_ref().to_owned();
                if stack.last().is_some_and(|s| s == "w:tc") {
                    let cell = cells.last_mut().ok_or(BAD)?;
                    if name == "w:tcPr" {
                        if cell.properties {
                            return Err(BAD);
                        }
                        cell.properties = true;
                        if matches!(event, Event::Empty(_)) {
                            out.get_mut().extend_from_slice(b"<w:tcPr>");
                            defaults(&mut out, cell);
                            out.get_mut().extend_from_slice(b"</w:tcPr>");
                            continue;
                        }
                    } else if !cell.properties {
                        absent_properties(&mut out, cell);
                    }
                }
                if stack.last().is_some_and(|s| s == "w:tcPr") {
                    if let Some(cell) = cells.last_mut() {
                        if name == "w:gridSpan" {
                            cell.span = true;
                        }
                        if name == "w:vMerge" {
                            cell.merge = true;
                        }
                    }
                }
                if matches!(event, Event::Start(_)) {
                    if name == "w:tc" {
                        cells.push(Cell::default());
                    }
                    stack.push(name);
                }
            }
            Event::End(e) => {
                let name = e.name().as_ref().to_owned();
                if name == "w:tcPr" {
                    defaults(&mut out, cells.last().ok_or(BAD)?);
                }
                if name == "w:tc" {
                    let mut cell = cells.pop().ok_or(BAD)?;
                    if !cell.properties {
                        absent_properties(&mut out, &mut cell);
                    }
                }
                if stack.pop().as_deref() != Some(name.as_str()) {
                    return Err(BAD);
                }
            }
            Event::Eof => {
                if !stack.is_empty() || !cells.is_empty() {
                    return Err(BAD);
                }
                break;
            }
            Event::DocType(_) | Event::CData(_) => return Err("DOCUMENT_XML_INVALID"),
            _ => {}
        }
        write_event(&mut out, event)?;
        if out.get_ref().len() > MAX_MAIN_XML as usize + 8 * 1024 * 1024 {
            return Err("DOCUMENT_XML_LIMIT");
        }
    }
    Ok(out.into_inner())
}
