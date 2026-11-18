use regex::Regex;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

const MAX_ROWS: usize = 32;
const LAST_EXCEL_ROW: u64 = 1_048_576;

/// Explicit physical rows on a named sheet. Position numbers and ordinary
/// numbers in the question are left to search.
pub fn sheet_rows(question: &str) -> BTreeMap<&str, BTreeSet<u64>> {
    static SHEETS: OnceLock<Regex> = OnceLock::new();
    static ROWS: OnceLock<Regex> = OnceLock::new();
    static RANGES: OnceLock<Regex> = OnceLock::new();
    let sheets = SHEETS.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(?:лист(?:е|а|у|ом)?|(?:work)?sheet)\s+(?:"([^"\r\n]{1,31})"|'([^'\r\n]{1,31})'|«([^»\r\n]{1,31})»|([\pL\pN_-]{1,31})\b)"#,
        )
        .unwrap()
    });
    let rows = ROWS.get_or_init(|| {
        Regex::new(r"(?i)\b(?:строк(?:а|и|у|е|ой|ах|ам|ами)?|rows?)\s*(?:№\s*)?([0-9]+(?:\s*[-–—:]\s*[0-9]+)?(?:\s*(?:,|\bи\b|\band\b)\s*[0-9]+(?:\s*[-–—:]\s*[0-9]+)?)*)").unwrap()
    });
    let ranges = RANGES.get_or_init(|| Regex::new(r"([0-9]+)(?:\s*[-–—:]\s*([0-9]+))?").unwrap());
    let mut result: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
    let mut matches = sheets.captures_iter(question).peekable();
    while let Some(sheet) = matches.next() {
        let name = sheet.iter().skip(1).flatten().next().unwrap().as_str();
        let start = sheet.get(0).unwrap().end();
        let end = matches
            .peek()
            .map_or(question.len(), |next| next.get(0).unwrap().start());
        let clause = &question[start..end];
        for address in rows.captures_iter(clause) {
            let full = address.get(0).unwrap();
            let suffix = &clause[full.end()..];
            if clause[..full.start()].contains(['.', '?', '!', ';'])
                || suffix
                    .strip_prefix('.')
                    .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_digit()))
            {
                continue;
            }
            let coordinates = result.entry(name).or_default();
            for range in ranges.captures_iter(&address[1]) {
                let first = range[1].parse::<u64>();
                let last = range.get(2).map_or(first.clone(), |m| m.as_str().parse());
                let (Ok(first), Ok(last)) = (first, last) else {
                    return BTreeMap::new();
                };
                if !(1..=LAST_EXCEL_ROW).contains(&first)
                    || last < first
                    || last > LAST_EXCEL_ROW
                    || last - first >= MAX_ROWS as u64
                {
                    return BTreeMap::new();
                }
                coordinates.extend(first..=last);
            }
        }
    }
    if result.values().map(BTreeSet::len).sum::<usize>() > MAX_ROWS {
        return BTreeMap::new();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_physical_addresses_separate_from_positions_and_other_sheets() {
        let addresses = sheet_rows(
            "На листе «Монтаж 01» приведи строки 4–6 и 12. На листе 2 позиции 1.2, 1.4 и 1.5. On sheet Log show rows 8, 15 and 18–19.",
        );
        assert_eq!(
            addresses,
            BTreeMap::from([
                ("Log", BTreeSet::from([8, 15, 18, 19])),
                ("Монтаж 01", BTreeSet::from([4, 5, 6, 12])),
            ])
        );
        assert!(sheet_rows("На листе 2 строки 1.2").is_empty());
        assert!(sheet_rows("На листе Log строки 4–900").is_empty());
        assert!(sheet_rows("Сравни позицию 4 и сумму 6 на листе Тест").is_empty());
    }
}
