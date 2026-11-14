//! Resolve only internal OPC relationships; read real embedded objects, never their preview.
use super::*;
use rich::Node;
mod docling;
type Result<T> = std::result::Result<T, &'static str>;
const BAD: &str = "DOCX_PART_INVALID";
const LIMIT: &str = "DOCX_PART_LIMIT";
const MAX_PART: u64 = 32 * 1024 * 1024;

pub(in crate::document) fn convert(
    bytes: &[u8],
    request: &Value,
    external: &impl Fn(&Value, Option<&[u8]>) -> Result<Value>,
) -> Result<Value> {
    let archive = if request["word_parser"] == "docling" {
        super::super::conversion::archive(bytes)?
    } else {
        ZipArchive::new(Cursor::new(bytes)).map_err(|_| "DOCUMENT_ARCHIVE_INVALID")?
    };
    let mut input = request.clone();
    input["source_sha256"] = json!(crate::sha256_hex(bytes));
    let mut package = Package {
        archive,
        request: &input,
        external,
        relationships: BTreeMap::new(),
        assets: BTreeMap::new(),
        ocr: false,
        visual: false,
        total: 0,
        picture_options: Value::Null,
        seen: std::collections::HashSet::new(),
        references: Vec::new(),
        note_depth: 0,
        unread_visual: 0,
        pictures: BTreeMap::new(),
        floating: Vec::new(),
    };
    package.relationships = package.rels("word/_rels/document.xml.rels")?;
    let mut text = if request["word_parser"] == "docling" {
        package.docling()?
    } else {
        let mut text = diagnostic::checked_with(bytes, &mut package)?;
        package.read_related(&mut text)?;
        text
    };
    // An anchored shape belongs to a separate layout story. Its XML insertion
    // point is not a textual position inside a word or a table cell.
    for (index, (_, content)) in package.floating.iter().enumerate() {
        if content.trim().is_empty() {
            continue;
        }
        text.push_str(&format!("\n\n## Отдельный плавающий объект {}\n\nТекст объекта оригинала; не продолжение абзаца и не ячейка таблицы.\n\n{}",index+1,content));
    }
    if text.len() > MAX_TEXT {
        return Err("DOCUMENT_TEXT_LIMIT");
    }
    let manifest = json!({"schema":"les.docx-parts.v1","source_sha256":crate::sha256_hex(bytes),
        "parts":package.assets.values().collect::<Vec<_>>(),"ocr":package.ocr,"visual_content":package.visual,"unread_visual_count":package.unread_visual});
    if !package.assets.is_empty() {
        package.call("source", "document.docx", bytes, &json!({}))?;
        package.call(
            "manifest",
            "manifest.json",
            &serde_json::to_vec(&manifest).map_err(|_| BAD)?,
            &json!({}),
        )?;
    }
    Ok(
        json!({"text":text,"metadata":{"extractor":if request["word_parser"]=="docling"{"les.docling-word.parts.v1"}else{"les.docx-parts.v1"},"ocr":package.ocr,
        "visual_content":package.visual,"docx_parts_count":package.assets.len(),
        "floating_objects":package.floating.iter().map(|(id,_)|json!({"part_sha256":id,"placement":"separate_story"})).collect::<Vec<_>>(),
        "unread_visual_count":package.unread_visual,"visual_coverage":"originals_and_structure_preserved_not_semantically_verified",
        "docx_parts_source_sha256":manifest["source_sha256"]}}),
    )
}

struct Package<'a, F: Fn(&Value, Option<&[u8]>) -> Result<Value>> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    request: &'a Value,
    external: &'a F,
    relationships: BTreeMap<String, (String, String)>,
    assets: BTreeMap<String, Value>,
    ocr: bool,
    visual: bool,
    total: u64,
    picture_options: Value,
    seen: std::collections::HashSet<String>,
    references: Vec<(String, String)>,
    note_depth: usize,
    unread_visual: usize,
    pictures: BTreeMap<String, (String, Value)>,
    floating: Vec<(String, String)>,
}

