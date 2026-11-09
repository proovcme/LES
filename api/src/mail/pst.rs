//! Read-only Outlook archive traversal. EML snapshots are written by the mail registry.
use super::Result;
use les_core::catalog::mail;
use outlook_pst::{
    ltp::{
        prop_context::PropertyValue,
        table_context::{TableContext, TableRowData},
    },
    messaging::{
        attachment::{AnsiAttachment, Attachment, AttachmentData, UnicodeAttachment},
        message::{AnsiMessage, Message, UnicodeMessage},
        store::{AnsiStore, EntryId, Store, UnicodeStore},
    },
    ndb::{header::NdbVersion, node_id::NodeId},
    AnsiPstFile, UnicodePstFile,
};
use std::{
    collections::HashSet,
    fmt::Write as _,
    fs::File,
    io::{Read, Seek, SeekFrom},
    rc::Rc,
    time::{Duration, Instant},
};

const INVALID: &str = "MAIL_ARCHIVE_INVALID";
const LIMIT: &str = "MAIL_ARCHIVE_LIMIT";
const MESSAGE_PROPERTIES: &[u16] = &[
    0x001a, 0x0037, 0x0039, 0x0042, 0x0c1a, 0x0c1f, 0x0e02, 0x0e03, 0x0e04, 0x0e06, 0x1000, 0x1013,
    0x1035, 0x1039, 0x1042, 0x3fde, 0x3ffd, 0x5d01, 0x5d02,
];
const MAX_ATTACHMENT: usize = 10 * 1024 * 1024;
const MAX_BATCH_BYTES: usize = 256 * 1024 * 1024;

enum Archive {
    Unicode(Rc<UnicodeStore>),
    Ansi(Rc<AnsiStore>),
}

impl Archive {
    fn open(mut file: File) -> Result<Self> {
        let mut header = [0; 12];
        file.read_exact(&mut header).map_err(|_| INVALID)?;
        if &header[..4] != b"!BDN" {
            return Err(INVALID);
        }
        let version = NdbVersion::try_from(u16::from_le_bytes([header[10], header[11]]))
            .map_err(|_| "MAIL_PST_VERSION_UNSUPPORTED")?;
        file.seek(SeekFrom::Start(0)).map_err(|_| INVALID)?;
        match version {
            NdbVersion::Unicode => UnicodePstFile::read_from(Box::new(file))
                .and_then(|file| UnicodeStore::read(Rc::new(file)))
                .map(Self::Unicode),
            NdbVersion::Ansi => AnsiPstFile::read_from(Box::new(file))
                .and_then(|file| AnsiStore::read(Rc::new(file)))
                .map(Self::Ansi),
        }
        .map_err(|_| INVALID)
    }

    fn store(&self) -> &dyn Store {
        match self {
            Self::Unicode(store) => store.as_ref(),
            Self::Ansi(store) => store.as_ref(),
        }
    }

    fn message(&self, entry: &EntryId) -> Option<Snapshot> {
        match self {
            Self::Unicode(store) => {
                let message =
                    UnicodeMessage::read(store.clone(), entry, Some(MESSAGE_PROPERTIES)).ok()?;
                snapshot(message.as_ref(), |node| {
                    UnicodeAttachment::read(message.clone(), node, None)
                        .ok()
                        .map(|v| v as Rc<dyn Attachment>)
                })
            }
            Self::Ansi(store) => {
                let message =
                    AnsiMessage::read(store.clone(), entry, Some(MESSAGE_PROPERTIES)).ok()?;
                snapshot(message.as_ref(), |node| {
                    AnsiAttachment::read(message.clone(), node, None)
                        .ok()
                        .map(|v| v as Rc<dyn Attachment>)
                })
            }
        }
    }
}

fn decoded(value: Option<&PropertyValue>, page: u32, subject: bool) -> String {
    match value {
        Some(PropertyValue::String8(value)) => {
            let bytes = value.buffer();
            let bytes = if subject && bytes.first() == Some(&1) {
                bytes.get(2..).unwrap_or_default()
            } else {
                bytes
            };
            mail::decode_mapi_text(bytes, page)
        }
        Some(PropertyValue::Unicode(value)) => {
            let chars = value.buffer();
            let chars = if subject && chars.first() == Some(&1) {
                chars.get(2..).unwrap_or_default()
            } else {
                chars
            };
            String::from_utf16_lossy(chars)
                .trim_end_matches('\0')
                .to_owned()
        }
        _ => String::new(),
    }
}

