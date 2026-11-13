//! Typed addresses distinguish prose spans from table cells and whole excerpts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Reference {
    Source(u64),
    Row(u64, u64),
    Paragraph(u64, u64),
}
impl Reference {
    pub(super) fn parse(text: &str) -> Result<Self, &'static str> {
        let text = text.strip_prefix('S').ok_or("TABLE_FACTS_REQUIRED")?;
        let number = |s: &str| s.parse::<u64>().map_err(|_| "TABLE_FACTS_REQUIRED");
        if let Some((s, row)) = text.split_once('R') {
            let (s, row) = (number(s)?, number(row)?);
            return Ok(if row == 0 {
                Self::Source(s)
            } else {
                Self::Row(s, row)
            });
        }
        if let Some((s, line)) = text.split_once('P') {
            let (s, line) = (number(s)?, number(line)?);
            if line == 0 {
                return Err("TABLE_FACTS_REQUIRED");
            }
            return Ok(Self::Paragraph(s, line));
        }
        Ok(Self::Source(number(text)?))
    }
    pub(super) fn key(self) -> (u64, u64) {
        match self {
            Self::Source(s) => (s, 0),
            Self::Row(s, n) | Self::Paragraph(s, n) => (s, n),
        }
    }
    pub(super) fn label(self) -> String {
        match self {
            Self::Source(s) => format!("S{s}"),
            Self::Row(s, n) => format!("S{s}R{n}"),
            Self::Paragraph(s, n) => format!("S{s}P{n}"),
        }
    }
}
