//! Borrow Docling's item tree. Markdown is never an input to this projection.
use super::*;
use docling_core::tree::{ItemTree, TreeKind};

pub(super) fn project(document: &docling::DoclingDocument) -> Result<Value> {
    let tree = document
        .tree
        .as_ref()
        .ok_or("DOCLING_WORD_STRUCTURE_MISSING")?;
    let mut blocks = Vec::new();
    for &id in &tree.body {
        visit(tree, id, &mut blocks, 0)?;
    }
    let groups=tree.items.iter().enumerate().filter(|(_,i)|!i.deleted).filter_map(|(id,item)|{
        if let TreeKind::Group{label,name}=&item.kind {
            Some(json!({"id":format!("docling:{id}"),"parent":item.parent.map(|p|format!("docling:{p}")),
                "label":label,"name":name,"children":item.children.iter().map(|id|format!("docling:{id}")).collect::<Vec<_>>()}))
        }else{None}
    }).collect::<Vec<_>>();
    Ok(json!({"schema":"les.document-structure.v1","blocks":blocks,"groups":groups}))
}

fn visit(tree: &ItemTree, id: usize, out: &mut Vec<Value>, depth: usize) -> Result<()> {
    if depth > 128 || out.len() > 100000 {
        return Err("DOCUMENT_STRUCTURE_LIMIT");
    }
    let item = tree.items.get(id).ok_or("DOCLING_WORD_INCOMPLETE")?;
    if item.deleted {
        return Ok(());
    }
    let mut block =
        json!({"id":format!("docling:{id}"),"parent":item.parent.map(|p|format!("docling:{p}"))});
    if let Some(p) = &item.prov {
        block["provenance"] = json!({"page":p.page_no,"bbox":p.bbox,"charspan":p.charspan,"bottom_left":p.bottom_left});
    }
    match &item.kind {
        TreeKind::Text {
            label,
            text,
            level,
            formatting,
            hyperlink,
            list,
            ..
        } => {
            block["kind"] = json!(if matches!(label.as_str(), "title" | "section_header") {
                "heading"
            } else {
                "paragraph"
            });
            block["label"] = json!(label);
            block["text"] = json!(text);
            block["level"] = json!(level.unwrap_or(1));
            block["hyperlink"] = json!(hyperlink);
            if let Some(f) = formatting {
                block["formatting"] = json!({"bold":f.bold,"italic":f.italic,"underline":f.underline,"strike":f.strikethrough,"script":format!("{:?}",f.script)});
            }
            if let Some(l) = list {
                block["list"] = json!({"enumerated":l.enumerated,"marker":l.marker});
            }
            out.push(block);
        }
        TreeKind::Code { text, language, .. } => {
            block["kind"] = json!("paragraph");
            block["label"] = json!("code");
            block["text"] = json!(text);
            block["language"] = json!(language);
            out.push(block);
        }
        TreeKind::Group { label, .. } if label == "inline" => {
            let mut runs = Vec::new();
            for &child in &item.children {
                visit(tree, child, &mut runs, depth + 1)?;
            }
            block["kind"] = json!("paragraph");
            block["runs"] = json!(runs);
            block["inline_whitespace"] = json!(tree.inline_whitespace);
            out.push(block);
            return Ok(());
        }
        TreeKind::Table {
            table, rich_cells, ..
        } => {
            block["kind"] = json!("table");
            block["rows"] = json!(table.rows.len());
            block["columns"] = json!(table.rows.iter().map(Vec::len).max().unwrap_or(0));
            let mut cells = Vec::new();
            let derived;
            let source_cells = if let Some(cells) = &table.cells {
                cells.as_slice()
            } else {
                derived = table.derive_cells();
                &derived
            };
            for cell in source_cells {
                let mut content = Vec::new();
                if let Some((_, _, group)) = rich_cells
                    .iter()
                    .find(|(r, c, _)| *r == cell.start_row && *c == cell.start_col)
                {
                    visit(tree, *group, &mut content, depth + 1)?;
                } else {
                    content.push(json!({"kind":"paragraph","text":cell.text}));
                }
                cells.push(json!({"row":cell.start_row,"column":cell.start_col,"row_span":cell.row_span,
                    "column_span":cell.col_span,"column_header":cell.column_header,"row_header":cell.row_header,
                    "row_section":cell.row_section,"bbox":cell.bbox,"content":content}));
            }
            block["cells"] = json!(cells);
            out.push(block);
            // Rich-cell children already have one owner above. Captions and
            // footnotes that are actual table children still belong to the body.
            for &child in &item.children {
                if !rich_cells.iter().any(|(_, _, g)| *g == child) {
                    visit(tree, child, out, depth + 1)?;
                }
            }
            return Ok(());
        }
        TreeKind::Group { .. } | TreeKind::Picture { .. } => {}
        _ => return Err("DOCLING_WORD_STRUCTURE_UNSUPPORTED"),
    }
    for &child in &item.children {
        visit(tree, child, out, depth + 1)?;
    }
    Ok(())
}
