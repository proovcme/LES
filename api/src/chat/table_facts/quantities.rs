//! Detect a changed literal quantity in citations to PDF text. A matching
//! literal does not establish relevance, so it grants no validation receipt.
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

static QUANTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        (?: ^ | [^\pL\pN_.,+−-] )
        ( [ +−- ]? [0-9]+ (?: [\x20\x{00a0}\x{202f}] [0-9]{3} )* (?: [.,] [0-9]+ )? )
        [\x20\t\x{00a0}\x{202f}]*
        ( [°%‰\pL] [\pL\pN°%‰²³/·]* )
    ",
    )
    .expect("literal quantity pattern")
});

fn values(text: &str) -> HashMap<String, HashSet<i128>> {
    let mut units = HashMap::<String, HashSet<i128>>::new();
    let plain = text.replace("**", "").replace("__", "");
    for found in QUANTITY.captures_iter(&plain) {
        if let Ok(value) = super::decimal(&found[1].replace('−', "-")) {
            units.entry(found[2].to_owned()).or_default().insert(value);
        }
    }
    units
}

pub(super) fn contradicts(answer: &str, excerpts: &[&str]) -> bool {
    let reported = values(answer);
    if reported.is_empty() {
        return false;
    }
    let mut observed = HashMap::<String, HashSet<i128>>::new();
    for excerpt in excerpts {
        for (unit, numbers) in values(excerpt) {
            observed.entry(unit).or_default().extend(numbers);
        }
    }
    reported.iter().any(|(unit, numbers)| {
        observed
            .get(unit)
            .is_some_and(|original| !numbers.is_subset(original))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checks_the_reported_number_with_its_own_unit() {
        let proof = ["Длина 17,25 м. В журнале 999 шт. Температура −5 °C."];
        assert!(contradicts("Длина **999 м** [Источник 1]", &proof));
        assert!(!contradicts("17.250 м и -5 °C [Источник 1]", &proof));
        assert!(!contradicts("См. [Источник 17]", &proof));
        assert!(!contradicts("Поз. А12м", &proof));
        assert!(!contradicts("Обе длины 17 м и 19 м.", &["17 м", "19 м"]));
    }
}
