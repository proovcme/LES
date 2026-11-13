//! Read referenced Word stories, with their own relationships and numbering state.
use super::*;
impl<F: Fn(&Value, Option<&[u8]>) -> Result<Value>> Package<'_, F> {
    pub(super) fn read_related(&mut self, text: &mut String) -> Result<()> {
        let mut index = 0;
        while index < self.references.len() {
            if index >= 128 {
                return Err(LIMIT);
            }
            let (name, kind) = self.references[index].clone();
            index += 1;
            let bytes = self.member(&name)?;
            let mut reader = Reader::from_reader(bytes.as_slice());
            let root = loop {
                match reader.read_event().map_err(|_| BAD)? {
                    Event::Start(e) => break Node::read(&mut reader, &e).ok_or(BAD)?,
                    Event::Decl(_) | Event::Text(_) => {}
                    _ => return Err(BAD),
                }
            };
            if !matches!(root.name.as_str(), "w:hdr" | "w:ftr") {
                return Err(BAD);
            }
            let old = self.enter(&name)?;
            let mut document = Document {
                styles: styles::read(&mut self.archive).ok_or(BAD)?,
                numbering: numbering::Numbering::read(&mut self.archive).ok_or(BAD)?,
                ..Document::default()
            };
            let result = self.textbox(&root, &mut document);
            self.relationships = old;
            let story = result?;
            if !story.is_empty() {
                text.push_str(&format!(
                    "\n\n[{} {name}]\n{story}",
                    if kind == "/header" {
                        "Верхний колонтитул"
                    } else {
                        "Нижний колонтитул"
                    }
                ));
            }
            if text.len() > MAX_TEXT {
                return Err("DOCUMENT_TEXT_LIMIT");
            }
        }
        Ok(())
    }
    pub(super) fn enter(&mut self, name: &str) -> Result<BTreeMap<String, (String, String)>> {
        let leaf = name.rsplit('/').next().ok_or(BAD)?;
        let next = self.rels(&format!("word/_rels/{leaf}.rels"))?;
        Ok(std::mem::replace(&mut self.relationships, next))
    }
    pub(super) fn note(&mut self, node: &Node, document: &mut Document) -> Result<String> {
        if self.note_depth >= 8 {
            return Err(LIMIT);
        }
        let (file, tag, label) = match node.name.as_str() {
            "w:footnoteReference" => ("word/footnotes.xml", "w:footnote", "Сноска"),
            "w:endnoteReference" => ("word/endnotes.xml", "w:endnote", "Концевая сноска"),
            "w:commentReference" => ("word/comments.xml", "w:comment", "Комментарий"),
            _ => return Err(BAD),
        };
        let id = node.attrs.get("w:id").ok_or(BAD)?;
        if id.parse::<i64>().ok().is_none_or(|n| n < 0) {
            return Err(BAD);
        }
        let bytes = self.member(file)?;
        let mut reader = Reader::from_reader(bytes.as_slice());
        let mut selected = None;
        loop {
            match reader.read_event().map_err(|_| BAD)? {
                Event::Start(e) if e.name().as_ref() == tag => {
                    let note = Node::read(&mut reader, &e).ok_or(BAD)?;
                    if note.attrs.get("w:id") == Some(id) {
                        if selected.replace(note).is_some() {
                            return Err(BAD);
                        }
                    }
                }
                Event::DocType(_) => return Err(BAD),
                Event::Eof => break,
                _ => {}
            }
        }
        let note = selected.ok_or(BAD)?;
        let old = self.enter(file)?;
        self.note_depth += 1;
        let result = self.textbox(&note, document);
        self.note_depth -= 1;
        self.relationships = old;
        Ok(format!("[{label}, идентификатор {id}]\n{}", result?))
    }
}