impl<F: Fn(&Value, Option<&[u8]>) -> Result<Value>> Package<'_, F> {
    fn member(&mut self, name: &str) -> Result<Vec<u8>> {
        let member = self.archive.by_name(name).map_err(|_| BAD)?;
        let additional = if self.seen.contains(name) {
            0
        } else {
            member.size()
        };
        if member.size() > MAX_PART
            || self
                .total
                .checked_add(additional)
                .is_none_or(|n| n > 128 * 1024 * 1024)
        {
            return Err(LIMIT);
        }
        self.total += additional;
        self.seen.insert(name.into());
        let mut bytes = Vec::new();
        member
            .take(MAX_PART + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| BAD)?;
        if bytes.len() as u64 > MAX_PART {
            return Err(LIMIT);
        }
        Ok(bytes)
    }
    fn rels(&mut self, name: &str) -> Result<BTreeMap<String, (String, String)>> {
        if self.archive.by_name(name).is_err() {
            return Ok(BTreeMap::new());
        }
        let bytes = self.member(name)?;
        let mut reader = Reader::from_reader(bytes.as_slice());
        let mut rels = BTreeMap::new();
        loop {
            match reader.read_event().map_err(|_| BAD)? {
                Event::Start(e) | Event::Empty(e) if e.name().as_ref() == "Relationship" => {
                    let id = attribute(&e, "Id").ok_or(BAD)?;
                    let target = attribute(&e, "Target").ok_or(BAD)?;
                    let kind = attribute(&e, "Type").ok_or(BAD)?;
                    // External relationships are retained in the package but never fetched.
                    let target = if attribute(&e, "TargetMode").as_deref() == Some("External") {
                        String::new()
                    } else {
                        target
                    };
                    if rels.insert(id, (target, kind)).is_some() {
                        return Err(BAD);
                    }
                }
                Event::DocType(_) => return Err(BAD),
                Event::Eof => break,
                _ => {}
            }
        }
        Ok(rels)
    }
    fn target(&self, id: &str, kind: &str) -> Result<String> {
        let (target, ty) = self.relationships.get(id).ok_or(BAD)?;
        if target.is_empty() {
            return Err("DOCX_EXTERNAL_PART_UNSUPPORTED");
        }
        if !ty.ends_with(kind) {
            return Err(BAD);
        }
        canonical(target)
    }
    fn call(&self, kind: &str, name: &str, bytes: &[u8], extra: &Value) -> Result<Value> {
        let mut input = self.request.clone();
        input["part_kind"] = json!(kind);
        input["part_name"] = json!(name);
        input["part_sha256"] = json!(crate::sha256_hex(bytes));
        input["part_options"] = extra.clone();
        (self.external)(&input, Some(bytes))
    }
    fn asset(&mut self, name: &str, kind: &str, bytes: &[u8], extra: &Value) -> Result<Value> {
        let value = self.call(kind, name, bytes, extra)?;
        let ocr = value["ocr"] == true || value["metadata"]["ocr"] == true;
        self.assets.entry(name.into()).or_insert_with(||json!({"member":name,"kind":kind,
            "sha256":crate::sha256_hex(bytes),"bytes":bytes.len(),"ocr":ocr,"coverage":value["coverage"],"preview_sha256":value["preview_sha256"],"text":value["text"],
            "native_text_runs":value["native_text_runs"],"recognition_method":value["recognition_method"]}));
        if let Some(preview) = value["preview_sha256"].as_str() {
            let entry = self.assets.get_mut(name).ok_or(BAD)?;
            if entry["views"].is_null() {
                entry["views"] = json!([]);
            }
            let views = entry["views"].as_array_mut().ok_or(BAD)?;
            if !views.iter().any(|v| v["sha256"] == preview) {
                if views.len() >= 1024 {
                    return Err(LIMIT);
                }
                views.push(json!({"sha256":preview,"options":extra,"text":value["text"],"ocr":value["ocr"],"coverage":value["coverage"],
                    "native_text_runs":value["native_text_runs"],"recognition_method":value["recognition_method"]}));
            }
        }
        self.ocr |= ocr;
        Ok(value)
    }
    fn children(&mut self, node: &Node, document: &mut Document) -> Result<String> {
        let mut text = Vec::new();
        for child in &node.children {
            let s = self.node(child, document)?;
            if !s.is_empty() {
                text.push(s);
            }
        }
        Ok(text.join("\n\n"))
    }
    fn textbox(&mut self, node: &Node, document: &mut Document) -> Result<String> {
        let mut writer = quick_xml::Writer::new(Vec::new());
        for child in &node.children {
            child.write(&mut writer).ok_or(BAD)?;
        }
        let xml = writer.into_inner();
        let mut inner = Document {
            styles: document.styles.clone(),
            numbering: document.numbering.fresh(),
            body: true,
            ..Document::default()
        };
        parse_events(&mut Reader::from_reader(xml.as_slice()), &mut inner, self)?;
        if inner.paragraph || inner.table.is_some() || !inner.properties.is_empty() {
            return Err(BAD);
        }
        Ok(inner.paragraphs.join("\n\n"))
    }
    fn picture(&mut self, node: &Node) -> Result<String> {
        let id = node
            .attrs
            .get("r:embed")
            .or_else(|| node.attrs.get("r:id"))
            .ok_or(BAD)?;
        let name = self.target(id, "/image")?;
        self.visual = true;
        let options = json!({"attributes":node.attrs,"picture":self.picture_options});
        let key = json!([name, options]).to_string();
        let (sha, value) = if let Some(value) = self.pictures.get(&key) {
            value.clone()
        } else {
            let bytes = self.member(&name)?;
            let sha = crate::sha256_hex(&bytes);
            let value = self.asset(&name, "image", &bytes, &options)?;
            if self.pictures.len() >= 4096 {
                return Err(LIMIT);
            }
            self.pictures.insert(key, (sha.clone(), value.clone()));
            (sha, value)
        };
        let text = value["text"].as_str().ok_or(BAD)?;
        if text.is_empty() && value["blank"] != true {
            self.unread_visual += 1;
            return Ok(format!("[Рисунок {name}; part_id={sha}; оригинал и видимое изображение сохранены. OCR не распознал текст. Это не пустой рисунок; содержимое требует визуальной проверки.]"));
        }
        Ok(if text.is_empty() {
            String::new()
        } else {
            format!(
                "[Текст рисунка {name}; part_id={}; {}, сверяйте с оригиналом]\n{text}",
                sha,
                if value["ocr"] == true {
                    "OCR"
                } else {
                    "текстовые записи оригинала"
                }
            )
        })
    }
    fn node(&mut self, node: &Node, document: &mut Document) -> Result<String> {
        match node.name.as_str() {
            "m:oMath" | "m:oMathPara" => node.math().ok_or("DOCX_EQUATION_UNSUPPORTED"),
            "w:object" => self.object(node),
            "w:txbxContent" => self.textbox(node, document),
            "w:footnoteReference" | "w:endnoteReference" | "w:commentReference" => {
                self.note(node, document)
            }
            "pic:pic" => {
                let previous = self.picture_options.clone();
                self.picture_options = json!({"crop":find(node,"a:srcRect").map(|n|&n.attrs),"transform":find(node,"a:xfrm").map(|n|&n.attrs)});
                let result = self.children(node, document);
                self.picture_options = previous;
                result
            }
            "a:blip" | "v:imagedata" => self.picture(node),
            "mc:AlternateContent" => {
                let choice = node
                    .children
                    .iter()
                    .find(|n| n.name == "mc:Choice")
                    .ok_or(BAD)?;
                if choice
                    .attrs
                    .get("Requires")
                    .is_none_or(|s| !s.split_whitespace().all(|s| matches!(s, "wps" | "wpg")))
                {
                    return Err("DOCX_DRAWING_UNSUPPORTED");
                }
                self.children(choice, document)
            }
            // The complete structure is persisted before interpretation, including
            // transforms, geometry, arrows, layout and appearance, not flattened away.
            "w:pict" | "w:drawing" | "wp:inline" | "wp:anchor" | "a:graphic" | "a:graphicData"
            | "pic:blipFill" | "wps:wsp" | "wps:txbx" | "wpg:wgp" | "wpg:grpSp" | "a:grpSp"
            | "v:group" | "v:shape" | "v:textbox" | "v:line" | "v:rect" | "v:oval" => {
                self.children(node, document)
            }
            s if formatting(s) => Ok(String::new()),
            _ => {
                eprintln!(
                    "{}",
                    json!({"event":"docx_unsupported_element","element":node.name})
                );
                Err("DOCX_DRAWING_UNSUPPORTED")
            }
        }
    }
}
impl<F: Fn(&Value, Option<&[u8]>) -> Result<Value>> Parts for Package<'_, F> {
    fn has_separate_text(&self) -> bool {
        self.floating.iter().any(|(_, s)| !s.trim().is_empty())
    }
    fn reference(&mut self, element: &BytesStart<'_>) -> Result<()> {
        let kind = if element.name().as_ref() == "w:headerReference" {
            "/header"
        } else {
            "/footer"
        };
        let name = self.target(&attribute(element, "r:id").ok_or(BAD)?, kind)?;
        if !self.references.iter().any(|v| v.0 == name) {
            self.references.push((name, kind.into()));
        }
        Ok(())
    }
    fn render(&mut self, node: &Node, document: &mut Document) -> Result<String> {
        let mut structure_id = None;
        if matches!(
            node.name.as_str(),
            "w:pict" | "w:drawing" | "w:object" | "mc:AlternateContent"
        ) {
            let mut ids = std::collections::BTreeSet::new();
            relationship_ids(node, &mut ids);
            let relationships = ids
                .into_iter()
                .filter_map(|id| self.relationships.get(&id).map(|v| (id, v)))
                .collect::<BTreeMap<_, _>>();
            let data = serde_json::to_vec(&json!({"schema":"les.docx-drawing.v1","root":model(node),"relationships":relationships})).map_err(|_| BAD)?;
            let id = crate::sha256_hex(&data);
            self.asset(
                &format!("structures/{id}.json"),
                "structure",
                &data,
                &json!({}),
            )?;
            self.visual = true;
            structure_id = Some(id);
        }
        let text = self.node(node, document)?;
        if floating(node) {
            self.floating.push((structure_id.ok_or(BAD)?, text));
            Ok(String::new())
        } else {
            Ok(text)
        }
    }
}
fn floating(node: &Node) -> bool {
    if node.name == "wp:anchor" {
        return true;
    }
    if node.name == "wp:inline" {
        return false;
    }
    if node.name == "mc:AlternateContent" {
        return node
            .children
            .iter()
            .find(|n| n.name == "mc:Choice")
            .is_some_and(floating);
    }
    if node.name == "v:shape"
        && node.attrs.get("style").is_some_and(|s| {
            s.split(';').any(|p| {
                p.split_once(':').is_some_and(|(k, v)| {
                    k.trim().eq_ignore_ascii_case("position")
                        && v.trim().eq_ignore_ascii_case("absolute")
                })
            })
        })
    {
        return true;
    }
    node.children.iter().any(floating)
}

