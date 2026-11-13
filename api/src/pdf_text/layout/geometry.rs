//! Conservative orthogonal grid recovery. Every glyph is emitted exactly once,
//! inside a confirmed cell or in the explicitly unassigned page text.
#[derive(Clone, Copy)]
pub(super) struct Matrix(pub [f64; 6]);
impl Matrix {
    pub(super) fn identity() -> Self {
        Self([1., 0., 0., 1., 0., 0.])
    }
    pub(super) fn point(self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }
    pub(super) fn compose(self, other: Self) -> Self {
        let [a, b, c, d, e, f] = self.0;
        let [g, h, i, j, k, l] = other.0;
        Self([
            a * g + c * h,
            b * g + d * h,
            a * i + c * j,
            b * i + d * j,
            a * k + c * l + e,
            b * k + d * l + f,
        ])
    }
}
#[derive(Clone, Copy)]
pub(super) struct Line {
    at: f64,
    start: f64,
    end: f64,
}
pub(super) struct Glyph {
    pub c: char,
    pub x: f64,
    pub y: f64,
    pub baseline: f64,
    pub left: f64,
    pub right: f64,
    pub order: i32,
    pub rotated: bool,
}
pub(super) fn line(a: (f64, f64), b: (f64, f64), h: &mut Vec<Line>, v: &mut Vec<Line>) {
    if [a.0, a.1, b.0, b.1].iter().any(|n| !n.is_finite()) {
        return;
    }
    if (a.1 - b.1).abs() < 0.5 && (a.0 - b.0).abs() > 3. {
        h.push(Line {
            at: (a.1 + b.1) / 2.,
            start: a.0.min(b.0),
            end: a.0.max(b.0),
        });
    }
    if (a.0 - b.0).abs() < 0.5 && (a.1 - b.1).abs() > 3. {
        v.push(Line {
            at: (a.0 + b.0) / 2.,
            start: a.1.min(b.1),
            end: a.1.max(b.1),
        });
    }
}
fn merge(mut lines: Vec<Line>) -> Option<Vec<Line>> {
    if lines.len() > 20_000 {
        return None;
    }
    lines.sort_by(|a, b| a.at.total_cmp(&b.at).then(a.start.total_cmp(&b.start)));
    let mut groups: Vec<(f64, Vec<Line>)> = Vec::new();
    for mut l in lines {
        if let Some(g) = groups.last_mut().filter(|g| (g.0 - l.at).abs() < 1.) {
            l.at = g.0;
            g.1.push(l);
        } else {
            groups.push((l.at, vec![l]));
        }
    }
    let mut out: Vec<Line> = Vec::new();
    for (_, mut group) in groups {
        group.sort_by(|a, b| a.start.total_cmp(&b.start));
        for l in group {
            if let Some(last) = out
                .last_mut()
                .filter(|p| p.at == l.at && l.start <= p.end + 2.)
            {
                last.end = last.end.max(l.end);
            } else {
                out.push(l);
            }
        }
    }
    Some(out)
}
fn content(mut glyphs: Vec<Glyph>) -> String {
    if glyphs.iter().any(|g| g.rotated) {
        glyphs.sort_by_key(|g| g.order);
        let mut text = String::new();
        let mut previous = None;
        for g in glyphs {
            if previous.is_some_and(|i| g.order > i + 1) && !text.ends_with(' ') {
                text.push(' ');
            }
            text.push(g.c);
            previous = Some(g.order);
        }
        return text.trim().replace('|', "\\|");
    }
    glyphs.sort_by(|a, b| {
        b.baseline
            .total_cmp(&a.baseline)
            .then(a.left.total_cmp(&b.left))
            .then(a.order.cmp(&b.order))
    });
    let mut lines: Vec<(f64, Vec<Glyph>)> = Vec::new();
    for g in glyphs {
        if let Some(row) = lines.last_mut().filter(|r| (r.0 - g.baseline).abs() < 1.) {
            row.1.push(g);
        } else {
            lines.push((g.baseline, vec![g]));
        }
    }
    let mut text = String::new();
    for (_, mut line) in lines {
        line.sort_by(|a, b| a.left.total_cmp(&b.left).then(a.order.cmp(&b.order)));
        if !text.is_empty() {
            text.push(' ');
        }
        let mut end = None;
        let mut previous_order = None;
        for g in line {
            // Ink boxes of adjacent digits may have wide gaps (e.g. "17").
            // Never manufacture a separator inside a contiguous source run.
            if previous_order.is_some_and(|i| g.order > i + 1)
                && end.is_some_and(|x| g.left - x > 1.2)
                && !text.ends_with(' ')
            {
                text.push(' ');
            }
            if !g.c.is_whitespace() || !text.ends_with(' ') {
                text.push(if g.c.is_whitespace() { ' ' } else { g.c });
            }
            end = Some(g.right);
            previous_order = Some(g.order);
        }
    }
    text.trim().replace('|', "\\|")
}
pub(super) fn reflow(glyphs: Vec<Glyph>, h: Vec<Line>, v: Vec<Line>) -> Option<String> {
    let v = merge(v)?;
    let h = merge(h)?;
    if h.len().saturating_mul(v.len()) > 2_000_000 {
        return None;
    }
    // Underlines wholly inside a cell do not create artificial row boundaries.
    let h: Vec<_> = h
        .into_iter()
        .filter(|l| {
            v.iter()
                .filter(|v| {
                    v.at >= l.start - 1.
                        && v.at <= l.end + 1.
                        && v.start <= l.at + 1.
                        && v.end >= l.at - 1.
                })
                .count()
                >= 3
        })
        .collect();
    let mut ys: Vec<_> = h.iter().map(|l| l.at).collect();
    ys.sort_by(|a, b| b.total_cmp(a));
    ys.dedup();
    if ys.len() < 3
        || ys.len() > 512
        || v.len() > 1024
        || ys.len().saturating_mul(v.len()).saturating_mul(h.len()) > 2_000_000
    {
        return None;
    }
    let mut rows: Vec<Vec<(f64, f64, Vec<Glyph>)>> = Vec::new();
    for pair in ys.windows(2) {
        let (top, bottom) = (pair[0], pair[1]);
        let mut xs: Vec<_> = v
            .iter()
            .filter(|l| l.start <= bottom + 1. && l.end >= top - 1.)
            .map(|l| l.at)
            .collect();
        xs.sort_by(f64::total_cmp);
        xs.dedup();
        let mut cells = Vec::new();
        for x in xs.windows(2) {
            // Both edges of a filled rule are not a new data column.
            if x[1] - x[0] <= 3. {
                continue;
            }
            if [top, bottom].iter().all(|y| {
                h.iter()
                    .any(|l| (l.at - y).abs() < 1. && l.start <= x[0] + 1. && l.end >= x[1] - 1.)
            }) {
                cells.push((x[0], x[1], Vec::new()));
            }
        }
        rows.push(cells);
    }
    if rows.iter().map(Vec::len).sum::<usize>() > 4096 {
        return None;
    }
    let mut free = Vec::new();
    let mut assigned = 0;
    for g in glyphs {
        let band = ys.partition_point(|y| *y > g.y);
        let cell = band
            .checked_sub(1)
            .and_then(|i| rows.get_mut(i))
            .and_then(|row| row.iter_mut().find(|(l, r, _)| g.x >= *l && g.x <= *r));
        if let Some((_, _, cell)) = cell {
            if !g.c.is_whitespace() {
                assigned += 1;
            }
            cell.push(g);
        } else {
            free.push(g);
        }
    }
    let meaningful = rows
        .iter()
        .filter(|row| {
            row.iter()
                .filter(|(_, _, g)| g.iter().any(|g| !g.c.is_whitespace()))
                .count()
                >= 2
        })
        .count();
    if meaningful < 2 || assigned == 0 {
        return None;
    }
    let mut output = String::from("[Ячейки PDF]\n");
    for row in rows {
        let values: Vec<_> = row.into_iter().map(|(_, _, g)| content(g)).collect();
        if values.iter().all(String::is_empty) {
            continue;
        }
        output.push('|');
        output.push_str(&values.join("|"));
        output.push_str("|\n");
    }
    if free.iter().any(|g| !g.c.is_whitespace()) {
        output.push_str("\n[Вне сетки: связи с ячейками неизвестны]\n");
        output.push_str(&content(free));
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_transforms_are_composed_in_document_space() {
        let outer = Matrix([0., 1., -1., 0., 10., 20.]);
        let inner = Matrix([2., 0., 0., 3., 4., 5.]);
        let p = inner.point(6., 7.);
        assert_eq!(outer.compose(inner).point(6., 7.), outer.point(p.0, p.1));
    }
}