fn number(value: Option<&PropertyValue>) -> Option<u32> {
    match value {
        Some(PropertyValue::Integer32(n)) => u32::try_from(*n).ok(),
        _ => None,
    }
}

fn column(table: &dyn TableContext, row: &TableRowData, id: u16) -> Option<PropertyValue> {
    let context = table.context();
    let (column, value) = context
        .columns()
        .iter()
        .zip(row.columns(context).ok()?)
        .find(|(column, _)| column.prop_id() == id)?;
    table.read_column(value.as_ref()?, column.prop_type()).ok()
}

fn header(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(220)
        .collect()
}

fn append_base64(output: &mut String, bytes: &[u8]) {
    let encoded = mail::encode_mime_base64(bytes);
    for offset in (0..encoded.len()).step_by(76) {
        output.push_str(&encoded[offset..(offset + 76).min(encoded.len())]);
        output.push_str("\r\n");
    }
}

struct Snapshot {
    eml: Vec<u8>,
    skipped_attachments: usize,
}

fn snapshot(
    message: &dyn Message,
    attachment: impl Fn(NodeId) -> Option<Rc<dyn Attachment>>,
) -> Option<Snapshot> {
    let props = message.properties();
    let page = number(props.get(0x3ffd))
        .or_else(|| number(props.get(0x3fde)))
        .filter(|v| *v != 0)
        .unwrap_or(1252);
    let value = |id| decoded(props.get(id), page, false);
    let class = value(0x001a).to_ascii_lowercase();
    if !class.is_empty() && !class.starts_with("ipm.note") && !class.starts_with("report.ipm.note")
    {
        return None;
    }
    let subject = decoded(props.get(0x0037), page, true);
    let mut body = value(0x1000);
    let mut content_type = "text/plain";
    if body.trim().is_empty() {
        body = match props.get(0x1013) {
            Some(PropertyValue::Binary(bytes)) => mail::decode_mapi_text(
                bytes.buffer(),
                number(props.get(0x3fde))
                    .filter(|v| *v != 0)
                    .unwrap_or(page),
            ),
            value => decoded(value, page, false),
        };
        if !body.is_empty() {
            content_type = "text/html";
        }
    }
    if body.len() > 10 * 1024 * 1024 {
        return None;
    }
    let mut eml = String::new();
    for (name, text) in [
        ("Subject", subject),
        ("To", value(0x0e04)),
        ("Cc", value(0x0e03)),
        ("Bcc", value(0x0e02)),
        ("Message-ID", value(0x1035)),
        ("References", value(0x1039)),
        ("In-Reply-To", value(0x1042)),
    ] {
        if !text.is_empty() {
            writeln_header(&mut eml, name, &text);
        }
    }
    let sender = [0x5d01, 0x5d02, 0x0c1f, 0x0c1a]
        .into_iter()
        .map(value)
        .find(|v| !v.is_empty());
    if let Some(sender) = sender {
        writeln_header(&mut eml, "From", &sender);
    }
    if let Some(date) = [0x0039, 0x0e06]
        .into_iter()
        .find_map(|id| match props.get(id) {
            Some(PropertyValue::Time(time)) => mail::filetime_rfc2822(*time),
            _ => None,
        })
    {
        writeln_header(&mut eml, "Date", &date);
    }
    let boundary = format!("les-pst-{}", les_core::sha256_hex(body.as_bytes()));
    let table = message.attachment_table();
    eml.push_str("MIME-Version: 1.0\r\n");
    if table.is_some() {
        write!(
            eml,
            "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\n"
        )
        .ok()?;
    }
    write!(
        eml,
        "Content-Type: {content_type}; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n"
    )
    .ok()?;
    append_base64(&mut eml, body.as_bytes());
    let mut skipped_attachments = 0;
    if let Some(table) = table {
        for (index, row) in table.rows_matrix().enumerate() {
            // The upstream reader re-enters its file lock for embedded messages.
            // Only by-value attachment rows may enter its attachment decoder.
            let method = column(table.as_ref(), row, 0x3705);
            let size = column(table.as_ref(), row, 0x0e20);
            let size = number(size.as_ref()).map(|v| v as usize);
            if index >= 100
                || number(method.as_ref()) != Some(1)
                || size.is_some_and(|n| {
                    n > MAX_ATTACHMENT || eml.len() + n.div_ceil(3) * 4 > mail::MAX_MESSAGE
                })
            {
                skipped_attachments += 1;
                continue;
            }
            let Some(file) = attachment(NodeId::from(u32::from(row.id()))) else {
                skipped_attachments += 1;
                continue;
            };
            let Some(AttachmentData::Binary(bytes)) = file.data() else {
                skipped_attachments += 1;
                continue;
            };
            let bytes = bytes.buffer();
            let encoded_size = bytes.len().div_ceil(3) * 4;
            if bytes.len() > MAX_ATTACHMENT
                || eml.len() + encoded_size + encoded_size.div_ceil(76) * 2 + 4096
                    > mail::MAX_MESSAGE
            {
                skipped_attachments += 1;
                continue;
            }
            let name = [0x3707, 0x3704]
                .into_iter()
                .map(|id| decoded(file.properties().get(id), page, false))
                .find(|v| !v.is_empty())
                .unwrap_or_else(|| format!("attachment-{}", index + 1));
            let name = header(&name);
            let filename =
                percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC);
            write!(eml,"--{boundary}\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename*=UTF-8''{filename}\r\nContent-Transfer-Encoding: base64\r\n\r\n").ok()?;
            append_base64(&mut eml, bytes);
        }
        write!(eml, "--{boundary}--\r\n").ok()?;
    }
    (eml.len() <= mail::MAX_MESSAGE).then(|| Snapshot {
        eml: eml.into_bytes(),
        skipped_attachments,
    })
}

