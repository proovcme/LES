//! Project saved cells and conservative paper/model-space grids as searchable rows.
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const MAX_LINES: usize = 6_000;
const MAX_COLUMNS: usize = 32;
const MAX_ROWS: usize = 512;

fn string(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.trim().is_empty())
}

pub(super) fn xy(value: &Value) -> Option<(f64, f64)> {
    Some((value["x"].as_f64()?, value["y"].as_f64()?))
}

pub(super) fn text_position(entry: &Value) -> Option<&Value> {
    [
        &entry["insertionPoint"],
        &entry["startPoint"],
        &entry["text"]["startPoint"],
    ]
    .into_iter()
    .find(|value| xy(value).is_some())
}

// Strip common AutoCAD display controls from search text. The saved cell remains available.
fn plain(raw: &str) -> String {
    let mut rendered = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '{' | '}' => {}
            '\\' => match chars.next() {
                Some('P' | '~') => rendered.push(' '),
                Some(ch @ ('{' | '}' | '\\')) => rendered.push(ch),
                Some('L' | 'l' | 'O' | 'o' | 'K' | 'k') => {}
                Some(
                    'F' | 'f' | 'C' | 'c' | 'H' | 'h' | 'W' | 'w' | 'T' | 't' | 'Q' | 'q' | 'A'
                    | 'a',
                ) if chars.clone().any(|c| c == ';') => {
                    let _ = chars.by_ref().find(|&c| c == ';');
                }
                Some(ch) => {
                    rendered.push('\\');
                    rendered.push(ch);
                }
                None => rendered.push('\\'),
            },
            _ => rendered.push(ch),
        }
    }
    rendered.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone)]
struct Cell<'a> {
    raw: &'a str,
    handle: &'a str,
    layer: Option<&'a str>,
}

type Row<'a> = Vec<Vec<Cell<'a>>>;

