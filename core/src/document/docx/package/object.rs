//! Read embedded native data without activating an OLE application.
use super::*;
impl<F: Fn(&Value, Option<&[u8]>) -> Result<Value>> Package<'_, F> {
    pub(super) fn object(&mut self, node: &Node) -> Result<String> {
        let ole = find(node, "o:OLEObject").ok_or("DOCX_EMBEDDED_OBJECT_UNSUPPORTED")?;
        if ole.attrs.get("Type").is_some_and(|v| v != "Embed") {
            return Err("DOCX_EXTERNAL_PART_UNSUPPORTED");
        }
        let id = ole.attrs.get("r:id").ok_or(BAD)?;
        let name = self
            .target(id, "/oleObject")
            .or_else(|_| self.target(id, "/package"))?;
        let bytes = self.member(&name)?;
        let mut part_id = crate::sha256_hex(&bytes);
        let program = ole.attrs.get("ProgID").map(String::as_str).unwrap_or("");
        let text = if bytes.starts_with(b"PK\x03\x04") && program.starts_with("Excel.Sheet") {
            let result = crate::document::conversion::embedded_spreadsheet(&bytes, &name)?;
            self.asset(&name,"binary",&bytes,&json!({"content_type":"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"}))?;
            result["text"].as_str().ok_or(BAD)?.to_owned()
        } else if matches!(program, "Equation.3" | "Equation.DSMT4") {
            let raw = crate::document::conversion::msg::compound_stream(
                &bytes,
                "Equation Native",
                1024 * 1024,
            )
            .map_err(|_| BAD)?
            .ok_or(BAD)?;
            self.asset(
                &name,
                "binary",
                &bytes,
                &json!({"content_type":"application/x-ole-storage"}),
            )?;
            equation::read(&raw)?
        } else if program == "FoxitReader.Document" || program == "AcroExch.Document.DC" {
            let raw = crate::document::conversion::msg::compound_stream(
                &bytes,
                "Contents",
                MAX_PART as usize,
            )
            .map_err(|_| BAD)?
            .ok_or(BAD)?;
            if !raw.starts_with(b"%PDF-") {
                return Err(BAD);
            }
            self.asset(
                &name,
                "binary",
                &bytes,
                &json!({"content_type":"application/x-ole-storage"}),
            )?;
            let value = self.asset(&format!("{name}/Contents.pdf"), "pdf", &raw, &json!({}))?;
            part_id = crate::sha256_hex(&raw);
            value["text"].as_str().ok_or(BAD)?.to_owned()
        } else if matches!(program, "PBrush" | "Unknown") {
            self.asset(
                &name,
                "binary",
                &bytes,
                &json!({"content_type":"application/x-ole-storage"}),
            )?;
            let (part, payload) = if program == "PBrush" {
                let raw = crate::document::conversion::msg::compound_stream(
                    &bytes,
                    "\u{1}Ole10Native",
                    MAX_PART as usize,
                )
                .map_err(|_| BAD)?
                .ok_or(BAD)?;
                let bitmap = ole_images::bitmap(&raw)?.to_vec();
                (format!("{name}/Native.bmp"), bitmap)
            } else {
                let raw = crate::document::conversion::msg::compound_stream(
                    &bytes,
                    "CONTENTS",
                    MAX_PART as usize,
                )
                .map_err(|_| BAD)?
                .ok_or(BAD)?;
                let emf = ole_images::enhanced(&raw)?.to_vec();
                (format!("{name}/Contents.emf"), emf)
            };
            part_id = crate::sha256_hex(&payload);
            let value = self.asset(
                &part,
                "image",
                &payload,
                &json!({"attributes":{},"picture":null}),
            )?;
            let text = value["text"].as_str().ok_or(BAD)?;
            if text.is_empty() && value["blank"] != true {
                self.unread_visual += 1;
                "Изображение исходного объекта сохранено. OCR не распознал текст; содержимое требует визуальной проверки.".to_owned()
            } else {
                text.to_owned()
            }
        } else {
            return Err("DOCX_EMBEDDED_OBJECT_UNSUPPORTED");
        };
        Ok(format!(
            "[Текст встроенного объекта {name}; part_id={part_id}; оригинал сохранён отдельно. Номера страниц/листов относятся к вложению, а не к страницам Word.]\n{text}"
        ))
    }
}
