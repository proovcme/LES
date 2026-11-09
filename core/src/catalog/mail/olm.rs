//! Bounded, read-only import of Outlook for Mac message XML in an OLM archive.
use super::{Result, MAX_MESSAGE};
use quick_xml::{
    escape::resolve_predefined_entity,
    events::{BytesStart, Event},
    Reader, XmlVersion,
};
use std::{
    fs::File,
    io::Read,
    time::{Duration, Instant},
};

const INVALID: &str = "MAIL_ARCHIVE_INVALID";
const MAX_ENTRY: usize = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const MAX_XML_TOTAL: usize = 256 * 1024 * 1024;

#[derive(Default)]
struct Message {
    subject: String,
    text: String,
    html: String,
    date: String,
    received: String,
    id: String,
    from: Vec<String>,
    to: Vec<String>,
    cc: Vec<String>,
}

fn header(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(900)
        .collect::<String>()
}

impl Message {
    fn address(&mut self, group: &[u8], el: &BytesStart<'_>) -> Result<()> {
        let target = match group {
            b"OPFMessageCopySenderAddress" => &mut self.from,
            b"OPFMessageCopyToAddresses" => &mut self.to,
            b"OPFMessageCopyCCAddresses" => &mut self.cc,
            _ => return Ok(()),
        };
        let mut name = String::new();
        let mut address = String::new();
        for attr in el.attributes() {
            let attr = attr.map_err(|_| INVALID)?;
            let output = match attr.key.local_name().as_ref() {
                "OPFContactEmailAddressName" => &mut name,
                "OPFContactEmailAddressAddress" => &mut address,
                _ => continue,
            };
            *output = header(
                &attr
                    .normalized_value(XmlVersion::Implicit1_0)
                    .map_err(|_| INVALID)?,
            );
        }
        if target.len() < 100 {
            match (name.trim(), address.trim()) {
                ("", "") => {}
                (name, "") => target.push(name.to_owned()),
                ("", addr) => target.push(addr.to_owned()),
                (name, addr) => target.push(format!(
                    "\"{}\" <{addr}>",
                    name.replace('\\', "\\\\").replace('"', "\\\"")
                )),
            }
        }
        Ok(())
    }

    fn add_text(&mut self, field: &[u8], text: &str) {
        match field {
            b"OPFMessageCopySubject" => self.subject.push_str(text),
            b"OPFMessageCopyBody" => self.text.push_str(text),
            b"OPFMessageCopyHTMLBody" => self.html.push_str(text),
            b"OPFMessageCopySentTime" => self.date.push_str(text),
            b"OPFMessageCopyReceivedTime" => self.received.push_str(text),
            b"OPFMessageCopyInternetMessageID" | b"OPFMessageCopyMessageID" => {
                self.id.push_str(text)
            }
            _ => {}
        }
    }

    fn into_eml(self) -> Option<Vec<u8>> {
        if self.subject.trim().is_empty()
            && self.text.trim().is_empty()
            && self.html.trim().is_empty()
            && self.from.is_empty()
        {
            return None;
        }
        let mut eml = format!("Subject: {}\r\n", header(self.subject.trim()));
        for (field, values) in [("From", self.from), ("To", self.to), ("Cc", self.cc)] {
            if !values.is_empty() {
                eml.push_str(&format!("{field}: {}\r\n", values.join(",\r\n ")));
            }
        }
        if let Some(date) = [&self.date, &self.received].into_iter().find_map(|raw| {
            chrono::DateTime::parse_from_rfc3339(raw.trim())
                .or_else(|_| chrono::DateTime::parse_from_rfc2822(raw.trim()))
                .ok()
        }) {
            eml.push_str(&format!("Date: {}\r\n", date.to_rfc2822()));
        }
        let id = header(self.id.trim().trim_matches(['<', '>']));
        if id.contains('@') && !id.chars().any(char::is_whitespace) {
            eml.push_str(&format!("Message-ID: <{id}>\r\n"));
        }
        let (mime, body) = if self.text.trim().is_empty() {
            ("html", self.html)
        } else {
            ("plain", self.text)
        };
        eml.push_str(&format!("MIME-Version: 1.0\r\nContent-Type: text/{mime}; charset=UTF-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n"));
        eml.push_str(&body);
        (eml.len() <= MAX_MESSAGE).then(|| eml.into_bytes())
    }
}