fn row_values(row: &Row<'_>) -> Vec<String> {
    row.iter()
        .map(|cell| {
            cell.iter()
                .map(|v| plain(v.raw))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn numeric(s: &str) -> bool {
    s.replace(',', ".").parse::<f64>().is_ok_and(f64::is_finite)
}

fn header_row(rows: &[Row<'_>]) -> Option<Vec<String>> {
    let (first, rest) = rows.split_first()?;
    let first = row_values(first);
    let labels = first
        .iter()
        .filter(|s| s.chars().any(char::is_alphabetic))
        .count();
    let has_numbers_below = rest.iter().map(row_values).any(|row| {
        row.iter()
            .zip(&first)
            .any(|(value, label)| numeric(value) && label.chars().any(char::is_alphabetic))
    });
    (labels >= 2 && first.iter().all(|s| !numeric(s)) && has_numbers_below).then_some(first)
}

struct Table<'a> {
    handle: &'a str,
    title: Option<String>,
    owner: &'a str,
    method: &'static str,
    rows: Vec<Row<'a>>,
    headers: Option<Vec<String>>,
    row_base: usize,
    grid_layers: BTreeSet<&'a str>,
    origin: Option<(f64, f64)>,
    table_layer: Option<&'a str>,
}

fn into_objects(
    table: Table<'_>,
    sheets: &BTreeMap<&str, Value>,
    layer_handles: &BTreeMap<&str, &str>,
    order: usize,
) -> Vec<Value> {
    let headers = table.headers.as_ref();
    let skip_header = usize::from(headers.is_some());
    table
        .rows
        .iter()
        .enumerate()
        .skip(skip_header)
        .filter_map(|(index, row)| {
            let cells = row_values(row);
            let filled = row.iter().filter(|col| !col.is_empty()).count();
            let anchor = row.iter().flatten().next()?;
            let row_number = index + 1 + table.row_base;
            let display_title = table.title.as_deref().unwrap_or("Таблица DWG");
            let mut properties = Map::new();
            let mut entries = Vec::new();
            for (index, (value, cell_sources)) in cells.iter().zip(row).enumerate() {
                let column = index + 1;
                let heading = headers.and_then(|h| h.get(index)).filter(|s| !s.is_empty());
                if !value.is_empty() {
                    let key = heading
                        .map_or_else(|| format!("Колонка {column}"), |h| format!("{column}. {h}"));
                    properties.insert(key, json!(value));
                }
                let sources = cell_sources
                    .iter()
                    .map(|cell| {
                        json!({"handle":cell.handle,"layer":cell.layer,
                    "layer_handle":cell.layer.and_then(|layer| layer_handles.get(layer)),
                    "saved_text":cell.raw})
                    })
                    .collect::<Vec<_>>();
                entries.push(
                    json!({"column":column,"heading":heading,"text":value,"sources":sources}),
                );
            }
            let layers = row
                .iter()
                .flatten()
                .filter_map(|c| c.layer)
                .chain(table.table_layer)
                .collect::<BTreeSet<_>>();
            let source_handle = if table.method == "table_block" {
                table.handle
            } else {
                anchor.handle
            };
            properties.insert("Слои надписей".into(), json!(layers));
            let table_row = json!({"table_handle":table.handle,"title":table.title,"row":row_number,
            "table_order":order,
            "method":table.method,"kind":if filled > 1 {"data"} else {"unclassified"},
            "cells":entries,"layers":layers,"grid_layers":table.grid_layers});
            Some(
                json!({"object_id":format!("dwg_table_rows:{}:{row_number}", table.handle),
                "native_id":source_handle,"section":"dwg_table_rows","name":display_title,
                "category":"DWG_TABLE_ROW","identifiers":{"handle":source_handle},
            "drawing_sheet":sheets.get(table.owner),"properties":properties,
            "dwg_table_row":table_row,"value_semantics":"saved_dwg"}),
            )
        })
        .collect()
}

fn native<'a>(entry: &'a Value, owner: &'a str) -> Option<Table<'a>> {
    let handle = string(&entry["handle"])?;
    let rows = usize::try_from(entry["rowCount"].as_u64()?).ok()?;
    let cols = usize::try_from(entry["columnCount"].as_u64()?).ok()?;
    let cells = entry["cells"].as_array()?;
    if !(1..=MAX_ROWS).contains(&rows)
        || !(1..=MAX_COLUMNS).contains(&cols)
        || rows * cols != cells.len()
    {
        return None;
    }
    let layer = string(&entry["layer"]);
    let mut rows = cells
        .chunks_exact(cols)
        .map(|row| {
            row.iter()
                .map(|cell| {
                    string(&cell["text"])
                        .into_iter()
                        .map(|raw| Cell { raw, handle, layer })
                        .collect()
                })
                .collect::<Row<'_>>()
        })
        .collect::<Vec<_>>();
    if !rows.iter().flatten().any(|column| !column.is_empty()) {
        return None;
    }
    let mut title = string(&entry["name"]).map(plain);
    let mut row_base = 0;
    if rows.len() >= 3
        && rows[0].iter().filter(|cell| !cell.is_empty()).count() == 1
        && cells[..cols]
            .iter()
            .any(|cell| cell["mergedValue"] == true || cell["mergedValue"] == 1)
    {
        title = rows
            .remove(0)
            .into_iter()
            .flatten()
            .next()
            .map(|cell| plain(cell.raw));
        row_base = 1;
    }
    let headers = header_row(&rows);
    Some(Table {
        handle,
        title,
        owner,
        method: "native",
        rows,
        headers,
        row_base,
        grid_layers: BTreeSet::new(),
        origin: xy(&entry["startPoint"]),
        table_layer: layer,
    })
}

#[derive(Clone)]
struct Span<'a> {
    coord: f64,
    start: f64,
    end: f64,
    handle: &'a str,
    layers: BTreeSet<&'a str>,
}

fn close(left: f64, right: f64, epsilon: f64) -> bool {
    (left - right).abs() <= epsilon
}

fn spans<'a>(lines: &[&'a Value], horizontal: bool, epsilon: f64) -> Vec<Span<'a>> {
    let mut spans = lines
        .iter()
        .filter_map(|e| {
            let (x1, y1) = xy(&e["startPoint"])?;
            let (x2, y2) = xy(&e["endPoint"])?;
            let (a, b, start, end) = if horizontal {
                (y1, y2, x1, x2)
            } else {
                (x1, x2, y1, y2)
            };
            if !close(a, b, epsilon) || close(start, end, epsilon) {
                return None;
            }
            Some(Span {
                coord: (a + b) / 2.0,
                start: start.min(end),
                end: start.max(end),
                handle: string(&e["handle"])?,
                layers: string(&e["layer"]).into_iter().collect(),
            })
        })
        .collect::<Vec<_>>();
    spans.sort_by(|a, b| a.coord.total_cmp(&b.coord));
    let mut merged = Vec::<Span<'a>>::new();
    let mut rest = spans.as_mut_slice();
    while let Some(first) = rest.first() {
        let coordinate = first.coord;
        let count = rest.partition_point(|s| close(s.coord, coordinate, epsilon));
        let (nearby, more) = rest.split_at_mut(count);
        nearby.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut spans = Vec::<Span<'a>>::new();
        for span in nearby {
            if let Some(last) = spans
                .last_mut()
                .filter(|last| span.start <= last.end + epsilon)
            {
                last.end = last.end.max(span.end);
                last.layers.append(&mut span.layers);
            } else {
                span.coord = coordinate;
                spans.push(span.clone());
            }
        }
        merged.extend(spans);
        rest = more;
    }
    merged
}

