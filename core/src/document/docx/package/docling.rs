//! Docling owns ordinary Word structure; LES owns exact special content and coverage.
use super::*;
use quick_xml::Writer;
use std::io::Write;
mod geometry;

fn special(name: &str) -> bool {
    matches!(
        name,
        "w:drawing"
            | "w:pict"
            | "w:object"
            | "mc:AlternateContent"
            | "m:oMath"
            | "m:oMathPara"
            | "w:footnoteReference"
            | "w:endnoteReference"
            | "w:commentReference"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn original() -> Vec<u8> {
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        z.start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        z.write_all(br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><w:body><w:p><w:r><w:t>Body</w:t></w:r><m:oMath><m:f><m:num><m:r><m:t>17.25</m:t></m:r></m:num><m:den><m:r><m:t>3</m:t></m:r></m:den></m:f></m:oMath></w:p></w:body></w:document>"#).unwrap();
        z.finish().unwrap().into_inner()
    }
    fn token(bytes: &[u8]) -> String {
        let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut text = String::new();
        z.by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        regex::Regex::new("LESPART[A-F0-9]+N[0-9]+END")
            .unwrap()
            .find(&text)
            .unwrap()
            .as_str()
            .into()
    }
    #[test]
    fn docling_cannot_drop_or_duplicate_checked_word_math() {
        let bytes = original();
        for copies in [0, 2] {
            let result =
                super::super::convert(&bytes, &json!({"word_parser":"docling"}), &|input, part| {
                    assert_eq!(input["part_kind"], "word_structure");
                    let marker = token(part.unwrap());
                    Ok(json!({"text":marker.repeat(copies)}))
                });
            assert_eq!(result.unwrap_err(), "DOCLING_WORD_INCOMPLETE");
        }
    }
    #[test]
    fn referenced_furniture_and_notes_have_one_native_owner_not_missing_body_slots() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, xml) in [
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="r"><w:body><w:p><w:r><w:t>Body</w:t><w:footnoteReference w:id="3"/></w:r></w:p><w:sectPr><w:headerReference r:id="header"/></w:sectPr></w:body></w:document>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships><Relationship Id="header" Type="x/header" Target="header1.xml"/></Relationships>"#,
            ),
            (
                "word/header1.xml",
                r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER</w:t></w:r></w:p></w:hdr>"#,
            ),
            (
                "word/footnotes.xml",
                r#"<w:footnotes xmlns:w="w"><w:footnote w:id="3"><w:p><w:r><w:t>NOTE</w:t></w:r></w:p></w:footnote><w:footnote w:id="4"><w:p><w:r><w:t>UNREFERENCED</w:t></w:r></w:p></w:footnote></w:footnotes>"#,
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        let value =
            super::super::convert(&bytes, &json!({"word_parser":"docling"}), &|input, part| {
                assert_eq!(input["part_kind"], "word_structure");
                let bytes = part.unwrap();
                let mut prepared = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
                let mut header = String::new();
                prepared
                    .by_name("word/header1.xml")
                    .unwrap()
                    .read_to_string(&mut header)
                    .unwrap();
                assert!(!header.contains("HEADER"));
                Ok(json!({"text":format!("Body {}",token(bytes))}))
            })
            .unwrap();
        let text = value["text"].as_str().unwrap();
        for word in ["Body", "HEADER", "NOTE"] {
            assert_eq!(text.matches(word).count(), 1);
        }
        assert!(!text.contains("UNREFERENCED"));
    }
    #[test]
    fn checked_word_math_is_reinserted_with_original_source_identity() {
        let bytes = original();
        let result =
            super::super::convert(&bytes, &json!({"word_parser":"docling"}), &|_, part| {
                Ok(json!({"text":format!("# Heading\n\n{}",token(part.unwrap()))}))
            })
            .unwrap();
        assert!(result["text"].as_str().unwrap().contains("(17.25)/(3)"));
        assert_eq!(
            result["metadata"]["docx_parts_source_sha256"],
            crate::sha256_hex(&bytes)
        );
        assert!(!result["text"].as_str().unwrap().contains("LESPART"));
    }
    #[test]
    fn a_span_proof_cannot_hide_an_extra_copy_or_a_second_logical_cell() {
        let prefix = "LESPART00000000000000000000000000000000";
        let token = format!("{prefix}N0END");
        let slots = vec![(token.clone(), "17.25".to_owned())];
        let proof = json!({token.clone():{"copies":2,"logical_cells":1,"span_positions":2}});
        assert_eq!(
            restore(&token.repeat(2), prefix, &slots, 0, &proof).unwrap(),
            "17.2517.25"
        );
        assert!(restore(&token.repeat(3), prefix, &slots, 0, &proof).is_err());
        assert!(restore(&token, prefix, &slots, 0, &proof).is_err());
        let bad = json!({token.clone():{"copies":2,"logical_cells":2,"span_positions":2}});
        assert!(restore(&token.repeat(2), prefix, &slots, 0, &bad).is_err());
    }
    #[test]
    fn many_special_parts_restore_once_without_repeated_whole_document_replacement() {
        let prefix = "LESPART00000000000000000000000000000000";
        let slots = (0..10000)
            .map(|i| (format!("{prefix}N{i}END"), format!("value{i}")))
            .collect::<Vec<_>>();
        let text = slots
            .iter()
            .map(|v| v.0.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let out = restore(&text, prefix, &slots, 0, &Value::Null).unwrap();
        assert!(out.starts_with("value0\n") && out.ends_with("value9999"));
        assert_eq!(out.lines().count(), 10000);
    }
    #[test]
    fn checked_cell_payload_cannot_invent_rows_columns_or_html() {
        let prefix = "LESPART00000000000000000000000000000000";
        let token = format!("{prefix}N0END");
        let slots = vec![(token.clone(), "OCR line\r\n42 | <value> & unit".to_owned())];
        let proof = json!({token.clone():{"copies":1,"logical_cells":1,"span_positions":1,"table_cell":true}});
        let out = restore(&format!("| Item | {token} |"), prefix, &slots, 0, &proof).unwrap();
        assert_eq!(
            out,
            "| Item | OCR line<br>42 &#124; &lt;value&gt; &amp; unit |"
        );
        assert_eq!(out.lines().count(), 1);
        assert_eq!(out.chars().filter(|c| *c == '|').count(), 3);
    }
    #[test]
    fn empty_table_markup_is_not_evidence_that_an_unread_image_was_read() {
        let prefix = "LESPART00000000000000000000000000000000";
        let token = format!("{prefix}N0END");
        let slots = vec![(token.clone(), "OCR not recognized".into())];
        assert_eq!(
            restore(
                &format!("| |\n| --- |\n| {token} |"),
                prefix,
                &slots,
                1,
                &Value::Null
            )
            .unwrap_err(),
            "DOCLING_WORD_INCOMPLETE"
        );
        assert!(restore(
            &format!("Source caption\n{token}"),
            prefix,
            &slots,
            1,
            &Value::Null
        )
        .is_ok());
    }
}
fn story(name: &str) -> bool {
    name == "word/document.xml"
        || name.starts_with("word/header") && name.ends_with(".xml")
        || name.starts_with("word/footer") && name.ends_with(".xml")
        || matches!(
            name,
            "word/footnotes.xml" | "word/endnotes.xml" | "word/comments.xml"
        )
}
fn write_event(out: &mut Writer<Vec<u8>>, event: Event<'_>) -> Result<()> {
    out.write_event(event).map_err(|_| BAD)
}
fn marker(out: &mut Writer<Vec<u8>>, stack: &[String], token: &str) {
    let paragraph = stack.iter().any(|s| s == "w:p");
    let run = stack.iter().any(|s| s == "w:r");
    let mut xml = String::new();
    if !paragraph {
        xml.push_str("<w:p>");
    }
    if !run {
        xml.push_str("<w:r>");
    }
    xml.push_str(&format!("<w:t>{token}</w:t>"));
    if !run {
        xml.push_str("</w:r>");
    }
    if !paragraph {
        xml.push_str("</w:p>");
    }
    out.get_mut().extend_from_slice(xml.as_bytes());
}
fn restore(
    text: &str,
    prefix: &str,
    slots: &[(String, String)],
    unread: usize,
    expansions: &Value,
) -> Result<String> {
    let expected = slots
        .iter()
        .map(|(token, _)| {
            if let Some(p) = expansions.get(token) {
                p["copies"]
                    .as_u64()
                    .filter(|n| {
                        *n >= 1
                            && *n <= 100000
                            && p["logical_cells"] == 1
                            && p["span_positions"].as_u64().is_some_and(|area| area >= *n)
                    })
                    .map(|n| n as usize)
                    .ok_or("DOCLING_WORD_INCOMPLETE")
            } else {
                Ok(1)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let mut seen = vec![0_usize; slots.len()];
    let mut out = String::new();
    let mut offset = 0;
    let mut ordinary = false;
    while let Some(at) = text[offset..].find(prefix) {
        let start = offset + at;
        let suffix = &text[start + prefix.len()..];
        let stop = suffix
            .find("END")
            .filter(|n| *n <= 12)
            .ok_or("DOCLING_WORD_INCOMPLETE")?;
        let index = suffix[..stop]
            .strip_prefix('N')
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|i| *i < slots.len())
            .ok_or("DOCLING_WORD_INCOMPLETE")?;
        let token_end = start + prefix.len() + stop + 3;
        if text[start..token_end] != slots[index].0 || seen[index] >= expected[index] {
            eprintln!(
                "{}",
                json!({"event":"docling_word_incomplete","stage":"duplicate_or_changed_slot","slot":index})
            );
            return Err("DOCLING_WORD_INCOMPLETE");
        }
        seen[index] += 1;
        let before = &text[offset..start];
        ordinary |= before.chars().any(char::is_alphanumeric);
        // Native OCR/OLE/math is raw content, not table syntax. Keep it inside
        // its proven cell; a pipe or newline must not create a false row.
        let value = if expansions[&slots[index].0]["table_cell"] == true {
            slots[index]
                .1
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('|', "&#124;")
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .replace('\n', "<br>")
        } else {
            slots[index].1.clone()
        };
        if out.len() + before.len() + value.len() > MAX_TEXT {
            return Err("DOCUMENT_TEXT_LIMIT");
        }
        out.push_str(before);
        out.push_str(&value);
        offset = token_end;
    }
    let tail = &text[offset..];
    ordinary |= tail.chars().any(char::is_alphanumeric);
    if out.len() + tail.len() > MAX_TEXT {
        return Err("DOCUMENT_TEXT_LIMIT");
    }
    out.push_str(tail);
    if let Some(index) = seen.iter().enumerate().position(|(i, n)| *n != expected[i]) {
        eprintln!(
            "{}",
            json!({"event":"docling_word_incomplete","stage":"missing_slot","slot":index,"slots":slots.len()})
        );
        return Err("DOCLING_WORD_INCOMPLETE");
    }
    if unread > 0 && !ordinary {
        return Err("DOCLING_WORD_INCOMPLETE");
    }
    Ok(out)
}
impl<F: Fn(&Value, Option<&[u8]>) -> Result<Value>> Package<'_, F> {
    pub(super) fn docling(&mut self) -> Result<String> {
        let names = (0..self.archive.len())
            .map(|i| {
                self.archive
                    .by_index(i)
                    .map_err(|_| BAD)?
                    .name()
                    .map(|s| s.into_owned())
                    .map_err(|_| BAD)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|_| BAD)?;
        let prefix = format!(
            "LESPART{}",
            random
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<String>()
        );
        let mut slots = Vec::new();
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for name in names {
            // Word stories are not interchangeable body paragraphs. Docling
            // may omit a first/even header according to section layout. The
            // existing native reader owns referenced furniture and notes once;
            // their special slots must not be demanded from Docling's body.
            let side = if name.starts_with("word/header") && name.ends_with(".xml") {
                Some("hdr")
            } else if name.starts_with("word/footer") && name.ends_with(".xml") {
                Some("ftr")
            } else {
                match name.as_str() {
                    "word/footnotes.xml" => Some("footnotes"),
                    "word/endnotes.xml" => Some("endnotes"),
                    "word/comments.xml" => Some("comments"),
                    _ => None,
                }
            };
            if let Some(root) = side {
                zip.start_file(&name, zip::write::SimpleFileOptions::default())
                    .map_err(|_| BAD)?;
                zip.write_all(format!("<w:{root} xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"/>").as_bytes()).map_err(|_|BAD)?;
                continue;
            }
            // Original media/OLE has already been handed to the existing reader by each slot.
            // It must not be decoded a second time into Docling's JSON or memory.
            if name.starts_with("word/media/") || name.starts_with("word/embeddings/") {
                continue;
            }
            if !story(&name) {
                let member = self.archive.by_name(&name).map_err(|_| BAD)?;
                zip.raw_copy_file(member).map_err(|_| BAD)?;
                continue;
            }
            let maximum = if name == "word/document.xml" {
                MAX_MAIN_XML
            } else {
                MAX_XML
            };
            let mut xml = Vec::new();
            {
                let member = self.archive.by_name(&name).map_err(|_| BAD)?;
                if member.size() > maximum {
                    return Err("DOCUMENT_XML_LIMIT");
                }
                member
                    .take(maximum + 1)
                    .read_to_end(&mut xml)
                    .map_err(|_| BAD)?;
                if xml.len() as u64 > maximum {
                    return Err("DOCUMENT_XML_LIMIT");
                }
            }
            let old = self.enter(&name)?;
            let normalized = geometry::normalize(&xml)?;
            let rendered = self.slot_xml(&normalized, &prefix, &mut slots);
            self.relationships = old;
            zip.start_file(&name, zip::write::SimpleFileOptions::default())
                .map_err(|_| BAD)?;
            zip.write_all(&rendered?).map_err(|_| BAD)?;
        }
        let prepared = zip.finish().map_err(|_| BAD)?.into_inner();
        let mut request = self.request.clone();
        request["part_kind"] = json!("word_structure");
        request["prepared_sha256"] = json!(crate::sha256_hex(&prepared));
        request["slot_prefix"] = json!(prefix);
        let result = (self.external)(&request, Some(&prepared))?;
        let text = result["text"].as_str().ok_or("DOCLING_WORD_INCOMPLETE")?;
        let mut text = restore(
            text,
            &prefix,
            &slots,
            self.unread_visual,
            &result["slot_expansions"],
        )?;
        if text.trim().is_empty() && !self.floating.iter().any(|(_, s)| !s.trim().is_empty()) {
            return Err("NO_SEARCHABLE_TEXT");
        }
        self.read_related(&mut text)?;
        Ok(text)
    }
    fn slot_xml(
        &mut self,
        xml: &[u8],
        prefix: &str,
        slots: &mut Vec<(String, String)>,
    ) -> Result<Vec<u8>> {
        let mut reader = Reader::from_reader(xml);
        let mut out = Writer::new(Vec::new());
        if xml.windows(prefix.len()).any(|s| s == prefix.as_bytes()) {
            return Err(BAD);
        }
        let mut stack = Vec::<String>::new();
        let mut document = Document {
            styles: styles::read(&mut self.archive).ok_or(BAD)?,
            numbering: numbering::Numbering::read(&mut self.archive).ok_or(BAD)?,
            ..Default::default()
        };
        loop {
            let event = reader.read_event().map_err(|_| "DOCUMENT_XML_INVALID")?;
            match event {
                Event::Start(e) if special(e.name().as_ref()) => {
                    let node = Node::read(&mut reader, &e).ok_or(BAD)?;
                    let value = self.render(&node, &mut document)?;
                    if slots.len() >= 100000 {
                        return Err(LIMIT);
                    }
                    let token = format!("{prefix}N{}END", slots.len());
                    marker(&mut out, &stack, &token);
                    slots.push((token, value));
                }
                Event::Empty(e) if special(e.name().as_ref()) => {
                    let node = Node::empty(&e).ok_or(BAD)?;
                    let value = self.render(&node, &mut document)?;
                    if slots.len() >= 100000 {
                        return Err(LIMIT);
                    }
                    let token = format!("{prefix}N{}END", slots.len());
                    marker(&mut out, &stack, &token);
                    slots.push((token, value));
                }
                Event::Start(e) => {
                    let name = e.name().as_ref().to_owned();
                    if matches!(name.as_str(), "w:headerReference" | "w:footerReference") {
                        self.reference(&e)?;
                    }
                    if matches!(
                        name.as_str(),
                        "w:ins"
                            | "w:del"
                            | "w:moveFrom"
                            | "w:moveTo"
                            | "w:pPrChange"
                            | "w:rPrChange"
                            | "w:tblPrChange"
                            | "w:tcPrChange"
                            | "w:trPrChange"
                    ) {
                        return Err("DOCX_REVISION_UNSUPPORTED");
                    }
                    stack.push(name);
                    write_event(&mut out, Event::Start(e))?;
                }
                Event::Empty(e) if e.name().as_ref() == "w:sym" => {
                    let value = symbols::decode(
                        &attribute(&e, "w:font").ok_or("DOCX_SYMBOL_UNSUPPORTED")?,
                        &attribute(&e, "w:char").ok_or("DOCX_SYMBOL_UNSUPPORTED")?,
                    )
                    .ok_or("DOCX_SYMBOL_UNSUPPORTED")?;
                    out.get_mut().extend_from_slice(
                        format!(
                            "<w:t>{}</w:t>",
                            quick_xml::escape::escape(&value.to_string())
                        )
                        .as_bytes(),
                    );
                }
                Event::Empty(e) => {
                    if matches!(e.name().as_ref(), "w:headerReference" | "w:footerReference") {
                        self.reference(&e)?;
                    }
                    write_event(&mut out, Event::Empty(e))?;
                }
                Event::End(e) => {
                    if stack.pop().as_deref() != Some(e.name().as_ref()) {
                        return Err(BAD);
                    }
                    write_event(&mut out, Event::End(e))?;
                }
                Event::Eof => {
                    if !stack.is_empty() {
                        return Err(BAD);
                    }
                    break;
                }
                Event::DocType(_) | Event::CData(_) => return Err("DOCUMENT_XML_INVALID"),
                other => write_event(&mut out, other)?,
            }
            if out.get_ref().len() > MAX_MAIN_XML as usize + 8 * 1024 * 1024 {
                return Err("DOCUMENT_XML_LIMIT");
            }
        }
        Ok(out.into_inner())
    }
}
