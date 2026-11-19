//! Bind running table captions only within their effective Word section.
//! Correspondence uses column boundaries in the same margin frame, never names,
//! column count alone, or a table elsewhere in the package.
use super::*;
mod body_headers;

#[derive(Default, Clone)]
struct Grid {
    widths: Vec<i64>,
    left: i64,
    aligned: bool,
    floating: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn table(widths: &[i64], cells: &[&str]) -> String {
        format!("<w:tbl><w:tblPr><w:tblW w:type=\"dxa\" w:w=\"{}\"/></w:tblPr><w:tblGrid>{}</w:tblGrid><w:tr>{}</w:tr></w:tbl>",widths.iter().sum::<i64>(),widths.iter().map(|w|format!("<w:gridCol w:w=\"{w}\"/>")).collect::<String>(),cells.iter().map(|s|format!("<w:tc><w:p><w:r><w:t>{s}</w:t></w:r></w:p></w:tc>")).collect::<String>())
    }
    pub(super) fn converted(body: &str, headers: &[(&str, &str)]) -> Value {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let rels = headers
            .iter()
            .map(|(id, _)| {
                format!("<Relationship Id=\"{id}\" Type=\"x/header\" Target=\"{id}.xml\"/>")
            })
            .collect::<String>();
        let mut members=vec![("word/document.xml".to_owned(),format!("<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"r\"><w:body>{body}</w:body></w:document>")),("word/_rels/document.xml.rels".into(),format!("<Relationships>{rels}</Relationships>"))];
        members.extend(headers.iter().map(|(id, content)| {
            (
                format!("word/{id}.xml"),
                format!("<w:hdr xmlns:w=\"w\">{content}</w:hdr>"),
            )
        }));
        for (name, text) in members {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zip, text.as_bytes()).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        super::super::convert(&bytes, &json!({"word_parser":"docling"}), &|_, prepared| {
            let mut zip = zip::ZipArchive::new(Cursor::new(prepared.unwrap())).unwrap();
            let mut xml = String::new();
            zip.by_name("word/document.xml")
                .unwrap()
                .read_to_string(&mut xml)
                .unwrap();
            Ok(json!({"text":xml}))
        })
        .unwrap()
    }
    #[test]
    fn effective_section_grid_binds_literal_captions_with_layout_rounding() {
        let header = table(&[1000, 2000], &["Количество", "Масса, кг"]);
        let body = format!(
            "{}<w:sectPr><w:headerReference w:type=\"default\" r:id=\"header\"/></w:sectPr>",
            table(&[1004, 1998], &["1", "69.0"])
        );
        let v = converted(&body, &[("header", &header)]);
        let proof = &v["metadata"]["docx_table_headers"];
        assert_eq!(proof[0]["status"], "bound");
        assert_eq!(proof[0]["labels"], json!(["Количество", "Масса, кг"]));
        let prepared = v["text"].as_str().unwrap();
        assert!(prepared.find("Количество").unwrap() < prepared.find("<w:t>1</w:t>").unwrap());
        assert!(prepared.contains("<w:tblHeader/>") && prepared.contains("<w:t>69.0</w:t>"));
    }
    #[test]
    fn equal_column_count_without_matching_geometry_is_not_a_schema() {
        let header = table(&[1000, 2000], &["Quantity", "Mass"]);
        let body = format!(
            "{}<w:sectPr><w:headerReference r:id=\"header\"/></w:sectPr>",
            table(&[2000, 1000], &["1", "69.0"])
        );
        assert_eq!(
            converted(&body, &[("header", &header)])["metadata"]["docx_table_headers"],
            json!([])
        );
    }
    #[test]
    fn conflicting_active_variants_are_preserved_as_ambiguous() {
        let a = table(&[1000, 2000], &["Quantity", "Mass"]);
        let b = table(&[1000, 2000], &["Mass", "Quantity"]);
        let body=format!("{}<w:sectPr><w:titlePg/><w:headerReference w:type=\"default\" r:id=\"a\"/><w:headerReference w:type=\"first\" r:id=\"b\"/></w:sectPr>",table(&[1000,2000],&["1","69.0"]));
        assert_eq!(
            converted(&body, &[("a", &a), ("b", &b)])["metadata"]["docx_table_headers"][0]
                ["status"],
            "ambiguous_section_headers"
        );
    }
    #[test]
    fn section_properties_apply_to_preceding_tables_and_empty_sections_inherit() {
        let a = table(&[1000, 2000], &["Quantity", "Mass"]);
        let b = table(&[1000, 2000], &["Power", "Voltage"]);
        let row = table(&[1000, 2000], &["1", "69.0"]);
        let body=format!("{row}<w:p><w:pPr><w:sectPr><w:headerReference r:id=\"a\"/></w:sectPr></w:pPr></w:p>{row}<w:p><w:pPr><w:sectPr/></w:pPr></w:p>{row}<w:sectPr><w:headerReference r:id=\"b\"/></w:sectPr>");
        let value = converted(&body, &[("a", &a), ("b", &b)]);
        let proofs = value["metadata"]["docx_table_headers"].as_array().unwrap();
        assert_eq!(proofs.len(), 3);
        assert_eq!(proofs[0]["labels"], proofs[1]["labels"]);
        assert_ne!(proofs[1]["labels"], proofs[2]["labels"]);
    }
}
impl Grid {
    fn new() -> Self {
        Self {
            aligned: true,
            ..Default::default()
        }
    }
    fn property(&mut self, e: &BytesStart<'_>) {
        match e.name().as_ref() {
            "w:tblInd" => {
                self.aligned &= attribute(e, "w:type").is_none_or(|s| s == "dxa");
                if !self.floating {
                    self.left = attribute(e, "w:w")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                }
            }
            "w:jc" => {
                self.aligned &=
                    attribute(e, "w:val").is_none_or(|s| matches!(s.as_str(), "left" | "start"))
            }
            "w:tblpPr" => {
                self.floating = true;
                self.aligned &= attribute(e, "w:horzAnchor").is_none_or(|s| s == "margin")
                    && attribute(e, "w:tblpXSpec").is_none();
                if let Some(x) = attribute(e, "w:tblpX").and_then(|s| s.parse().ok()) {
                    self.left = x;
                } else {
                    self.aligned = false;
                }
            }
            _ => {}
        }
    }
    fn compatible(&self, other: &Self) -> Option<i64> {
        if !self.aligned
            || !other.aligned
            || self.widths.len() < 2
            || self.widths.len() != other.widths.len()
        {
            return None;
        }
        let narrow = *self.widths.iter().chain(&other.widths).min()?;
        if narrow <= 0 {
            return None;
        }
        let (mut a, mut b, mut delta) = (
            self.left,
            other.left,
            self.left.checked_sub(other.left)?.checked_abs()?,
        );
        for (x, y) in self.widths.iter().zip(&other.widths) {
            a = a.checked_add(*x)?;
            b = b.checked_add(*y)?;
            delta = delta.max(a.checked_sub(b)?.checked_abs()?);
        }
        // Every boundary must remain in its own column neighborhood. Small
        // Word layout rounding is allowed, while a different grid is rejected.
        (delta.checked_mul(4)? < narrow).then_some(delta)
    }
}
struct BodyTable {
    grid: Grid,
    insert: Option<usize>,
    explicit: bool,
    ordinal: usize,
    header_rows: body_headers::LeadingRows,
}
struct Caption {
    grid: Grid,
    labels: Vec<String>,
    part: String,
}
fn child<'a>(n: &'a Node, name: &str) -> Option<&'a Node> {
    n.children.iter().find(|n| n.name == name)
}
fn descendants(n: &Node, name: &str) -> bool {
    n.name == name || n.children.iter().any(|n| descendants(n, name))
}
fn literal(n: &Node) -> String {
    if n.name == "w:t" {
        return n.text.clone();
    }
    if matches!(n.name.as_str(), "w:br" | "w:tab") {
        return " ".into();
    }
    let mut text = n.children.iter().map(literal).collect::<Vec<_>>().join("");
    if n.name == "w:p" {
        text.push('\n');
    }
    text
}
fn normalized(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn caption(table: &Node, part: &str) -> Option<Caption> {
    if [
        "w:drawing",
        "w:pict",
        "w:object",
        "w:fldChar",
        "w:instrText",
        "m:oMath",
        "m:oMathPara",
        "w:sym",
        "w:footnoteReference",
        "w:endnoteReference",
        "w:tbl",
    ]
    .iter()
    .any(|name| {
        if *name == "w:tbl" {
            table.children.iter().any(|n| descendants(n, name))
        } else {
            descendants(table, name)
        }
    }) {
        return None;
    }
    let mut grid = Grid::new();
    if let Some(properties) = child(table, "w:tblPr") {
        for p in &properties.children {
            let mut e = BytesStart::new(p.name.as_str());
            for (k, v) in &p.attrs {
                e.push_attribute((k.as_str(), v.as_str()));
            }
            grid.property(&e);
        }
    }
    for col in &child(table, "w:tblGrid")?.children {
        if col.name == "w:gridCol" {
            grid.widths.push(col.attrs.get("w:w")?.parse().ok()?);
        }
    }
    let width = grid.widths.len();
    if !(2..=128).contains(&width) {
        return None;
    }
    let mut labels = vec![Vec::<String>::new(); width];
    let mut previous = vec![String::new(); width];
    for row in table.children.iter().filter(|n| n.name == "w:tr") {
        let mut cells = Vec::new();
        if let Some(pr) = child(row, "w:trPr") {
            if child(pr, "w:gridBefore").is_some() || child(pr, "w:gridAfter").is_some() {
                return None;
            }
        }
        for cell in row.children.iter().filter(|n| n.name == "w:tc") {
            let properties = child(cell, "w:tcPr");
            let span = properties
                .and_then(|n| child(n, "w:gridSpan"))
                .and_then(|n| n.attrs.get("w:val"))
                .map_or(Some(1), |s| s.parse::<usize>().ok())?;
            if span == 0 || cells.len().checked_add(span)? > width {
                return None;
            }
            let merge = properties.and_then(|n| child(n, "w:vMerge"));
            let continued =
                merge.is_some_and(|n| n.attrs.get("w:val").is_none_or(|s| s == "continue"));
            let text = normalized(&literal(cell));
            for _ in 0..span {
                cells.push(if continued {
                    previous[cells.len()].clone()
                } else {
                    text.clone()
                });
            }
        }
        if cells.len() != width {
            return None;
        }
        // An explicit ordinal row carries column addresses, not captions.
        if cells
            .iter()
            .enumerate()
            .all(|(i, s)| s.trim() == (i + 1).to_string())
        {
            continue;
        }
        for (i, s) in cells.iter().enumerate() {
            if !s.is_empty() && !labels[i].contains(s) {
                labels[i].push(s.clone());
            }
        }
        previous = cells;
    }
    let labels = labels
        .into_iter()
        .map(|v| v.join(" / "))
        .collect::<Vec<_>>();
    if labels
        .iter()
        .filter(|s| s.chars().any(char::is_alphabetic))
        .count()
        < 2
        || labels.iter().any(|s| s.len() > 2048)
    {
        return None;
    }
    Some(Caption {
        grid,
        labels,
        part: part.into(),
    })
}
fn enabled(n: &Node) -> bool {
    n.attrs
        .get("w:val")
        .is_none_or(|v| !matches!(v.as_str(), "0" | "false" | "off"))
}
fn row_xml(labels: &[String], widths: &[i64]) -> String {
    let mut row = "<w:tr><w:trPr><w:tblHeader/></w:trPr>".to_owned();
    for (label, width) in labels.iter().zip(widths) {
        row.push_str(&format!("<w:tc><w:tcPr><w:tcW w:type=\"dxa\" w:w=\"{width}\"/></w:tcPr><w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p></w:tc>",quick_xml::escape::escape(label)));
    }
    row.push_str("</w:tr>");
    row
}

impl<F: Fn(&Value, Option<&[u8]>) -> Result<Value>> Package<'_, F> {
    pub(super) fn section_headers(&mut self, xml: &[u8]) -> Result<(Vec<u8>, Vec<Value>)> {
        let even = if self.archive.by_name("word/settings.xml").is_ok() {
            let settings = self.member("word/settings.xml")?;
            let mut r = Reader::from_reader(settings.as_slice());
            let mut on = false;
            loop {
                match r.read_event().map_err(|_| BAD)? {
                    Event::Start(e) | Event::Empty(e)
                        if e.name().as_ref() == "w:evenAndOddHeaders" =>
                    {
                        on = attribute(&e, "w:val")
                            .is_none_or(|s| !matches!(s.as_str(), "0" | "false" | "off"))
                    }
                    Event::Eof => break,
                    Event::DocType(_) => return Err(BAD),
                    _ => {}
                }
            }
            on
        } else {
            false
        };
        let mut reader = Reader::from_reader(xml);
        let mut stack = Vec::<String>::new();
        let mut tables = Vec::new();
        let mut active: BTreeMap<String, String> = BTreeMap::new();
        let mut current: Option<BodyTable> = None;
        let mut ordinal = 0;
        let mut insertions = Vec::<(usize, String)>::new();
        let mut bindings = Vec::new();
        loop {
            let position = reader.buffer_position() as usize;
            let event = reader.read_event().map_err(|_| BAD)?;
            match &event {
                Event::Start(e) | Event::Empty(e)
                    if e.name().as_ref() == "w:sectPr"
                        && !stack
                            .iter()
                            .any(|s| matches!(s.as_str(), "w:tbl" | "w:txbxContent")) =>
                {
                    let section = if matches!(event, Event::Empty(_)) {
                        Node::empty(e)
                    } else {
                        Node::read(&mut reader, e)
                    }
                    .ok_or(BAD)?;
                    self.bind_section(
                        &section,
                        even,
                        &mut active,
                        &mut tables,
                        &mut insertions,
                        &mut bindings,
                    )?;
                    continue;
                }
                Event::Start(e) | Event::Empty(e) => {
                    let name = e.name().as_ref().to_owned();
                    if name == "w:tbl"
                        && current.is_none()
                        && stack.iter().any(|s| s == "w:body")
                        && !stack.iter().any(|s| s == "w:txbxContent")
                    {
                        current = Some(BodyTable {
                            grid: Grid::new(),
                            insert: None,
                            explicit: false,
                            ordinal,
                            header_rows: body_headers::LeadingRows::default(),
                        });
                        ordinal += 1;
                    } else if let Some(table) = current.as_mut() {
                        let depths = stack.iter().filter(|s| *s == "w:tbl").count();
                        if depths == 1 {
                            table.header_rows.start(
                                e,
                                &stack,
                                position,
                                matches!(event, Event::Empty(_)),
                            );
                            if name == "w:gridCol" && stack.last().is_some_and(|s| s == "w:tblGrid")
                            {
                                table.grid.widths.push(
                                    attribute(e, "w:w")
                                        .and_then(|s| s.parse::<i64>().ok())
                                        .ok_or(BAD)?,
                                );
                            }
                            if stack.last().is_some_and(|s| s == "w:tblPr") {
                                table.grid.property(e);
                            }
                            if name == "w:tr" && table.insert.is_none() {
                                table.insert = Some(position);
                            }
                            if name == "w:tblHeader"
                                && attribute(e, "w:val")
                                    .is_none_or(|s| !matches!(s.as_str(), "0" | "false" | "off"))
                            {
                                table.explicit = true;
                            }
                        }
                    }
                    if matches!(event, Event::Start(_)) {
                        stack.push(name);
                    }
                }
                Event::End(e) => {
                    let name = e.name().as_ref().to_owned();
                    if let Some(table) = current.as_mut() {
                        table
                            .header_rows
                            .end(&name, &stack, reader.buffer_position() as usize);
                    }
                    if stack.pop().as_deref() != Some(name.as_str()) {
                        return Err(BAD);
                    }
                    if name == "w:tbl" && !stack.iter().any(|s| s == "w:tbl") {
                        if let Some(t) = current.take() {
                            if let Some(labels) = t.header_rows.labels(xml, &t.grid) {
                                insertions
                                    .push((t.insert.ok_or(BAD)?, row_xml(&labels, &t.grid.widths)));
                                bindings.push(json!({"table":t.ordinal,"status":"bound_body_header","schema":"les.docx-leading-header.v1","labels":labels}));
                            }
                            tables.push(t);
                        }
                    }
                }
                Event::Eof => break,
                Event::DocType(_) => return Err(BAD),
                _ => {}
            }
        }
        if !stack.is_empty() {
            return Err(BAD);
        }
        if !tables.is_empty() {
            let empty = Node {
                name: "w:sectPr".into(),
                attrs: BTreeMap::new(),
                children: Vec::new(),
                text: String::new(),
            };
            self.bind_section(
                &empty,
                even,
                &mut active,
                &mut tables,
                &mut insertions,
                &mut bindings,
            )?;
        }
        insertions.sort_by_key(|v| v.0);
        let mut out = Vec::with_capacity(xml.len());
        let mut at = 0;
        for (position, row) in insertions {
            if position < at || position > xml.len() {
                return Err(BAD);
            }
            out.extend_from_slice(&xml[at..position]);
            out.extend_from_slice(row.as_bytes());
            at = position;
        }
        out.extend_from_slice(&xml[at..]);
        if out.len() > MAX_MAIN_XML as usize + 8 * 1024 * 1024 {
            return Err(LIMIT);
        }
        Ok((out, bindings))
    }
    fn bind_section(
        &mut self,
        section: &Node,
        even: bool,
        active: &mut BTreeMap<String, String>,
        tables: &mut Vec<BodyTable>,
        insertions: &mut Vec<(usize, String)>,
        bindings: &mut Vec<Value>,
    ) -> Result<()> {
        for reference in section
            .children
            .iter()
            .filter(|n| n.name == "w:headerReference")
        {
            let kind = reference
                .attrs
                .get("w:type")
                .cloned()
                .unwrap_or_else(|| "default".into());
            let path = self.target(reference.attrs.get("r:id").ok_or(BAD)?, "/header")?;
            active.insert(kind, path);
        }
        let first = child(section, "w:titlePg").is_some_and(enabled);
        let selected = active
            .iter()
            .filter(|(k, _)| {
                k.as_str() == "default"
                    || (k.as_str() == "even" && even)
                    || (k.as_str() == "first" && first)
            })
            .map(|(_, p)| p.clone())
            .collect::<Vec<_>>();
        let mut stories = Vec::new();
        for path in selected {
            let bytes = self.member(&path)?;
            let mut reader = Reader::from_reader(bytes.as_slice());
            let mut captions = Vec::new();
            loop {
                match reader.read_event().map_err(|_| BAD)? {
                    Event::Start(e) if e.name().as_ref() == "w:tbl" => {
                        let table = Node::read(&mut reader, &e).ok_or(BAD)?;
                        if let Some(c) = caption(&table, &path) {
                            captions.push(c);
                        }
                    }
                    Event::Eof => break,
                    Event::DocType(_) => return Err(BAD),
                    _ => {}
                }
            }
            stories.push(captions);
        }
        for table in tables.drain(..) {
            if table.explicit || table.insert.is_none() || stories.is_empty() {
                continue;
            }
            let candidates = stories
                .iter()
                .map(|s| {
                    s.iter()
                        .filter(|h| h.grid.compatible(&table.grid).is_some())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            if candidates.iter().any(|s| s.len() != 1) {
                continue;
            }
            let head = candidates[0][0];
            if candidates.iter().any(|s| s[0].labels != head.labels) {
                bindings.push(json!({"table":table.ordinal,"status":"ambiguous_section_headers"}));
                continue;
            }
            insertions.push((
                table.insert.unwrap(),
                row_xml(&head.labels, &table.grid.widths),
            ));
            bindings.push(json!({"table":table.ordinal,"status":"bound","schema":"les.docx-section-grid.v1","parts":candidates.iter().map(|s|s[0].part.clone()).collect::<Vec<_>>(),"column_widths_twips":table.grid.widths,"max_boundary_delta_twips":head.grid.compatible(&table.grid),"labels":head.labels}));
        }
        Ok(())
    }
}
