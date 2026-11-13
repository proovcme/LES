use super::*;
use std::{cell::RefCell, io::Write};
fn archive(body: &str, members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let xml=format!("<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"r\" xmlns:v=\"v\" xmlns:mc=\"mc\" xmlns:wps=\"wps\"><w:body>{body}</w:body></w:document>");
    zip.start_file(
        "word/document.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(xml.as_bytes()).unwrap();
    for (name, bytes) in members {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
#[test]
fn empty_ocr_preserves_real_image_structure_and_revision_without_claiming_blank() {
    let rels=br#"<Relationships><Relationship Id="picture" Type="x/image" Target="media/proof.png"/><Relationship Id="unused" Type="x/customXml" Target="../customXml/item.xml"/></Relationships>"#;
    let bytes=archive("<w:p><w:r><w:t>Before</w:t><w:pict><v:shape><v:imagedata r:id=\"picture\"/></v:shape></w:pict><w:t>After</w:t></w:r></w:p>",&[("word/_rels/document.xml.rels",rels),("word/media/proof.png",b"original image bytes")]);
    let stored = RefCell::new(BTreeMap::new());
    let value = convert(&bytes, &json!({}), &|input, data| {
        let data = data.unwrap();
        assert_eq!(input["part_sha256"], crate::sha256_hex(data));
        assert_eq!(input["source_sha256"], crate::sha256_hex(&bytes));
        stored.borrow_mut().insert(
            input["part_kind"].as_str().unwrap().to_owned(),
            data.to_vec(),
        );
        Ok(if input["part_kind"] == "image" {
            json!({"text":"","blank":false,"ocr":true,"coverage":"no_recognized_text"})
        } else {
            json!({"stored":true})
        })
    })
    .unwrap();
    let text = value["text"].as_str().unwrap();
    assert!(
        text.contains("Before") && text.contains("After") && text.contains("Это не пустой рисунок")
    );
    assert_eq!(value["metadata"]["unread_visual_count"], 1);
    let stored = stored.borrow();
    assert_eq!(stored["source"], bytes);
    assert_eq!(stored["image"], b"original image bytes");
    let manifest: Value = serde_json::from_slice(&stored["manifest"]).unwrap();
    assert_eq!(manifest["source_sha256"], crate::sha256_hex(&bytes));
    let structure: Value = serde_json::from_slice(&stored["structure"]).unwrap();
    assert!(structure["relationships"].get("unused").is_none());
}
#[test]
fn choice_text_header_and_referenced_note_are_read_once() {
    let body="<w:p><w:r><mc:AlternateContent><mc:Choice Requires=\"wps\"><w:pict><v:shape><v:textbox><w:txbxContent><w:p><w:r><w:t>CHOICE</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></mc:Choice><mc:Fallback><w:r><w:t>FALLBACK MUST NOT DUPLICATE</w:t></w:r></mc:Fallback></mc:AlternateContent><w:footnoteReference w:id=\"3\"/></w:r></w:p><w:sectPr><w:headerReference r:id=\"hdr\"/></w:sectPr>";
    let bytes=archive(body,&[("word/_rels/document.xml.rels",br#"<Relationships><Relationship Id="hdr" Type="x/header" Target="header1.xml"/></Relationships>"#),("word/header1.xml",br#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER</w:t></w:r></w:p></w:hdr>"#),("word/footnotes.xml",br#"<w:footnotes xmlns:w="w"><w:footnote w:id="3"><w:p><w:r><w:footnoteRef/><w:t>NOTE</w:t></w:r></w:p></w:footnote><w:footnote w:id="4"><w:p><w:r><w:t>UNUSED</w:t></w:r></w:p></w:footnote></w:footnotes>"#)]);
    let value = convert(&bytes, &json!({}), &|_, _| Ok(json!({"stored":true}))).unwrap();
    let text = value["text"].as_str().unwrap();
    for token in ["CHOICE", "HEADER", "NOTE"] {
        assert_eq!(text.matches(token).count(), 1);
    }
    assert!(!text.contains("FALLBACK") && !text.contains("UNUSED"));
}
#[test]
fn used_external_or_traversing_image_is_never_opened() {
    for target in [
        "../../secret.png",
        "https://example.invalid/private.png",
        "media/%2e%2e/secret.png",
    ] {
        let rels=format!("<Relationships><Relationship Id=\"x\" Type=\"x/image\" Target=\"{target}\"/></Relationships>");
        let bytes = archive(
            "<w:p><w:r><w:pict><v:shape><v:imagedata r:id=\"x\"/></v:shape></w:pict></w:r></w:p>",
            &[("word/_rels/document.xml.rels", rels.as_bytes())],
        );
        assert!(convert(&bytes, &json!({}), &|input, _| {
            assert_ne!(input["part_kind"], "image");
            Ok(json!({}))
        })
        .is_err());
    }
}