struct Label<'a> {
    x: f64,
    y: f64,
    height: f64,
    cell: Cell<'a>,
}

fn labels<'a>(entries: &[&'a Value]) -> Vec<Label<'a>> {
    entries
        .iter()
        .filter_map(|entry| {
            let rotation = entry["rotation"]
                .as_f64()
                .or_else(|| entry["text"]["rotation"].as_f64())
                .unwrap_or(0.0);
            if !matches!(entry["type"].as_str(), Some("TEXT" | "MTEXT"))
                || entry["isVisible"] == false
                || rotation.sin().abs() > 1e-6
                || rotation.cos() < 0.0
            {
                return None;
            }
            let raw = super::projection::dwg_text(entry)?;
            if raw.chars().take(2_049).count() > 2_048 {
                return None;
            }
            let (x, y) = xy(text_position(entry)?)?;
            Some(Label {
                x,
                y,
                height: entry["textHeight"]
                    .as_f64()
                    .or_else(|| entry["text"]["textHeight"].as_f64())?,
                cell: Cell {
                    raw,
                    handle: string(&entry["handle"])?,
                    layer: string(&entry["layer"]),
                },
            })
        })
        .filter(|label| label.height > 0.0)
        .collect()
}

fn grid<'a>(
    owner: &'a str,
    bounds: &[&Span<'a>],
    vertical: &[Span<'a>],
    labels: &[Label<'a>],
    height: f64,
    epsilon: f64,
    table_entity: Option<&'a Value>,
) -> Option<Table<'a>> {
    let bottom = bounds.first()?.coord;
    let top = bounds.last()?.coord;
    let left = bounds[0].start;
    let right = bounds[0].end;
    let mut cols = vertical
        .iter()
        .filter(|line| {
            line.coord >= left - epsilon
                && line.coord <= right + epsilon
                && ((line.start <= bottom + epsilon && line.end >= top - epsilon)
                    || (table_entity.is_some() && line.end > bottom && line.start < top))
        })
        .collect::<Vec<_>>();
    if table_entity.is_some() {
        cols.dedup_by(|a, b| close(a.coord, b.coord, epsilon));
    }
    if !(3..=MAX_COLUMNS + 1).contains(&cols.len())
        || !close(cols[0].coord, left, epsilon)
        || !close(cols.last()?.coord, right, epsilon)
    {
        return None;
    }
    let mut rows = vec![vec![Vec::new(); cols.len() - 1]; bounds.len() - 1];
    let mut headers = labels
        .iter()
        .filter(|l| {
            l.x > left + epsilon
                && l.x < right - epsilon
                && l.y > top
                && l.y - top <= 4.0 * height
                && l.height >= height * 1.15
        })
        .collect::<Vec<_>>();
    headers.sort_by(|a, b| a.y.total_cmp(&b.y));
    let title = match headers.as_slice() {
        [head, ..] if headers.get(1).is_none_or(|next| next.y - head.y > epsilon) => Some(*head),
        _ => None,
    };
    let mut interior = labels
        .iter()
        .filter(|label| label.x >= left && label.x < right && label.y >= bottom && label.y < top)
        .collect::<Vec<_>>();
    interior.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));
    for label in interior {
        let row = bounds.len() - 1 - bounds.partition_point(|line| line.coord <= label.y);
        let Some(column) = cols
            .partition_point(|line| line.coord <= label.x)
            .checked_sub(1)
        else {
            continue;
        };
        if let Some(cell) = rows.get_mut(row).and_then(|row| row.get_mut(column)) {
            cell.push(label.cell.clone());
        }
    }
    if rows
        .iter()
        .filter(|row| row.iter().filter(|col| !col.is_empty()).count() >= 2)
        .count()
        < 2
    {
        return None;
    }
    let mut row_base = 0;
    let mut table_title = title.map(|l| plain(l.cell.raw));
    let top_row_bottom = bounds[bounds.len() - 2].coord;
    let merged_top = || {
        !vertical.iter().any(|line| {
            line.coord > left + epsilon
                && line.coord < right - epsilon
                && line.start < top - epsilon
                && line.end > top_row_bottom + epsilon
        })
    };
    if table_entity.is_some()
        && rows.len() >= 3
        && rows[0].iter().flatten().count() == 1
        && rows[1].iter().filter(|column| !column.is_empty()).count() >= 2
        && merged_top()
    {
        table_title = rows
            .remove(0)
            .into_iter()
            .flatten()
            .next()
            .map(|cell| plain(cell.raw));
        row_base = 1;
    }
    let mut header = header_row(&rows);
    if header.is_none()
        && bounds[bounds.len() - 1 - row_base].coord - bounds[bounds.len() - 2 - row_base].coord
            > 1.5 * height
        && rows[0].iter().filter(|cell| !cell.is_empty()).count() >= 2
    {
        let first = row_values(&rows[0]);
        if first
            .iter()
            .filter(|s| s.chars().any(char::is_alphabetic))
            .count()
            >= 2
            && first.iter().all(|s| !numeric(s))
        {
            header = Some(first);
        }
    }
    if header.is_none() && table_entity.is_none() {
        return None;
    }
    let grid_layers = bounds
        .iter()
        .flat_map(|line| line.layers.iter().copied())
        .chain(cols.iter().flat_map(|line| line.layers.iter().copied()))
        .collect();
    let (handle, method, origin, table_layer) = if let Some(entry) = table_entity {
        (
            string(&entry["handle"])?,
            "table_block",
            xy(&entry["startPoint"]),
            string(&entry["layer"]),
        )
    } else {
        (
            title.map_or(bounds.last()?.handle, |l| l.cell.handle),
            "grid",
            Some((left, top)),
            None,
        )
    };
    Some(Table {
        handle,
        title: table_title,
        owner,
        method,
        rows,
        headers: header,
        row_base,
        grid_layers,
        origin,
        table_layer,
    })
}