fn parse(raw: &[u8]) -> Result<Option<Vec<u8>>> {
    let converted;
    let raw = if raw.starts_with(&[0xff, 0xfe]) || raw.starts_with(&[0xfe, 0xff]) {
        let bytes = raw.get(2..).ok_or(INVALID)?;
        if bytes.len() % 2 != 0 {
            return Err(INVALID);
        }
        let little_endian = raw[0] == 0xff;
        let units: Vec<_> = bytes
            .chunks_exact(2)
            .map(|chunk| {
                let pair = [chunk[0], chunk[1]];
                if little_endian {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                }
            })
            .collect();
        converted = String::from_utf16(&units).map_err(|_| INVALID)?;
        converted.as_bytes()
    } else {
        raw
    };
    let mut reader = Reader::from_reader(raw);
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut message = Message::default();
    loop {
        match reader.read_event().map_err(|_| INVALID)? {
            Event::Start(el) => {
                if stack.len() >= 64 || el.name().as_ref().len() > 256 {
                    return Err(INVALID);
                }
                if el.local_name().as_ref() == "emailAddress" {
                    if let Some(group) = stack.last() {
                        message.address(group, &el)?;
                    }
                }
                stack.push(el.local_name().as_ref().as_bytes().to_vec());
            }
            Event::Empty(el) if el.local_name().as_ref() == "emailAddress" => {
                if let Some(group) = stack.last() {
                    message.address(group, &el)?;
                }
            }
            Event::End(_) => {
                stack.pop().ok_or(INVALID)?;
            }
            Event::Text(text) => {
                if let Some(field) = stack.last() {
                    message.add_text(field, &text.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::CData(text) => {
                if let Some(field) = stack.last() {
                    message.add_text(field, text.as_ref());
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(field) = stack.last() {
                    if let Some(value) = reference.resolve_char_ref().map_err(|_| INVALID)? {
                        message.add_text(field, value.encode_utf8(&mut [0; 4]));
                    } else {
                        message.add_text(
                            field,
                            resolve_predefined_entity(reference.as_ref()).ok_or(INVALID)?,
                        );
                    }
                }
            }
            Event::DocType(_) => return Err(INVALID),
            Event::Eof if stack.is_empty() => return Ok(message.into_eml()),
            Event::Eof => return Err(INVALID),
            _ => {}
        }
    }
}

pub fn visit_olm(
    file: File,
    offset: usize,
    limit: usize,
    mut save: impl FnMut(&str, &[u8]) -> Result<()>,
) -> Result<(usize, usize, Option<usize>)> {
    let mut archive = zip::ZipArchive::new(file).map_err(|_| INVALID)?;
    if archive.len() > MAX_ENTRIES || offset > archive.len() {
        return Err("MAIL_ARCHIVE_LIMIT");
    }
    let start = Instant::now();
    let (mut saved, mut skipped, mut total) = (0, 0, 0);
    for index in offset..archive.len() {
        if saved == limit || total >= MAX_XML_TOTAL || start.elapsed() > Duration::from_secs(100) {
            return Ok((saved, skipped, Some(index)));
        }
        let mut entry = archive.by_index(index).map_err(|_| INVALID)?;
        let name = entry.name().map_err(|_| INVALID)?.into_owned();
        let lower = name.to_ascii_lowercase();
        if !entry.is_file() || !lower.ends_with(".xml") || !lower.contains("message") {
            continue;
        }
        if entry.size() > MAX_ENTRY as u64 || name.len() > 4096 {
            skipped += 1;
            continue;
        }
        let mut raw = Vec::new();
        (&mut entry)
            .take(MAX_ENTRY as u64 + 1)
            .read_to_end(&mut raw)
            .map_err(|_| INVALID)?;
        total += raw.len();
        if raw.len() > MAX_ENTRY {
            skipped += 1;
            continue;
        }
        match parse(&raw) {
            Ok(Some(eml)) => {
                save(&name, &eml)?;
                saved += 1;
            }
            Ok(None) | Err(_) => skipped += 1,
        }
    }
    Ok((saved, skipped, None))
}
