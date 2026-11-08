use crate::workspace::{Failure, Result};
#[derive(Clone)]
pub(super) struct Region {
    pub first_row: u32,
    pub last_row: u32,
    pub first_col: u32,
    pub last_col: u32,
}
pub(super) fn invalid() -> Failure {
    Failure::new(
        "WORKBOOK_RANGE_INVALID",
        "Укажите точное имя листа и адрес A1 или A1:B10",
    )
}
pub(super) fn column(mut value: u32) -> String {
    let mut text = Vec::new();
    while value > 0 {
        value -= 1;
        text.push(char::from(b'A' + (value % 26) as u8));
        value /= 26;
    }
    text.iter().rev().collect()
}
pub(super) fn coordinate(value: &str) -> Result<(u32, u32)> {
    let value = value.strip_prefix('$').unwrap_or(value);
    let mut letters = 0;
    let mut col = 0_u32;
    for byte in value.bytes().take_while(u8::is_ascii_alphabetic) {
        letters += 1;
        col = col * 26 + u32::from(byte.to_ascii_uppercase() - b'A' + 1);
        if letters > 3 {
            return Err(invalid());
        }
    }
    let row = value[letters..]
        .strip_prefix('$')
        .unwrap_or(&value[letters..]);
    if letters == 0
        || row.is_empty()
        || row.len() > 5
        || row.starts_with('0')
        || !row.bytes().all(|v| v.is_ascii_digit())
    {
        return Err(invalid());
    }
    Ok((row.parse().map_err(|_| invalid())?, col))
}
impl Region {
    pub(super) fn parse(value: &str) -> Result<Self> {
        let mut parts = value.split(':');
        let (first_row, first_col) = coordinate(parts.next().unwrap_or(""))?;
        let (last_row, last_col) = parts
            .next()
            .map(coordinate)
            .transpose()?
            .unwrap_or((first_row, first_col));
        if parts.next().is_some() || last_row < first_row || last_col < first_col {
            return Err(invalid());
        }
        if last_row > 20_000 || last_col > 512 {
            return Err(Failure::new(
                "WORKBOOK_RANGE_LIMIT",
                "Диапазон превышает предел чтения таблицы",
            ));
        }
        Ok(Self {
            first_row,
            last_row,
            first_col,
            last_col,
        })
    }
    pub(super) fn contains(&self, row: u32, col: u32) -> bool {
        (self.first_row..=self.last_row).contains(&row)
            && (self.first_col..=self.last_col).contains(&col)
    }
    pub(super) fn intersects(&self, other: &Self) -> bool {
        self.first_row <= other.last_row
            && other.first_row <= self.last_row
            && self.first_col <= other.last_col
            && other.first_col <= self.last_col
    }
    pub(super) fn size(&self) -> u64 {
        u64::from(self.last_row - self.first_row + 1)
            * u64::from(self.last_col - self.first_col + 1)
    }
    pub(super) fn display(&self) -> String {
        let first = format!("{}{}", column(self.first_col), self.first_row);
        if self.first_row == self.last_row && self.first_col == self.last_col {
            first
        } else {
            format!("{first}:{}{}", column(self.last_col), self.last_row)
        }
    }
}