fn grid_tables<'a>(
    owner: &'a str,
    entries: &[&'a Value],
    labels: &[Label<'a>],
    table_entity: Option<&'a Value>,
) -> Vec<Table<'a>> {
    let lines = entries
        .iter()
        .copied()
        .filter(|e| e["type"] == "LINE" && e["isVisible"] != false)
        .take(MAX_LINES + 1)
        .collect::<Vec<_>>();
    if lines.len() > MAX_LINES || labels.len() < 4 {
        return Vec::new();
    }
    let mut heights = labels.iter().map(|l| l.height).collect::<Vec<_>>();
    heights.sort_by(f64::total_cmp);
    let height = heights[heights.len() / 2];
    let epsilon = height * 0.025;
    let horizontal = spans(&lines, true, epsilon);
    let vertical = spans(&lines, false, epsilon);
    let mut groups: Vec<Vec<&Span<'_>>> = Vec::new();
    for line in &horizontal {
        if let Some(group) = groups
            .iter_mut()
            .find(|g| close(g[0].start, line.start, epsilon) && close(g[0].end, line.end, epsilon))
        {
            group.push(line);
        } else {
            groups.push(vec![line]);
        }
    }
    let mut tables = Vec::new();
    for group in groups {
        let mut start = 0;
        for end in 1..=group.len() {
            let connected = end < group.len()
                && [group[end - 1].start, group[end - 1].end]
                    .into_iter()
                    .all(|x| {
                        vertical.iter().any(|line| {
                            close(line.coord, x, epsilon)
                                && line.start <= group[end - 1].coord + epsilon
                                && line.end >= group[end].coord - epsilon
                        })
                    });
            if connected {
                continue;
            }
            let bounds = &group[start..end];
            if (3..=MAX_ROWS + 1).contains(&bounds.len()) {
                if let Some(table) = grid(
                    owner,
                    bounds,
                    &vertical,
                    labels,
                    height,
                    epsilon,
                    table_entity,
                ) {
                    tables.push(table);
                }
            }
            start = end;
        }
    }
    tables
}

