//! Expand explicit OOXML cell spans while keeping shared row/column labels.
#[derive(Default)]
pub(super) struct Table {
    rows: Vec<Vec<String>>,
    row: Option<Vec<String>>,
    pub cell: Option<Vec<String>>,
    span: usize,
    merge: Option<bool>,
    previous_merges: Vec<Option<String>>,
    current_merges: Vec<Option<String>>,
    grid_width: usize,
    row_suffix: Option<usize>,
    text_bytes: usize,
}

impl Table {
    pub fn continuation(&self) -> bool {
        self.merge == Some(true)
    }
    pub fn start_row(&mut self) -> Option<()> {
        if self.row.is_some() {
            return None;
        }
        self.row = Some(Vec::new());
        self.current_merges.clear();
        self.row_suffix = None;
        Some(())
    }

    pub fn column(&mut self) -> Option<()> {
        if self.row.is_some() || !self.rows.is_empty() || self.grid_width >= 512 {
            return None;
        }
        self.grid_width += 1;
        Some(())
    }

    pub fn omitted(&mut self, count: usize, before: bool) -> Option<()> {
        if count > 512 || self.cell.is_some() {
            return None;
        }
        let row = self.row.as_mut()?;
        if before {
            if !row.is_empty() {
                return None;
            }
            row.resize(count, String::new());
            self.current_merges.resize(count, None);
        } else if self.row_suffix.replace(count).is_some() {
            return None;
        }
        Some(())
    }

    pub fn start_cell(&mut self) -> Option<()> {
        if self.row.is_none() || self.cell.is_some() {
            return None;
        }
        self.cell = Some(Vec::new());
        self.span = 1;
        self.merge = None;
        Some(())
    }

    pub fn span(&mut self, width: usize) -> Option<()> {
        if self.cell.is_none() || !(1..=512).contains(&width) {
            return None;
        }
        self.span = width;
        Some(())
    }

    pub fn merge(&mut self, value: Option<&str>) -> Option<()> {
        if self.cell.is_none() {
            return None;
        }
        self.merge = Some(match value {
            Some("restart") => false,
            None | Some("continue") => true,
            _ => return None,
        });
        Some(())
    }

    pub fn finish_cell(&mut self) -> Option<()> {
        let mut text = self.cell.take()?.join("\n");
        let row = self.row.as_mut()?;
        if row.len() + self.span > 512 {
            return None;
        }
        if self.merge == Some(true) {
            if !text.is_empty() {
                return None;
            }
            let source = self.previous_merges.get(row.len())?.as_ref()?;
            if (row.len()..row.len() + self.span).any(|column| {
                self.previous_merges.get(column).and_then(Option::as_ref) != Some(source)
            }) {
                return None;
            }
            text = source.clone();
        }
        self.text_bytes = self
            .text_bytes
            .checked_add(text.len().checked_mul(self.span)?)?;
        if self.text_bytes > super::MAX_TEXT {
            return None;
        }
        for _ in 0..self.span {
            row.push(text.clone());
            self.current_merges.push(self.merge.map(|_| text.clone()));
        }
        Some(())
    }

    pub fn finish_row(&mut self) -> Option<()> {
        if self.cell.is_some() {
            return None;
        }
        let mut row = self.row.take()?;
        let width = row.len().checked_add(self.row_suffix.unwrap_or(0))?;
        if width > 512 {
            return None;
        }
        row.resize(width, String::new());
        self.current_merges.resize(width, None);
        if row.is_empty() || (self.rows.len() + 1).saturating_mul(row.len()) > 100_000 {
            return None;
        }
        self.rows.push(row);
        self.previous_merges = std::mem::take(&mut self.current_merges);
        Some(())
    }

    pub fn markdown(self) -> Option<String> {
        let width = if self.grid_width > 0 {
            self.grid_width
        } else {
            self.rows.first()?.len()
        };
        if width == 0 || self.rows.iter().any(|row| row.len() != width) {
            eprintln!(
                "{}",
                serde_json::json!({"event":"docx_table_grid_mismatch","width":width,"mismatches":self.rows.iter().enumerate().filter(|(_,r)|r.len()!=width).take(8).map(|(i,r)|serde_json::json!([i,r.len()])).collect::<Vec<_>>()})
            );
            return None;
        }
        let mut out = format!("{}|\n{}|", "|  ".repeat(width), "| --- ".repeat(width));
        for row in self.rows {
            out.push_str("\n|");
            for cell in row {
                out.push(' ');
                out.push_str(&cell.replace('|', "\\|").replace('\n', "<br>"));
                out.push_str(" |");
            }
        }
        Some(out)
    }
}
