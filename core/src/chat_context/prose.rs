//! Exact prose spans in mixed table/text evidence. Addresses use original lines.
pub(super) fn spans(text: &str, body_offset: usize) -> Vec<(u64, String)> {
    let Some(body) = text.get(body_offset..) else {
        return vec![];
    };
    if (body_offset > 0 && text.as_bytes()[body_offset - 1] != b'\n')
        || super::table_rows(body).is_empty()
    {
        return vec![];
    }
    let prefix_lines = text[..body_offset].bytes().filter(|b| *b == b'\n').count();
    let mut output = Vec::new();
    let mut start = None;
    let mut end = body_offset;
    let mut offset = body_offset;
    let flush = |start: &mut Option<(u64, usize)>, end, out: &mut Vec<_>| {
        if let Some((line, begin)) = start.take() {
            out.push((line, text[begin..end].to_owned()));
        }
    };
    for (i, raw) in body.split_inclusive('\n').enumerate() {
        let line = raw.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();
        let separator = trimmed
            .strip_prefix('|')
            .and_then(|s| s.strip_suffix('|'))
            .is_some_and(|s| {
                let fields: Vec<_> = s.split('|').collect();
                fields.len() >= 2
                    && fields.iter().all(|v| {
                        let v = v.trim().trim_matches(':');
                        v.len() >= 3 && v.bytes().all(|b| b == b'-')
                    })
            });
        if trimmed.is_empty() || separator || !super::table_rows(line).is_empty() {
            flush(&mut start, end, &mut output);
        } else {
            start.get_or_insert(((prefix_lines + i + 1) as u64, offset));
            end = offset + line.len();
        }
        offset += raw.len();
    }
    flush(&mut start, end, &mut output);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_text_keeps_exact_unicode_crlf_and_original_line_addresses() {
        let prefix = "[Source 1]\r\n{\"page\":2}\r\n";
        let body = "Пояснение 17,35 кВт.\r\nПродолжение абзаца.\r\n\r\n|Name|Qty|\r\n|---|---|\r\n|A|7|\r\n\r\nПосле таблицы: 29,5 м.";
        let text = format!("{prefix}{body}");
        assert_eq!(
            spans(&text, prefix.len()),
            vec![
                (3, "Пояснение 17,35 кВт.\r\nПродолжение абзаца.".into()),
                (10, "После таблицы: 29,5 м.".into()),
            ]
        );
        assert!(spans(&text, 1).is_empty());
        assert!(spans(&text, usize::MAX).is_empty());
        assert!(spans("Просто текст 99", 0).is_empty());
    }
}