fn aligned<'a>(owner: &'a str, labels: &[Label<'a>], occupied: &BTreeSet<&str>) -> Vec<Table<'a>> {
    let mut layers = BTreeMap::<&str, Vec<&Label<'a>>>::new();
    for label in labels.iter().filter(|l| !occupied.contains(l.cell.handle)) {
        if let Some(layer) = label.cell.layer {
            layers.entry(layer).or_default().push(label);
        }
    }
    let mut tables = Vec::new();
    for mut layer in layers.into_values() {
        if !(8..=4_000).contains(&layer.len()) {
            continue;
        }
        layer.sort_by(|a, b| b.y.total_cmp(&a.y));
        let mut bands = Vec::new();
        let mut remaining = layer.as_mut_slice();
        while let Some(first) = remaining.first() {
            let (y, tolerance) = (first.y, first.height * 0.2);
            let len = remaining.partition_point(|l| y - l.y <= tolerance);
            let (band, rest) = remaining.split_at_mut(len);
            band.sort_by(|a, b| a.x.total_cmp(&b.x));
            bands.push(&*band);
            remaining = rest;
        }
        let mut index = 0;
        while index + 3 < bands.len() {
            let header = &bands[index];
            let gap = header[0].y - bands[index + 1][0].y;
            let height = bands[index + 1][0].height;
            let count = header.len();
            let valid_header = (2..=MAX_COLUMNS).contains(&count)
                && header.iter().all(|label| label.height >= height * 1.1)
                && header.windows(2).all(|pair| pair[1].x - pair[0].x > height);
            if !valid_header || gap < height || gap > 6.0 * height {
                index += 1;
                continue;
            }
            let n = bands[index + 1..]
                .windows(2)
                .take(MAX_ROWS - 2)
                .take_while(|pair| {
                    let dy = pair[0][0].y - pair[1][0].y;
                    dy >= height
                        && dy <= 6.0 * height
                        && [pair[0], pair[1]].into_iter().all(|band| {
                            band.len() == count
                                && band.iter().zip(header.iter()).all(|(label, head)| {
                                    (label.x - head.x).abs() <= height * 0.35
                                        && label.height <= head.height / 1.1
                                })
                        })
                })
                .count()
                + 1;
            if n < 3 {
                index += 1;
                continue;
            }
            let rows = bands[index..=index + n]
                .iter()
                .map(|band| band.iter().map(|l| vec![l.cell.clone()]).collect())
                .collect::<Vec<Row<'_>>>();
            if let Some(headers) = header_row(&rows) {
                tables.push(Table {
                    handle: header[0].cell.handle,
                    title: None,
                    owner,
                    method: "aligned",
                    rows,
                    headers: Some(headers),
                    row_base: 0,
                    grid_layers: BTreeSet::new(),
                    origin: Some((header[0].x, header[0].y)),
                    table_layer: None,
                });
                index += n + 1;
            } else {
                index += 1;
            }
        }
    }
    tables
}

pub(super) fn objects(
    elements: &[Value],
    layouts: &[Value],
    blocks: &[Value],
    sheets: &BTreeMap<&str, Value>,
    layer_handles: &BTreeMap<&str, &str>,
    max_rows: usize,
) -> Vec<Value> {
    let owners = layouts
        .iter()
        .filter_map(|entry| string(&entry["paperSpaceTableId"]))
        .collect::<BTreeSet<_>>();
    let mut by_owner = BTreeMap::<&str, Vec<&Value>>::new();
    let table_blocks = blocks
        .iter()
        .filter_map(|block| {
            string(&block["name"]).filter(|name| name.starts_with("*T"))?;
            string(&block["handle"])
        })
        .collect::<BTreeSet<_>>();
    for entry in elements {
        if let Some(owner) = string(&entry["ownerBlockRecordSoftId"])
            .filter(|id| owners.contains(id) || table_blocks.contains(id))
        {
            by_owner.entry(owner).or_default().push(entry);
        }
    }
    let mut result = Vec::new();
    for (&owner, entries) in by_owner
        .iter()
        .filter(|(owner, _)| owners.contains(**owner))
    {
        let local_labels = labels(entries);
        let mut tables = entries
            .iter()
            .filter(|e| e["type"] == "ACAD_TABLE")
            .filter_map(|entry| {
                if let Some(table) = native(entry, owner) {
                    return Some(table);
                }
                let block =
                    string(&entry["blockRecordHandle"]).filter(|h| table_blocks.contains(h))?;
                let content = by_owner.get(block)?;
                let mut tables = grid_tables(owner, content, &labels(content), Some(entry));
                if tables.len() == 1 {
                    tables.pop()
                } else {
                    None
                }
            })
            .chain(grid_tables(owner, entries, &local_labels, None))
            .collect::<Vec<_>>();
        let occupied = tables
            .iter()
            .flat_map(|table| {
                std::iter::once(table.handle).chain(
                    table
                        .rows
                        .iter()
                        .flatten()
                        .flatten()
                        .map(|cell| cell.handle),
                )
            })
            .collect::<BTreeSet<_>>();
        tables.extend(aligned(owner, &local_labels, &occupied));
        tables.sort_by(|a, b| match (a.origin, b.origin) {
            (Some((ax, ay)), Some((bx, by))) => by.total_cmp(&ay).then(ax.total_cmp(&bx)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.handle.cmp(b.handle),
        });
        for (order, table) in tables.into_iter().enumerate() {
            let rows = into_objects(table, sheets, layer_handles, order);
            if rows.len() <= max_rows.saturating_sub(result.len())
                && rows.iter().all(|row| row.to_string().len() <= 64 * 1024)
            {
                result.extend(rows);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(handle: &str, x: f64, y: f64, height: f64, value: &str) -> Value {
        json!({"handle":handle,"type":"MTEXT","ownerBlockRecordSoftId":"A",
            "insertionPoint":{"x":x,"y":y},"text":value,"textHeight":height,"layer":"Labels"})
    }

    fn project(elements: &[Value]) -> Vec<Value> {
        objects(
            elements,
            &[json!({"paperSpaceTableId":"A"})],
            &[],
            &BTreeMap::from([("A", json!({"name":"Layout1","handle":"B"}))]),
            &BTreeMap::from([("Labels", "C")]),
            100,
        )
    }

    #[test]
    fn joins_cells_in_a_grid_across_layers_and_preserves_provenance() {
        let mut elements = Vec::new();
        for (i, y) in [0, 4, 8, 12].into_iter().enumerate() {
            for (j, (from, to)) in [(0, 10), (10, 20)].into_iter().enumerate() {
                elements.push(
                    json!({"handle":format!("H{i}{j}"),"type":"LINE","ownerBlockRecordSoftId":"A",
                    "layer":"Grid","startPoint":{"x":from,"y":y},"endPoint":{"x":to,"y":y}}),
                );
            }
        }
        for (i, x) in [0, 10, 20].into_iter().enumerate() {
            elements.push(
                json!({"handle":format!("V{i}"),"type":"LINE","ownerBlockRecordSoftId":"A",
                "layer":"Grid","startPoint":{"x":x,"y":0},"endPoint":{"x":x,"y":12}}),
            );
        }
        elements.extend([
            text("title", 10.0, 14.0, 1.5, "Room schedule"),
            text("name", 5.0, 10.0, 1.0, "Name"),
            text("area", 15.0, 10.0, 1.0, "Area"),
            text("room", 2.0, 6.0, 1.0, "Corridor"),
            text("value", 14.0, 6.0, 1.0, "28,75"),
            text("room2", 2.0, 2.0, 1.0, "Office"),
            text("value2", 14.0, 2.0, 1.0, "15.20"),
            text("unrelated", 22.0, 6.0, 1.0, "100"),
        ]);
        let rows = project(&elements);
        assert_eq!(rows.len(), 2);
        let row = &rows[0]["dwg_table_row"];
        assert_eq!(row["table_handle"], "title");
        assert_eq!(row["grid_layers"], json!(["Grid"]));
        assert_eq!(row["cells"][1]["text"], "28,75");
        assert_eq!(row["cells"][1]["sources"][0]["layer_handle"], "C");
        assert_eq!(row["cells"][1]["sources"][0]["handle"], "value");
        for entry in &mut elements {
            entry["ownerBlockRecordSoftId"] = json!("PV");
        }
        elements.push(
            json!({"handle":"19","type":"ACAD_TABLE","ownerBlockRecordSoftId":"A",
            "blockRecordHandle":"PV","rowCount":0,"columnCount":0,"cells":[],"layer":"Main"}),
        );
        let preview_rows = objects(
            &elements,
            &[json!({"paperSpaceTableId":"A"})],
            &[json!({"handle":"PV","name":"*T1"})],
            &BTreeMap::new(),
            &BTreeMap::from([("Labels", "C")]),
            100,
        );
        assert_eq!(preview_rows.len(), 2);
        assert_eq!(preview_rows[0]["native_id"], "19");
        assert_eq!(preview_rows[0]["dwg_table_row"]["method"], "table_block");
        assert_eq!(
            preview_rows[0]["dwg_table_row"]["layers"],
            json!(["Labels", "Main"])
        );
    }

    #[test]
    fn reads_native_table_cells_as_saved_rows() {
        let rows = project(&[
            json!({"type":"ACAD_TABLE","handle":"19","ownerBlockRecordSoftId":"A",
            "layer":"Labels","rowCount":3,"columnCount":2,"cells":[
                {"text":"Name"},{"text":"Area"},{"text":"Corridor"},{"text":"28,75"},
                {"text":"Office"},{"text":"15.20"}
            ]}),
        ]);
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0]["object_id"], rows[1]["object_id"]);
        assert_eq!(rows[0]["native_id"], rows[1]["native_id"]);
        assert_eq!(rows[1]["dwg_table_row"]["row"], 3);
        assert_eq!(rows[1]["dwg_table_row"]["method"], "native");
        assert_eq!(rows[1]["dwg_table_row"]["cells"][1]["text"], "15.20");
    }

    #[test]
    fn requires_repeated_aligned_rows_and_a_header_when_no_grid_exists() {
        let mut elements = vec![
            text("head1", 1.0, 16.0, 1.2, "Name"),
            text("head2", 8.0, 16.0, 1.2, "Area"),
            text("a1", 1.0, 12.0, 1.0, "Corridor"),
            text("a2", 8.0, 12.0, 1.0, "28,75"),
            text("b1", 1.0, 8.0, 1.0, "Office"),
            text("b2", 8.0, 8.0, 1.0, "42"),
            text("c1", 1.0, 4.0, 1.0, "Store"),
            text("c2", 8.0, 4.0, 1.0, "12"),
        ];
        let rows = project(&elements);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["dwg_table_row"]["method"], "aligned");
        elements[7]["insertionPoint"]["x"] = json!(9);
        assert!(project(&elements).is_empty());
    }
}
