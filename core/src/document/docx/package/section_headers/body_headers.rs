//! Only the contiguous, explicitly marked leading rows form a repeated header.
//! Keep every original row: the extra caption is a lossless column projection.
use super::*;

pub(super) struct LeadingRows {
    open: bool,
    start: Option<usize>,
    marked: bool,
    ranges: Vec<(usize, usize)>,
}
impl Default for LeadingRows {
    fn default() -> Self {
        Self {
            open: true,
            start: None,
            marked: false,
            ranges: Vec::new(),
        }
    }
}
impl LeadingRows {
    pub(super) fn start(
        &mut self,
        e: &BytesStart<'_>,
        stack: &[String],
        position: usize,
        empty: bool,
    ) {
        if !self.open {
            return;
        }
        if e.name().as_ref() == "w:tr" && stack.last().is_some_and(|s| s == "w:tbl") {
            if empty {
                self.open = false;
                return;
            }
            self.start = Some(position);
            self.marked = false;
        } else if e.name().as_ref() == "w:tblHeader"
            && stack.last().is_some_and(|s| s == "w:trPr")
            && attribute(e, "w:val").is_none_or(|s| !matches!(s.as_str(), "0" | "false" | "off"))
        {
            self.marked = true;
        }
    }
    pub(super) fn end(&mut self, name: &str, stack: &[String], position: usize) {
        if name != "w:tr" || stack.iter().filter(|s| *s == "w:tbl").count() != 1 {
            return;
        }
        if let Some(start) = self.start.take() {
            if self.marked {
                self.ranges.push((start, position));
            } else {
                self.open = false;
            }
        }
    }
    pub(super) fn labels(&self, xml: &[u8], grid: &Grid) -> Option<Vec<String>> {
        // If the entire table is marked, there is no confirmed data boundary.
        // Do not turn its body into repeating column labels.
        if self.open || self.ranges.len() < 2 {
            return None;
        }
        let size = self
            .ranges
            .iter()
            .try_fold(0usize, |n, (a, b)| n.checked_add(b.checked_sub(*a)?))?;
        if size > MAX_PART as usize {
            return None;
        }
        let mut text = format!(
            "<w:tbl><w:tblGrid>{}</w:tblGrid>",
            grid.widths
                .iter()
                .map(|w| format!("<w:gridCol w:w=\"{w}\"/>"))
                .collect::<String>()
        )
        .into_bytes();
        for (a, b) in &self.ranges {
            text.extend_from_slice(xml.get(*a..*b)?);
        }
        text.extend_from_slice(b"</w:tbl>");
        let mut reader = Reader::from_reader(text.as_slice());
        let Event::Start(e) = reader.read_event().ok()? else {
            return None;
        };
        let table = Node::read(&mut reader, &e)?;
        caption(&table, "body").map(|c| c.labels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn marked(text: &str) -> String {
        format!(
            "<w:tr><w:trPr><w:tblHeader/></w:trPr>{}</w:tr>",
            text.split('|')
                .map(|s| format!("<w:tc><w:p><w:r><w:t>{s}</w:t></w:r></w:p></w:tc>"))
                .collect::<String>()
        )
    }
    #[test]
    fn compound_header_keeps_subcolumns_and_all_original_rows() {
        let table = format!("<w:tbl><w:tblGrid><w:gridCol w:w=\"1000\"/><w:gridCol w:w=\"1000\"/></w:tblGrid>{}{}<w:tr><w:tc><w:p><w:r><w:t>17.25</w:t></w:r></w:p></w:tc></w:tr>{}</w:tbl>",marked("Design|Design"),marked("Quantity|Mass, kg"),marked("Unrelated|Later"));
        let value = super::super::tests::converted(&table, &[]);
        let proof = &value["metadata"]["docx_table_headers"][0];
        assert_eq!(proof["status"], "bound_body_header");
        assert_eq!(
            proof["labels"],
            json!(["Design / Quantity", "Design / Mass, kg"])
        );
        let xml = value["text"].as_str().unwrap();
        assert!(xml.contains("<w:t>17.25</w:t>"));
        assert!(xml.contains("Unrelated") && !proof["labels"].to_string().contains("Unrelated"));
    }
    #[test]
    fn false_header_flag_does_not_claim_a_data_row() {
        let body = format!("<w:tbl><w:tblGrid><w:gridCol w:w=\"1000\"/><w:gridCol w:w=\"1000\"/></w:tblGrid>{}{}</w:tbl>",marked("Quantity|Mass"),marked("17.25|9").replace("<w:tblHeader/>","<w:tblHeader w:val=\"0\"/>"));
        let value = super::super::tests::converted(&body, &[]);
        assert_eq!(value["metadata"]["docx_table_headers"], json!([]));
    }
    #[test]
    fn an_entirely_marked_table_is_not_projected_into_column_names() {
        let body = format!("<w:tbl><w:tblGrid><w:gridCol w:w=\"1000\"/><w:gridCol w:w=\"1000\"/></w:tblGrid>{}{}</w:tbl>",marked("Quantity|Mass"),marked("17.25|9"));
        let value = super::super::tests::converted(&body, &[]);
        assert_eq!(value["metadata"]["docx_table_headers"], json!([]));
        assert!(value["text"].as_str().unwrap().contains("<w:t>17.25</w:t>"));
    }
}
