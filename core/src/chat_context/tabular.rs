//! Column and row addresses are a model projection; stored quotes stay intact.
fn body(line: &str) -> Option<&str> {
    line.trim().strip_prefix('|')?.strip_suffix('|')
}
fn cells(line: &str) -> Option<Vec<String>> {
    let row = body(line)?;
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut escaped = false;
    for ch in row.chars() {
        if escaped {
            if ch != '|' {
                cell.push('\\');
            }
            cell.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '|' {
            cells.push(cell.trim().to_owned());
            cell.clear();
        } else {
            cell.push(ch);
        }
    }
    if escaped {
        cell.push('\\');
    }
    cells.push(cell.trim().to_owned());
    if cells.len() < 2
        || cells.iter().all(|s| s.is_empty())
        || cells.iter().all(|s| {
            let s = s.trim_matches(':');
            s.len() >= 3 && s.chars().all(|c| c == '-')
        })
    {
        return None;
    }
    Some(cells)
}
pub(super) fn rows(text: &str) -> Vec<(u64, Vec<String>)> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| cells(line).map(|v| ((i + 1) as u64, v)))
        .collect()
}
pub(super) fn headers(text: &str) -> Vec<(u64, Vec<String>)> {
    let lines = text.lines().collect::<Vec<_>>();
    lines
        .windows(2)
        .enumerate()
        .filter_map(|(i, pair)| {
            let labels = cells(pair[0])?;
            let rule = body(pair[1])?;
            let fields = rule.split('|').map(str::trim).collect::<Vec<_>>();
            (fields.len() == labels.len()
                && fields.iter().all(|s| {
                    let s = s.trim_matches(':');
                    s.len() >= 3 && s.bytes().all(|c| c == b'-')
                }))
            .then_some(((i + 1) as u64, labels))
        })
        .collect()
}
#[cfg(test)]
pub(super) fn render(text: &str, source: u64) -> String {
    render_with_prose(text, source, &[])
}
pub(super) fn render_with_prose(text: &str, source: u64, spans: &[(u64, String)]) -> String {
    text.lines()
        .enumerate()
        .map(|(row, line)| {
            let Some(cells) = cells(line) else {
                if spans.iter().any(|(start, _)| *start == (row + 1) as u64) {
                    return format!("S{source}P{} {line}", row + 1);
                }
                return line.to_owned();
            };
            let mut values = Vec::new();
            let mut i = 0;
            while i < cells.len() {
                let start = i;
                while i + 1 < cells.len() && cells[i + 1] == cells[start] {
                    i += 1;
                }
                if !cells[start].is_empty() {
                    values.push(format!(
                        "C{}{}: {}",
                        start + 1,
                        if i == start {
                            String::new()
                        } else {
                            format!("–C{}", i + 1)
                        },
                        cells[start]
                    ));
                }
                i += 1;
            }
            format!("S{source}R{} {}", row + 1, values.join("; "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_pipe_lines_and_every_unicode_prefix_are_safe_context() {
        for rule in ["", "|", "||", "|\t|", "| - |"] {
            assert!(headers(&format!("| Header Ω | Value |\n{rule}")).is_empty());
        }
        let text = "| Header Ω | Value |\n| --- | --- |\n| Item 日本語 | 17.25 |";
        for end in text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
        {
            let prefix = &text[..end];
            let _ = (rows(prefix), headers(prefix), render(prefix, 1));
        }
        assert_eq!(
            headers(text),
            vec![(1, vec!["Header Ω".into(), "Value".into()])]
        );
        assert!(render(text, 1).contains("C2: 17.25"));
    }
    #[test]
    fn addresses_keep_blank_columns_and_do_not_turn_them_into_zero() {
        let text="| Change | Sheet | Description | Code |\n| --- | --- | --- | --- |\n| 1 | 8 | Move unit | 1 |\n| 1 | | Empty sheet | 0 |";
        let out = render(text, 1);
        assert!(out.contains("C1: 1; C2: 8; C3: Move unit; C4: 1"));
        assert!(out.contains("C1: 1; C3: Empty sheet; C4: 0"));
        assert!(!out.contains("C2: 0"));
    }
    #[test]
    fn duplicated_adjacent_labels_and_escaped_cell_pipes_are_preserved() {
        assert_eq!(
            render("| Title | Title | A\\|B | -17.25 |", 1),
            "S1R1 C1–C2: Title; C3: A|B; C4: -17.25"
        );
        assert_eq!(render("Ordinary prose 17.25", 1), "Ordinary prose 17.25");
    }
    #[test]
    fn prose_and_cells_share_original_addresses_without_making_prose_an_operand() {
        let text = "[Source 1]\n{}\nBefore 17.35.\n\n|Name|Qty|\n|---|---|\n|A|7|\n\nAfter 29.5.";
        let offset = text.find("Before").unwrap();
        let spans = super::super::prose_spans(text, offset);
        let out = render_with_prose(text, 1, &spans);
        assert!(out.contains("S1P3 Before 17.35.") && out.contains("S1P9 After 29.5."));
        assert!(out.contains("S1R7 C1: A; C2: 7"));
        assert!(!out.contains("P6") && !out.contains("P1"));
    }
}
