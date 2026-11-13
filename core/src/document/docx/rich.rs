//! Bounded semantic subtrees. Unknown formula operators and picture content fail closed.
use super::*;
use quick_xml::{events::BytesText, Writer};

pub(super) struct Node {
    pub(super) name: String,
    pub(super) attrs: BTreeMap<String, String>,
    pub(super) children: Vec<Node>,
    pub(super) text: String,
}

impl Node {
    pub(super) fn read<R: BufRead>(reader: &mut Reader<R>, start: &BytesStart<'_>) -> Option<Self> {
        Self::capture(reader, start, 0, &mut 0)
    }

    fn capture<R: BufRead>(
        reader: &mut Reader<R>,
        start: &BytesStart<'_>,
        depth: usize,
        count: &mut usize,
    ) -> Option<Self> {
        if depth > 48 || *count >= 100_000 {
            return None;
        }
        *count += 1;
        let mut node = Self::empty(start)?;
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            match reader.read_event_into(&mut buffer).ok()? {
                Event::Start(e) => node
                    .children
                    .push(Self::capture(reader, &e, depth + 1, count)?),
                Event::Empty(e) => {
                    *count += 1;
                    if *count > 100_000 {
                        return None;
                    }
                    node.children.push(Self::empty(&e)?);
                }
                Event::Text(t) => node.text.push_str(&t.xml_content(XmlVersion::Implicit1_0)),
                Event::GeneralRef(r) => {
                    if let Some(ch) = r.resolve_char_ref().ok()? {
                        node.text.push(ch);
                    } else {
                        node.text.push_str(resolve_predefined_entity(r.as_ref())?);
                    }
                }
                Event::End(e) if e.name().as_ref() == node.name => break,
                Event::Comment(_) => {}
                _ => return None,
            }
        }
        if !node.children.is_empty() && !node.text.trim().is_empty() {
            return None;
        }
        Some(node)
    }

    pub(super) fn empty(start: &BytesStart<'_>) -> Option<Self> {
        let mut attrs = BTreeMap::new();
        for attr in start.attributes() {
            let attr = attr.ok()?;
            if attrs
                .insert(
                    attr.key.as_ref().to_owned(),
                    quick_xml::escape::unescape(&attr.value).ok()?.into_owned(),
                )
                .is_some()
            {
                return None;
            }
        }
        Some(Self {
            name: start.name().as_ref().to_owned(),
            attrs,
            children: Vec::new(),
            text: String::new(),
        })
    }

    pub(super) fn write(&self, writer: &mut Writer<Vec<u8>>) -> Option<()> {
        let mut start = BytesStart::new(self.name.as_str());
        for (key, value) in &self.attrs {
            start.push_attribute((key.as_str(), quick_xml::escape::escape(value).as_ref()));
        }
        writer.write_event(Event::Start(start)).ok()?;
        writer
            .write_event(Event::Text(BytesText::new(&self.text)))
            .ok()?;
        for child in &self.children {
            child.write(writer)?;
        }
        writer
            .write_event(Event::End(quick_xml::events::BytesEnd::new(
                self.name.as_str(),
            )))
            .ok()?;
        Some(())
    }

    fn contains_numbering(&self) -> bool {
        self.name == "w:numPr" || self.children.iter().any(Self::contains_numbering)
    }

    fn arguments(&self, names: &[&str], properties: &[&str]) -> Option<Vec<String>> {
        let mut result = Vec::new();
        let mut actual = Vec::new();
        for child in &self.children {
            if properties.contains(&child.name.as_str()) {
                child.formatting()?;
            } else {
                actual.push(child.name.as_str());
                result.push(child.math()?);
            }
        }
        (actual == names).then_some(result)
    }

    fn formatting(&self) -> Option<()> {
        // Only appearance properties; semantic settings such as fraction type, accents,
        // delimiters, n-ary operators or hidden degrees need a dedicated renderer.
        match self.name.as_str() {
            "m:rPr" | "m:ctrlPr" | "w:rPr" | "m:sSupPr" | "m:sSubPr" | "m:sSubSupPr"
            | "m:oMathParaPr" | "m:fPr" | "m:funcPr" | "m:radPr" | "m:eqArrPr" => {
                for child in &self.children {
                    child.formatting()?;
                }
            }
            "m:type"
                if self.children.is_empty()
                    && self.attrs.get("m:val").is_some_and(|v| v == "bar") => {}
            "m:degHide"
                if self.children.is_empty()
                    && self.attrs.get("m:val").is_some_and(|v| {
                        matches!(v.as_str(), "0" | "1" | "true" | "false" | "on" | "off")
                    }) => {}
            "w:rFonts" | "w:b" | "w:i" | "w:bCs" | "w:iCs" | "w:sz" | "w:szCs" | "w:color"
            | "w:lang" | "w:noProof" | "m:sty" | "m:scr" | "m:nor" | "m:jc"
                if self.children.is_empty() => {}
            _ => return None,
        }
        self.text.trim().is_empty().then_some(())
    }

    pub(super) fn math(&self) -> Option<String> {
        let result = match self.name.as_str() {
            "w:br" if self.children.is_empty() => "\n".into(),
            "w:tab" if self.children.is_empty() => "\t".into(),
            "m:t" if self.children.is_empty() => self.text.clone(),
            "m:oMath" | "m:oMathPara" | "m:r" | "m:e" | "m:sub" | "m:sup" | "m:num" | "m:den"
            | "m:deg" | "m:fName" => {
                let mut result = String::new();
                for child in &self.children {
                    if matches!(child.name.as_str(), "m:rPr" | "w:rPr" | "m:oMathParaPr") {
                        child.formatting()?;
                    } else {
                        result.push_str(&child.math()?);
                    }
                }
                result
            }
            "m:sSup" => {
                let a = self.arguments(&["m:e", "m:sup"], &["m:sSupPr"])?;
                if a[0].is_empty() {
                    return None;
                }
                format!("({})^({})", a[0], a[1])
            }
            "m:sSub" => {
                let a = self.arguments(&["m:e", "m:sub"], &["m:sSubPr"])?;
                if a[0].is_empty() {
                    return None;
                }
                format!("({})_({})", a[0], a[1])
            }
            "m:sSubSup" => {
                let a = self.arguments(&["m:e", "m:sub", "m:sup"], &["m:sSubSupPr"])?;
                if a[0].is_empty() {
                    return None;
                }
                format!("({})_({})^({})", a[0], a[1], a[2])
            }
            "m:f" => {
                // Appearance-only properties and explicit bar keep the default quotient.
                // Linear/no-bar/skewed fractions still require a separate interpretation.
                let a = self.arguments(&["m:num", "m:den"], &["m:fPr"])?;
                if a.iter().any(String::is_empty) {
                    return None;
                }
                format!("({})/({})", a[0], a[1])
            }
            "m:func" => {
                let a = self.arguments(&["m:fName", "m:e"], &["m:funcPr"])?;
                if a[0].is_empty() {
                    return None;
                }
                format!("{}({})", a[0], a[1])
            }
            "m:rad" => {
                let a = self.arguments(&["m:deg", "m:e"], &["m:radPr"])?;
                if a[1].is_empty() {
                    return None;
                }
                let hidden = self
                    .children
                    .iter()
                    .find(|n| n.name == "m:radPr")
                    .and_then(|p| p.children.iter().find(|n| n.name == "m:degHide"))
                    .and_then(|n| n.attrs.get("m:val"))
                    .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "on"));
                if hidden && !a[0].is_empty() {
                    return None;
                }
                if a[0].is_empty() {
                    format!("sqrt({})", a[1])
                } else {
                    format!("root({};{})", a[0], a[1])
                }
            }
            "m:eqArr" => {
                let mut rows = Vec::new();
                for child in &self.children {
                    if child.name == "m:eqArrPr" {
                        child.formatting()?;
                    } else if child.name == "m:e" {
                        rows.push(child.math()?);
                    } else {
                        return None;
                    }
                }
                if rows.is_empty() {
                    return None;
                }
                format!("aligned[{}]", rows.join(" ; "))
            }
            "m:d" => {
                let mut begin = "(".to_owned();
                let mut end = ")".to_owned();
                let mut separator = "|".to_owned();
                let mut slots = Vec::new();
                for child in &self.children {
                    if child.name == "m:dPr" {
                        for p in &child.children {
                            match p.name.as_str() {
                                "m:ctrlPr" => p.formatting()?,
                                "m:begChr" | "m:endChr" | "m:sepChr" if p.children.is_empty() => {
                                    let s = p.attrs.get("m:val")?;
                                    if s.chars().count() > 1 {
                                        return None;
                                    }
                                    match p.name.as_str() {
                                        "m:begChr" => begin = s.clone(),
                                        "m:endChr" => end = s.clone(),
                                        _ => separator = s.clone(),
                                    }
                                }
                                _ => return None,
                            }
                        }
                    } else if child.name == "m:e" {
                        slots.push(child.math()?);
                    } else {
                        return None;
                    }
                }
                if slots.is_empty() {
                    return None;
                }
                format!("{begin}{}{end}", slots.join(&separator))
            }
            _ => return None,
        };
        (!result.is_empty()
            || result.contains(['\n', '\t'])
            || matches!(self.name.as_str(), "m:deg" | "m:sup" | "m:sub" | "m:e"))
        .then_some(result)
    }

    pub(super) fn drawing(&self, document: &mut Document) -> Option<String> {
        match self.name.as_str() {
            "w:txbxContent" => {
                // A textbox is a separate Word story. Sharing the body's counter would
                // guess its list labels; keep this boundary explicit until story lists exist.
                if self.contains_numbering() {
                    return None;
                }
                let mut writer = Writer::new(Vec::new());
                for child in &self.children {
                    child.write(&mut writer)?;
                }
                let xml = writer.into_inner();
                let mut nested = Document {
                    styles: document.styles.clone(),
                    body: true,
                    ..Document::default()
                };
                parse_into(&xml, &mut nested).ok()?;
                if nested.paragraph || nested.table.is_some() || !nested.properties.is_empty() {
                    return None;
                }
                Some(nested.paragraphs.join("\n\n"))
            }
            "w:pict" | "v:shape" | "v:textbox" => {
                let mut boxes = Vec::new();
                for child in &self.children {
                    boxes.push(child.drawing(document)?);
                }
                let text = boxes
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                (!text.is_empty()).then_some(text)
            }
            // No image, OLE, arbitrary VML path, chart or external reference is discarded.
            _ => None,
        }
    }
}
