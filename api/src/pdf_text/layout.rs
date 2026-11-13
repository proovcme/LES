//! Geometry, not content-stream order, associates table cells. No OCR or
//! subject-specific labels, quantities, sums or inferred missing values.
mod geometry;
use super::*;
use geometry::{Glyph, Line, Matrix};
// fpdf_edit.h constants from the pinned PDFium 7763 bindings.
const MOVE_TO: u32 = 2;
const LINE_TO: u32 = 0;

pub(super) fn read(
    pdf: &dyn PdfiumLibraryBindings,
    page: FPDF_PAGE,
    text: FPDF_TEXTPAGE,
) -> Option<String> {
    let mut horizontal = Vec::new();
    let mut vertical = Vec::new();
    let mut count = 0;
    paths(
        pdf,
        page,
        None,
        Matrix::identity(),
        0,
        &mut count,
        &mut horizontal,
        &mut vertical,
    )?;
    if horizontal.len() < 3 || vertical.len() < 3 {
        return None;
    }
    let n = unsafe { pdf.FPDFText_CountChars(text) };
    if !(1..=200_000).contains(&n) {
        return None;
    }
    let mut glyphs = Vec::new();
    for i in 0..n {
        let c = char::from_u32(unsafe { pdf.FPDFText_GetUnicode(text, i) })?;
        if c.is_control() {
            continue;
        }
        let (mut l, mut r, mut b, mut t, mut x, mut y) = (0., 0., 0., 0., 0., 0.);
        if unsafe { pdf.FPDFText_GetCharBox(text, i, &mut l, &mut r, &mut b, &mut t) } == 0
            || unsafe { pdf.FPDFText_GetCharOrigin(text, i, &mut x, &mut y) } == 0
        {
            return None;
        }
        if [l, r, b, t, x, y].iter().any(|n| !f64::is_finite(*n)) {
            return None;
        }
        glyphs.push(Glyph {
            c,
            x: (l + r) / 2.,
            y: (b + t) / 2.,
            baseline: y,
            left: l,
            right: r,
            order: i,
            rotated: unsafe { pdf.FPDFText_GetCharAngle(text, i) }.abs() > 0.05,
        });
    }
    geometry::reflow(glyphs, horizontal, vertical)
}

fn paths(
    pdf: &dyn PdfiumLibraryBindings,
    page: FPDF_PAGE,
    form: Option<FPDF_PAGEOBJECT>,
    parent: Matrix,
    depth: u8,
    count: &mut usize,
    horizontal: &mut Vec<Line>,
    vertical: &mut Vec<Line>,
) -> Option<()> {
    if depth > 14 {
        return None;
    }
    let n = unsafe {
        form.map_or_else(
            || pdf.FPDFPage_CountObjects(page),
            |f| pdf.FPDFFormObj_CountObjects(f),
        )
    };
    if n < 0 {
        return None;
    }
    for i in 0..n {
        *count += 1;
        if *count > 100_000 {
            return None;
        }
        let object = unsafe {
            form.map_or_else(
                || pdf.FPDFPage_GetObject(page, i),
                |f| pdf.FPDFFormObj_GetObject(f, i as _),
            )
        };
        if object.is_null() {
            return None;
        }
        let kind = unsafe { pdf.FPDFPageObj_GetType(object) } as u32;
        if kind != FPDF_PAGEOBJ_PATH && kind != FPDF_PAGEOBJ_FORM {
            continue;
        }
        let mut m = FS_MATRIX {
            a: 1.,
            b: 0.,
            c: 0.,
            d: 1.,
            e: 0.,
            f: 0.,
        };
        if unsafe { pdf.FPDFPageObj_GetMatrix(object, &mut m) } == 0 {
            return None;
        }
        let matrix = parent.compose(Matrix([
            m.a as f64, m.b as f64, m.c as f64, m.d as f64, m.e as f64, m.f as f64,
        ]));
        if kind == FPDF_PAGEOBJ_FORM {
            paths(
                pdf,
                page,
                Some(object),
                matrix,
                depth + 1,
                count,
                horizontal,
                vertical,
            )?;
            continue;
        }
        let n = unsafe { pdf.FPDFPath_CountSegments(object) };
        if n < 0 || n > 100_000 {
            return None;
        }
        let mut previous = None;
        let mut first = None;
        for j in 0..n {
            *count += 1;
            if *count > 100_000 {
                return None;
            }
            let segment = unsafe { pdf.FPDFPath_GetPathSegment(object, j) };
            let (mut x, mut y) = (0., 0.);
            if segment.is_null()
                || unsafe { pdf.FPDFPathSegment_GetPoint(segment, &mut x, &mut y) } == 0
            {
                return None;
            }
            let point = matrix.point(x as f64, y as f64);
            match unsafe { pdf.FPDFPathSegment_GetType(segment) } as u32 {
                MOVE_TO => first = Some(point),
                LINE_TO => {
                    if let Some(before) = previous {
                        geometry::line(before, point, horizontal, vertical);
                    }
                }
                _ => (),
            }
            if unsafe { pdf.FPDFPathSegment_GetClose(segment) } != 0 {
                if let Some(first) = first {
                    geometry::line(point, first, horizontal, vertical);
                }
            }
            previous = Some(point);
        }
    }
    Some(())
}