fn canonical(target: &str) -> Result<String> {
    if target.contains(['\\', ':', '\0', '?', '#', '%']) {
        return Err(BAD);
    }
    let mut parts = if target.starts_with('/') {
        Vec::new()
    } else {
        vec!["word"]
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or(BAD)?;
            }
            s => parts.push(s),
        }
    }
    if parts.first() != Some(&"word") || parts.len() < 2 {
        return Err(BAD);
    }
    Ok(parts.join("/"))
}
fn find<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
    if node.name == name {
        Some(node)
    } else {
        node.children.iter().find_map(|c| find(c, name))
    }
}
fn model(node: &Node) -> Value {
    json!({"name":node.name,"attributes":node.attrs,"text":node.text,"children":node.children.iter().map(model).collect::<Vec<_>>()})
}
fn relationship_ids(node: &Node, ids: &mut std::collections::BTreeSet<String>) {
    for (key, value) in &node.attrs {
        if matches!(key.as_str(), "r:embed" | "r:id" | "r:link") {
            ids.insert(value.clone());
        }
    }
    for child in &node.children {
        relationship_ids(child, ids);
    }
}
fn formatting(name: &str) -> bool {
    matches!(
        name,
        "pic:nvPicPr"
            | "pic:spPr"
            | "wp:extent"
            | "wp:effectExtent"
            | "wp:docPr"
            | "wp:cNvGraphicFramePr"
            | "wp:simplePos"
            | "wp:positionH"
            | "wp:positionV"
            | "wp:wrapNone"
            | "wp:wrapSquare"
            | "wp:wrapTight"
            | "wp:wrapThrough"
            | "wp:wrapTopAndBottom"
            | "wp14:sizeRelH"
            | "wp14:sizeRelV"
            | "a:srcRect"
            | "a:stretch"
            | "a:extLst"
            | "wps:cNvSpPr"
            | "wps:cNvPr"
            | "wps:cNvCnPr"
            | "wps:spPr"
            | "wps:bodyPr"
            | "wps:style"
            | "wpg:cNvGrpSpPr"
            | "wpg:cNvPr"
            | "wpg:grpSpPr"
            | "a:grpSpPr"
            | "a:nvGrpSpPr"
            | "v:shapetype"
            | "v:stroke"
            | "v:path"
            | "v:fill"
            | "v:shadow"
            | "v:formulas"
            | "v:handles"
            | "o:lock"
            | "o:extrusion"
            | "o:skew"
            | "o:shadowObscured"
            | "w10:wrap"
            | "w10:anchorlock"
    )
}

mod equation;
mod related;
#[cfg(test)]
mod tests;

mod object;
mod ole_images;
