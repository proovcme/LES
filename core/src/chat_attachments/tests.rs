use super::*;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("les-attachment-{}", new_id().unwrap()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn json_original_does_not_overwrite_its_card_and_detects_changed_bytes() {
    let scratch = Scratch::new();
    let input = scratch.0.join("源.json");
    fs::write(&input, b"{\"value\":7}").unwrap();
    let base = scratch.0.join("store");
    let ident = "read_123456abcdef";
    let card = preserve(&base, &input, ident, "Данные 日本語.json").unwrap();
    assert_eq!(card["stored_name"], "read_123456abcdef.source.json");
    let asset = resolve(&base, ident, MAX_AGE).unwrap();
    assert_eq!(asset.metadata, card);
    assert_eq!(fs::read(asset.path).unwrap(), fs::read(input).unwrap());
    fs::write(
        base.join(card["stored_name"].as_str().unwrap()),
        b"{\"value\":8}",
    )
    .unwrap();
    assert!(matches!(
        resolve(&base, ident, MAX_AGE),
        Err("ATTACHMENT_HASH_CHANGED")
    ));
}
#[test]
fn legacy_name_and_size_are_verified_and_unsafe_card_cannot_delete_target() {
    let scratch = Scratch::new();
    let input = scratch.0.join("original.txt");
    fs::write(&input, b"unchanged").unwrap();
    let base = scratch.0.join("store");
    let ident = "read_aabbccddeeff";
    let mut card = preserve(&base, &input, ident, "one.txt").unwrap();
    let previous = base.join(card["stored_name"].as_str().unwrap());
    let legacy = base.join(format!("{ident}.txt"));
    fs::rename(previous, &legacy).unwrap();
    card["stored_name"] = json!(format!("{ident}.txt"));
    let cp = base.join(format!("{ident}.json"));
    fs::write(&cp, card.to_string()).unwrap();
    assert!(resolve(&base, ident, MAX_AGE).is_ok());
    card["stored_name"] = json!("../original.txt");
    fs::write(&cp, card.to_string()).unwrap();
    assert!(matches!(
        resolve(&base, ident, MAX_AGE),
        Err("ATTACHMENT_PATH_UNSAFE")
    ));
    consume(&base, ident).unwrap();
    assert_eq!(fs::read(input).unwrap(), b"unchanged");
}
#[test]
fn expiry_preserves_other_attachments_and_original_sources() {
    let scratch = Scratch::new();
    let input = scratch.0.join("source.txt");
    fs::write(&input, b"source").unwrap();
    let base = scratch.0.join("store");
    let first = "read_123456789abc";
    let second = "read_123456789abd";
    let mut card = preserve(&base, &input, first, "one.txt").unwrap();
    preserve(&base, &input, second, "two.txt").unwrap();
    card["created_at_epoch"] = json!(1);
    fs::write(base.join(format!("{first}.json")), card.to_string()).unwrap();
    assert_eq!(cleanup(&base, MAX_AGE).unwrap(), 1);
    assert!(resolve(&base, second, MAX_AGE).is_ok());
    assert_eq!(fs::read(input).unwrap(), b"source");
}
#[test]
fn csv_keeps_physical_rows_and_literal_empty_cells() {
    let scratch = Scratch::new();
    let input = scratch.0.join("rows.csv");
    fs::write(&input, "A;B\r\n\r\n0;;42\r\n\"long\nvalue\";\";\"\r\n").unwrap();
    let result = crate::document::conversion::csv_snapshot(&input).unwrap();
    assert_eq!(result["rows"][1]["locator"], "CSV!R3");
    assert_eq!(result["rows"][1]["cells"][1]["coordinate"], "C3");
    assert_eq!(result["rows"][1]["cells"][0]["value"], "0");
}
#[test]
fn malformed_image_is_not_admitted_by_header_alone() {
    assert_eq!(
        picture::bytes(b"\x89PNG\r\n\x1a\n").unwrap_err(),
        "ATTACHMENT_IMAGE_INVALID"
    );
}
#[test]
fn leftover_locks_do_not_exhaust_the_expired_card_scan_budget() {
    let scratch = Scratch::new();
    let input = scratch.0.join("input.txt");
    fs::write(&input, b"source").unwrap();
    let base = scratch.0.join("store");
    let ident = "read_ffffffffffff";
    let mut card = preserve(&base, &input, ident, "file.txt").unwrap();
    card["created_at_epoch"] = json!(1);
    fs::write(base.join(format!("{ident}.json")), card.to_string()).unwrap();
    for n in 0..10 {
        fs::write(base.join(format!("read_{n:012x}.lock")), b"").unwrap();
    }
    assert_eq!(cleanup_cards(&base, MAX_AGE, 1).unwrap(), 1);
    assert!(!base.join(card["stored_name"].as_str().unwrap()).exists());
    assert_eq!(fs::read(input).unwrap(), b"source");
}