fn writeln_header(eml: &mut String, name: &str, value: &str) {
    eml.push_str(name);
    eml.push_str(": ");
    eml.push_str(&header(value));
    eml.push_str("\r\n");
}

pub(super) fn visit(
    file: File,
    offset: usize,
    limit: usize,
    mut save: impl FnMut(&str, &[u8]) -> Result<()>,
) -> Result<(usize, usize, Option<usize>, usize)> {
    let archive = Archive::open(file)?;
    let store = archive.store();
    let root = store
        .properties()
        .ipm_sub_tree_entry_id()
        .map_err(|_| INVALID)?;
    let mut pending = vec![(root, String::new())];
    let mut folders = HashSet::new();
    let mut messages = HashSet::new();
    let (mut index, mut saved, mut skipped, mut total, mut skipped_attachments) = (0, 0, 0, 0, 0);
    let start = Instant::now();
    while let Some((entry, parent)) = pending.pop() {
        if !folders.insert(u32::from(entry.node_id())) {
            continue;
        }
        if folders.len() > 20_000 {
            return Err(LIMIT);
        }
        let folder = store.open_folder(&entry).map_err(|_| INVALID)?;
        let name = header(&folder.properties().display_name().unwrap_or_default());
        let path = if parent.is_empty() {
            name
        } else {
            format!("{parent}/{name}")
        };
        if path.len() > 3500 {
            return Err(LIMIT);
        }
        if let Some(children) = folder.hierarchy_table() {
            for row in children.rows_matrix() {
                if pending.len() >= 20_000 {
                    return Err(LIMIT);
                }
                let node = NodeId::from(u32::from(row.id()));
                pending.push((
                    store
                        .properties()
                        .make_entry_id(node)
                        .map_err(|_| INVALID)?,
                    path.clone(),
                ));
            }
        }
        let Some(contents) = folder.contents_table() else {
            continue;
        };
        for row in contents.rows_matrix() {
            let node = u32::from(row.id());
            if !messages.insert(node) {
                continue;
            }
            if index >= 1_000_000 {
                return Err(LIMIT);
            }
            if index >= offset
                && (saved == limit
                    || total >= MAX_BATCH_BYTES
                    || start.elapsed() > Duration::from_secs(100))
            {
                return Ok((saved, skipped, Some(index), skipped_attachments));
            }
            index += 1;
            if index <= offset {
                continue;
            }
            let entry = store
                .properties()
                .make_entry_id(NodeId::from(node))
                .map_err(|_| INVALID)?;
            let Some(snapshot) = archive.message(&entry) else {
                skipped += 1;
                continue;
            };
            total += snapshot.eml.len();
            skipped_attachments += snapshot.skipped_attachments;
            save(&format!("{path}/{node:08x}"), &snapshot.eml)?;
            saved += 1;
        }
    }
    Ok((saved, skipped, None, skipped_attachments))
}
