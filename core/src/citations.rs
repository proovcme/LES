//! Citation syntax only, never a factual validator or source selector.
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[(?:Источники?\s+([0-9][0-9,;\s–—\-]*)(?:,[ \t]*(?:страница|стр\.?|page)[ \t]+[0-9]+)?|([0-9][0-9,;\s–—\-]*))(?:\|([^\]\n]*))?\]").unwrap()
});
static NAMED_PARENTHETICAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\(Источники?[ \t]+([0-9][0-9,; \t–—\-]*)(?:,[ \t]*(?:страница|стр\.?|page)[ \t]+[0-9]+)?\)").unwrap()
});
static BARE_LINK_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\bссылка(?:[ \t]+на[ \t]+[^\n:*]{1,80})?[ \t]*:[ \t*]*)Источник[ \t]+([0-9]+)\b",
    )
    .unwrap()
});
static NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s*[-–—]\s*(\d+)|(\d+)").unwrap());
static CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)```.*?(?:```|$)|~~~.*?(?:~~~|$)|``.*?(?:``|$)|`[^`\n]*(?:`|$)|!?\[[^\]\n]*\]\([^\)\n]*\)").unwrap()
});
static HANDLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bQ[0-9]+\.H[0-9]+\b").unwrap());

fn numbers(value: &str) -> BTreeSet<u32> {
    let mut result = BTreeSet::new();
    for item in NUMBER.captures_iter(value) {
        if let Some(single) = item.get(3) {
            if let Ok(n) = single.as_str().parse::<u32>() {
                result.insert(n);
            }
        } else {
            let a = item[1].parse::<u32>().unwrap_or(u32::MAX);
            let b = item[2].parse::<u32>().unwrap_or(u32::MAX);
            if a <= b && b - a <= 1000 {
                result.extend(a..=b);
            } else {
                result.extend([a, b]);
            }
        }
    }
    result
}

pub fn execute(request: &Value) -> Result<Value, &'static str> {
    let answer = request["answer"].as_str().ok_or("CITATION_INPUT_INVALID")?;
    let map = request.get("sources").and_then(Value::as_array);
    let mut headers = HashMap::new();
    let mut valid = BTreeSet::new();
    let mut handles = HashMap::new();
    let mut urls = HashMap::<&str, Vec<u32>>::new();
    if let Some(sources) = map {
        for source in sources {
            if let Some(index) = source["index"].as_u64().and_then(|n| u32::try_from(n).ok()) {
                valid.insert(index);
                if let Some(url) = source["url"].as_str().filter(|s| s.starts_with("/api/")) {
                    urls.entry(url).or_default().push(index);
                }
                if let Some(handle) = source["evidence_ref"].as_str() {
                    handles.insert(handle.to_uppercase(), index);
                }
                if let Some(header) = source["header"]
                    .as_str()
                    .filter(|s| s.starts_with('[') && s.ends_with(']') && s.len() < 16384)
                {
                    headers.insert(header.to_owned(), index);
                }
            }
        }
    }
    let mut cited = BTreeSet::new();
    let mut unknown = BTreeSet::new();
    let mut repairs = 0;
    let mut transform = |part: &str| {
        let mut text = part.to_owned();
        // Repair a missing opening bracket only when the whole supplied source
        // header matches, including its document identity. Never infer a source.
        let mut ordered: Vec<_> = headers.iter().collect();
        ordered.sort_by_key(|(header, index)| (std::cmp::Reverse(header.len()), **index));
        for (header, index) in ordered {
            let token = format!("[Источник {index}]");
            for form in [header.as_str(), &header[1..]] {
                let count = text.matches(form).count();
                if count > 0 {
                    text = text.replace(form, &token);
                    repairs += count;
                }
            }
        }
        text = HANDLE
            .replace_all(&text, |cap: &regex::Captures| {
                match handles.get(&cap[0].to_uppercase()) {
                    Some(index) => format!("[Источник {index}]"),
                    None => cap[0].to_owned(),
                }
            })
            .into_owned();
        let text = NAMED_PARENTHETICAL.replace_all(&text, "[Источник $1]");
        let text = BARE_LINK_LABEL.replace_all(&text, |cap: &regex::Captures| {
            let whole = cap.get(0).unwrap();
            let follows_file_name = text[whole.end()..]
                .strip_prefix('.')
                .is_some_and(|rest| rest.starts_with(char::is_alphanumeric));
            match cap[2].parse::<u32>() {
                Ok(index) if valid.contains(&index) && !follows_file_name => {
                    format!("{}[Источник {index}]", &cap[1])
                }
                _ => whole.as_str().to_owned(),
            }
        });
        MARKER
            .replace_all(&text, |cap: &regex::Captures| {
                let mut indexes = numbers(cap.get(1).or_else(|| cap.get(2)).unwrap().as_str());
                if let Some(extra) = cap.get(3) {
                    for part in extra.as_str().split('|') {
                        if part.chars().all(|ch| {
                            ch.is_ascii_digit() || ch.is_whitespace() || ",;–—-".contains(ch)
                        }) {
                            indexes.extend(numbers(part));
                        }
                    }
                }
                let mut rendered = Vec::new();
                for n in indexes {
                    if map.is_none() || valid.contains(&n) {
                        cited.insert(n);
                        rendered.push(format!("[Источник {n}]"));
                    } else {
                        unknown.insert(n);
                        rendered.push(format!("[Источник {n}]"));
                    }
                }
                if rendered.is_empty() {
                    cap[0].to_owned()
                } else {
                    rendered.join(" ")
                }
            })
            .into_owned()
    };
    let mut output = String::new();
    let mut cursor = 0;
    let mut link_cited = BTreeSet::new();
    for code in CODE.find_iter(answer) {
        output.push_str(&transform(&answer[cursor..code.start()]));
        // Markdown destinations are protected from marker rewriting, but a
        // model occasionally prepends file:// to a supplied HTTP API path.
        // Repair only an exact source-map URL; never guess a file or page.
        let fragment = code.as_str();
        if fragment.starts_with('[') {
            if let Some((label, destination)) = fragment.rsplit_once("](") {
                if let Some(raw) = destination.strip_suffix(')') {
                    let url = raw.strip_prefix("file://").unwrap_or(raw);
                    if let Some(indexes) = urls.get(url) {
                        if raw == url {
                            output.push_str(fragment);
                        } else {
                            output.push_str(&format!("{label}]({url})"));
                        }
                        // A page/file URL can represent several visible
                        // fragments. Do not arbitrarily choose the last one.
                        link_cited.extend(indexes.iter().copied());
                        cursor = code.end();
                        continue;
                    }
                }
            }
        }
        output.push_str(fragment);
        cursor = code.end();
    }
    output.push_str(&transform(&answer[cursor..]));
    cited.extend(link_cited);
    Ok(json!({"answer":output,"indexes":cited,"unknown_indexes":unknown,"header_repairs":repairs}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_exact_bound_markdown_link_counts_without_rewriting_its_label() {
        let answer = "[План · стр. 2](/api/source?id=x#page=2) ` [код](/api/source?id=y) `";
        let result=execute(&json!({"answer":answer,"sources":[{"index":2,"url":"/api/source?id=x#page=2"},{"index":7,"url":"/api/source?id=y"}]})).unwrap();
        assert_eq!(result["answer"], answer);
        assert_eq!(result["indexes"], json!([2]));
    }
    #[test]
    fn repairs_only_exact_bound_api_destinations_outside_code() {
        let answer = "[Документ · стр. 7](file:///api/source?id=a#page=7) ` [код](file:///api/source?id=a#page=7) ` [чужая страница](file:///api/source?id=a#page=8) [внешняя](https://example.org)";
        let result = execute(
            &json!({"answer":answer,"sources":[{"index":3,"url":"/api/source?id=a#page=7"}]}),
        )
        .unwrap();
        assert_eq!(result["indexes"], json!([3]));
        assert_eq!(result["answer"], "[Документ · стр. 7](/api/source?id=a#page=7) ` [код](file:///api/source?id=a#page=7) ` [чужая страница](file:///api/source?id=a#page=8) [внешняя](https://example.org)");
    }
    #[test]
    fn headers_ranges_code_and_unknowns_remain_distinct() {
        let r=execute(&json!({"answer":"Значение *Источник 1 | файл 87.pdf | TABLE]* [Источники 1–2] `код [Источник 2]` [999]", "sources":[{"index":1,"header":"[Источник 1 | файл 87.pdf | TABLE]"},{"index":2}]})).unwrap();
        assert_eq!(r["indexes"], json!([1, 2]));
        assert_eq!(r["unknown_indexes"], json!([999]));
        assert_eq!(r["header_repairs"], 1);
        assert!(r["answer"].as_str().unwrap().contains("`код [Источник 2]`"));
        assert!(!r["answer"].as_str().unwrap().contains("файл 87"));
    }

    #[test]
    fn named_parentheses_become_links_without_treating_other_numbers_as_citations() {
        let result = execute(&json!({
            "answer": "32 (Источник 1), 7654321 (источник 2), номер (17), `(Источник 3)`",
            "sources": [{"index": 1}, {"index": 2}]
        }))
        .unwrap();
        assert_eq!(result["indexes"], json!([1, 2]));
        assert_eq!(
            result["answer"],
            "32 [Источник 1], 7654321 [Источник 2], номер (17), `(Источник 3)`"
        );
    }

    #[test]
    fn a_page_qualifier_is_not_a_second_source_index() {
        let result = execute(&json!({
            "answer": "32 (Источник 1, страница 24). 90 [Источник 2, стр. 36]. [5, страница 9]",
            "sources": [{"index": 1}, {"index": 2}, {"index": 24}, {"index": 36}]
        }))
        .unwrap();
        assert_eq!(result["indexes"], json!([1, 2]));
        assert_eq!(
            result["answer"],
            "32 [Источник 1]. 90 [Источник 2]. [5, страница 9]"
        );
    }

    #[test]
    fn bare_known_label_links_without_rewriting_document_names_or_code() {
        let result = execute(&json!({
            "answer": "**Ссылка:** Источник 4 (Таблица 1). Источник 4.docx, Источник 9, `Источник 4`",
            "sources": [{"index": 4}]
        }))
        .unwrap();
        assert_eq!(result["indexes"], json!([4]));
        assert_eq!(
            result["answer"],
            "**Ссылка:** [Источник 4] (Таблица 1). Источник 4.docx, Источник 9, `Источник 4`"
        );
    }
}
