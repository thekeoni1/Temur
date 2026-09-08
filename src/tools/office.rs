//! T54 (D20/D23): text out of PDFs and office documents, pure Rust.
//!
//! The dogfood shape both findings share is a model reaching for a system
//! tool the machine lacks: pdftotext for a resume, libreoffice for a
//! spreadsheet. temur is a zero-runtime-dependency static binary, so the
//! fitting answer is a compiled-in capability, never a shell-out.
//!
//! Everything here returns TEXT. The read tool then pages it through its
//! existing offset / limit / MAX_LINE_LENGTH / MAX_BYTES pipeline, so a
//! 300-page PDF is paged exactly the way a long log file is.
//!
//! 32-bit discipline: every size and offset is u64, and the caps below are
//! hard because the address space is not. Zip input is treated as hostile.

use super::ToolError;
use std::io::Read;
use std::path::Path;

/// Largest document accepted at all. A 32-bit address space is the reason
/// this is a hard refusal rather than a best effort.
pub const MAX_INPUT_BYTES: u64 = 32 * 1024 * 1024;

/// Ceiling on what a zip-based format (xlsx, ods, docx) may inflate to in
/// total. Only the entries actually needed are opened, so this bounds a
/// decompression bomb rather than an honest large workbook.
pub const MAX_UNZIPPED_BYTES: u64 = 64 * 1024 * 1024;

/// Extensions this module can turn into text. The read tool consults this
/// before its binary refusal.
pub fn is_document(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "pdf" | "xlsx" | "xlsm" | "xls" | "ods" | "docx"
    )
}

/// Text of a document, or ONE honest sentence the model can act on.
///
/// `want_lines` is how many lines the caller can actually render (its
/// offset+limit window). The PDF path stops producing pages once it has
/// that many, so showing page one of a 300-page report does not cost the
/// whole report. The returned flag is false when extraction stopped early,
/// which is how the read tool knows not to quote a total line count.
pub fn extract(path: &Path, want_lines: u64) -> Result<(String, bool), ToolError> {
    let meta = std::fs::metadata(path).map_err(|e| ToolError::failed(e.to_string()))?;
    if meta.len() > MAX_INPUT_BYTES {
        return Err(ToolError::failed(format!(
            "This file is {} MiB, over the {} MiB limit for documents temur reads; \
             ask the user for an excerpt, or extract the part you need with bash.",
            meta.len() / (1024 * 1024),
            MAX_INPUT_BYTES / (1024 * 1024)
        )));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => pdf(path, want_lines),
        // Both of these parse their whole input by construction: a workbook
        // is one zip and a docx body is one XML part, so there is no
        // partial state to report and the text is always complete.
        "xlsx" | "xlsm" | "xls" | "ods" => spreadsheet(path).map(|t| (t, true)),
        "docx" => docx(path).map(|t| (t, true)),
        other => Err(ToolError::failed(format!(
            "temur cannot read {other} files as text."
        ))),
    }
}

// --------------------------------------------------------------- PDF

/// PDF text, page at a time, under `catch_unwind`.
///
/// pdf-extract panics rather than erroring on malformed input: the T54 P0
/// spike hit 3 panics in 25 seeded corruptions, both sites its own
/// `.expect()` (`missing object reference`, `wrong type`). A tool that
/// aborts the process on a bad file is worse than one that says it cannot
/// read it, so extraction runs under `catch_unwind` and a caught panic
/// becomes the ordinary error sentence below. The payload is NEVER shown:
/// it is an internal crate message that tells the model nothing it can act
/// on, and printing it would invite the model to debug our dependency.
fn pdf(path: &Path, want_lines: u64) -> Result<(String, bool), ToolError> {
    let bytes = std::fs::read(path).map_err(|e| ToolError::failed(e.to_string()))?;
    let hushed = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_pages(&bytes, want_lines)
    }));
    std::panic::set_hook(hushed);

    let (text, complete) = match caught {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(ToolError::failed(
                "temur could not read this PDF: it is malformed or uses a feature temur \
                 does not support. Ask the user for the text.",
            ))
        }
    };
    if text.trim().is_empty() {
        return Err(ToolError::failed(
            "This PDF has no text layer (it is probably a scan or images); \
             temur cannot see images. Ask the user for the text.",
        ));
    }
    Ok((text, complete))
}

/// Page-at-a-time extraction, stopping once `want_lines` lines exist.
fn pdf_pages(bytes: &[u8], want_lines: u64) -> Result<(String, bool), ToolError> {
    // lopdf through pdf-extract's own re-export, so the Document type is
    // definitionally the one output_doc_page expects and the two can never
    // drift to different lopdf versions.
    let doc =
        pdf_extract::Document::load_mem(bytes).map_err(|e| pdf_open_error(&e.to_string()))?;
    let mut pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    pages.sort_unstable();
    let mut out = String::new();
    let mut complete = true;
    for (i, page) in pages.iter().enumerate() {
        {
            let mut sink = pdf_extract::PlainTextOutput::new(&mut out);
            pdf_extract::output_doc_page(&doc, &mut sink, *page)
                .map_err(|e| ToolError::failed(format!("temur could not read this PDF: {e}")))?;
        }
        // The window the caller can actually render is the stopping rule:
        // extracting the rest would be work whose output is discarded.
        // One line beyond the window, so the caller can tell "more follows"
        // from "that was all".
        if out.lines().count() as u64 > want_lines && i + 1 < pages.len() {
            complete = false;
            break;
        }
    }
    Ok((out, complete))
}

/// Encryption is the one PDF failure worth naming precisely, because the
/// user can do something about it and no amount of retrying helps.
fn pdf_open_error(msg: &str) -> ToolError {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("encrypt") || lower.contains("password") {
        return ToolError::failed(
            "This PDF is encrypted; temur cannot open it. Ask the user for an \
             unencrypted copy or for the text.",
        );
    }
    ToolError::failed(
        "temur could not read this PDF: it is malformed or not a PDF. Ask the user \
         for the text.",
    )
}

// -------------------------------------------------------- spreadsheets

/// One block per sheet, rows as CSV lines.
///
/// CACHED VALUES ONLY, never formulas: calamine yields what the writing
/// application last computed, which is the number a person would see. temur
/// evaluates nothing, so a workbook whose cached values are stale shows the
/// stale value rather than a guess.
fn spreadsheet(path: &Path) -> Result<String, ToolError> {
    use calamine::{open_workbook_auto, Reader};
    let mut wb = open_workbook_auto(path).map_err(|e| spreadsheet_error(&e.to_string()))?;
    let mut out = String::new();
    for name in wb.sheet_names().to_vec() {
        let range = match wb.worksheet_range(&name) {
            Ok(r) => r,
            Err(e) => return Err(spreadsheet_error(&e.to_string())),
        };
        out.push_str(&format!("== Sheet: {name} ==\n"));
        for row in range.rows() {
            let mut cells: Vec<String> = row.iter().map(cell_text).collect();
            // Trailing empties are formatting, not data.
            while cells.last().map(|c| c.is_empty()).unwrap_or(false) {
                cells.pop();
            }
            out.push_str(&csv_row(&cells));
            out.push('\n');
        }
    }
    if out.is_empty() {
        return Err(ToolError::failed(
            "This workbook has no sheets temur can read.",
        ));
    }
    Ok(out)
}

fn cell_text(c: &calamine::Data) -> String {
    use calamine::Data;
    match c {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        // Whole numbers render as integers: a spreadsheet showing 42 should
        // not read back as 42.0 and invite the model to "fix" it.
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                format!("{f}")
            }
        }
        Data::Int(i) => format!("{i}"),
        Data::Bool(b) => format!("{b}"),
        Data::Error(e) => format!("#ERR:{e:?}"),
        Data::DateTime(d) => format!("{d}"),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
    }
}

/// RFC-4180-ish quoting, so a cell containing a comma survives the round
/// trip through P2's CSV writer.
fn csv_row(cells: &[String]) -> String {
    cells
        .iter()
        .map(|c| {
            if c.contains([',', '"', '\n', '\r']) {
                format!("\"{}\"", c.replace('"', "\"\""))
            } else {
                c.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn spreadsheet_error(msg: &str) -> ToolError {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("password") || lower.contains("encrypt") {
        return ToolError::failed(
            "This workbook is password protected; temur cannot open it. Ask the user \
             for an unprotected copy.",
        );
    }
    ToolError::failed(
        "temur could not read this workbook: it is malformed or not a spreadsheet.",
    )
}

// ---------------------------------------------------------------- docx

/// docx text by hand: paragraphs as lines, table rows as tab-separated
/// cells. No docx crate; word/document.xml is a small enough grammar that
/// one would be more dependency than parser.
fn docx(path: &Path) -> Result<String, ToolError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let xml = zip_entry_to_string(path, "word/document.xml")?;
    let mut reader = Reader::from_str(&xml);
    let mut out = String::new();
    let mut cur = String::new();
    let mut cells: Vec<String> = Vec::new();
    let mut in_table = false;

    loop {
        match reader.read_event() {
            Err(_) => {
                return Err(ToolError::failed(
                    "temur could not read this document: its XML is malformed.",
                ))
            }
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let ln = e.local_name();
                let n: &[u8] = ln.as_ref();
                if n == b"tbl" {
                    in_table = true;
                } else if n == b"tr" {
                    cells.clear();
                } else if n == b"tc" || (n == b"p" && !in_table) {
                    cur.clear();
                }
            }
            Ok(Event::Text(t)) => {
                if let Ok(d) = t.decode() {
                    cur.push_str(&d);
                }
            }
            // Entity references arrive as their OWN event: ignoring them
            // silently drops "&" and friends from the text. Found in the
            // T54 P0 spike, where a sentence lost its ampersand.
            Ok(Event::GeneralRef(r)) => {
                if let Ok(d) = r.decode() {
                    if let Some(res) = quick_xml::escape::resolve_predefined_entity(&d) {
                        cur.push_str(res);
                    } else if let Ok(Some(ch)) = r.resolve_char_ref() {
                        cur.push(ch);
                    }
                }
            }
            Ok(Event::End(e)) => {
                let ln = e.local_name();
                let n: &[u8] = ln.as_ref();
                if n == b"tbl" {
                    in_table = false;
                } else if n == b"tc" {
                    cells.push(cur.trim().to_string());
                } else if n == b"tr" {
                    out.push_str(&cells.join("\t"));
                    out.push('\n');
                } else if n == b"p" && !in_table {
                    out.push_str(cur.trim());
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
    if out.trim().is_empty() {
        return Err(ToolError::failed(
            "This document has no readable text (it may be empty or contain only images).",
        ));
    }
    Ok(out)
}

/// One named entry out of a zip container, with the decompression cap
/// applied. Only the entry asked for is opened: a docx may carry megabytes
/// of media that nothing here needs to touch.
fn zip_entry_to_string(path: &Path, entry: &str) -> Result<String, ToolError> {
    let file = std::fs::File::open(path).map_err(|e| ToolError::failed(e.to_string()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| {
        ToolError::failed(
            "temur could not read this document: it is malformed or not a Word document.",
        )
    })?;
    let zf = archive.by_name(entry).map_err(|_| {
        ToolError::failed(
            "temur could not read this document: it is missing its main body part, so it \
             is probably not a Word document.",
        )
    })?;
    // Declared size first, so a bomb is refused before a byte is inflated.
    if zf.size() > MAX_UNZIPPED_BYTES {
        return Err(ToolError::failed(format!(
            "This document expands to more than {} MiB; temur will not unpack it.",
            MAX_UNZIPPED_BYTES / (1024 * 1024)
        )));
    }
    // ...and a bounded read anyway, because the declared size is the
    // archive's claim about itself and this input is treated as hostile.
    let mut buf = Vec::new();
    zf.take(MAX_UNZIPPED_BYTES)
        .read_to_end(&mut buf)
        .map_err(|_| {
            ToolError::failed("temur could not read this document: its contents are corrupt.")
        })?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ------------------------------------------------------- writing xlsx

/// A cell as it will be written: a number, or text.
///
/// The ONLY inference is numeric, and it is deliberately narrow. Anything
/// that is not a plain integer or float stays text, including dates,
/// currency and percentages, because guessing a format is how a
/// spreadsheet silently changes someone's data.
enum Cell {
    Number(f64),
    Text(String),
}

/// FORMULA INJECTION IS NOT POSSIBLE HERE, by construction: nothing in
/// this module calls a formula-writing API, so a cell whose text begins
/// with `=`, `+`, `-` or `@` is written as the literal text it is. That is
/// the same rule a careful CSV importer applies, and it matters more here
/// because the CSV is written by a model acting on someone else's input.
fn classify(field: &str) -> Cell {
    // A leading `-` is only ever a formula lead-in when what follows is
    // NOT a number, so "-5" stays the number it obviously is.
    if let Ok(i) = field.parse::<i64>() {
        return Cell::Number(i as f64);
    }
    if let Ok(f) = field.parse::<f64>() {
        if f.is_finite() {
            return Cell::Number(f);
        }
    }
    Cell::Text(field.to_string())
}

/// RFC-4180-ish CSV: quoted fields, doubled quotes inside them, embedded
/// commas and newlines. The first row is DATA, not a header: temur has no
/// way to know which it is and pretending otherwise loses a row.
fn parse_csv(content: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = content.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    } else if any && rows.is_empty() {
        rows.push(vec![String::new()]);
    }
    // A trailing newline ends the last row; it does not add an empty one.
    rows
}

/// Write CSV text as a one-sheet workbook. Used by the write tool when
/// filePath ends in .xlsx, so no new tool and no new parameter exist.
pub fn write_csv_as_xlsx(path: &Path, content: &str) -> Result<(), ToolError> {
    let rows = parse_csv(content);
    let mut wb = rust_xlsxwriter::Workbook::new();
    let ws = wb.add_worksheet();
    for (r, row) in rows.iter().enumerate() {
        // u32 row / u16 col are rust_xlsxwriter's own types; the bounds
        // below are Excel's, and exceeding them is a refusal rather than a
        // silently short workbook.
        let r32 = u32::try_from(r).map_err(|_| too_big())?;
        if r32 >= 1_048_576 {
            return Err(too_big());
        }
        for (c, field) in row.iter().enumerate() {
            let c16 = u16::try_from(c).map_err(|_| too_big())?;
            if c16 >= 16_384 {
                return Err(too_big());
            }
            match classify(field) {
                Cell::Number(n) => ws.write_number(r32, c16, n),
                Cell::Text(t) => ws.write_string(r32, c16, &t),
            }
            .map_err(|e| ToolError::failed(format!("temur could not write this workbook: {e}")))?;
        }
    }
    wb.save(path)
        .map_err(|e| ToolError::failed(format!("temur could not write this workbook: {e}")))?;
    Ok(())
}

fn too_big() -> ToolError {
    ToolError::failed(
        "That is more rows or columns than a worksheet can hold (1,048,576 by 16,384); \
         write it as CSV instead.",
    )
}

// ------------------------------------------------- workbooks with charts

/// One cell of a `spreadsheet` call: values only, never a formula.
///
/// rust_xlsxwriter writes a formula WITHOUT a cached result, so a workbook
/// carrying one reads back as 0 until a spreadsheet application opens and
/// recalculates it (measured in the T54 P0 spike). A tool whose output
/// reads back as zeroes is worse than one that refuses formulas, so this
/// surface has no formula at all.
pub enum SheetCell {
    Number(f64),
    Text(String),
    Empty,
}

pub struct SheetSpec {
    pub name: String,
    pub rows: Vec<Vec<SheetCell>>,
}

pub struct SeriesSpec {
    pub name: Option<String>,
    /// (first_row, first_col, last_row, last_col), 0-indexed.
    pub values: (u32, u16, u32, u16),
}

pub struct ChartSpec {
    pub sheet: String,
    pub kind: String,
    pub title: Option<String>,
    pub categories: Option<(u32, u16, u32, u16)>,
    pub series: Vec<SeriesSpec>,
    /// (row, col) of the chart's top-left corner.
    pub anchor: (u32, u16),
}

/// Parse an A1 range ("A2:A12", or "B7" for a single cell) into 0-indexed
/// (first_row, first_col, last_row, last_col).
pub fn parse_a1_range(s: &str) -> Option<(u32, u16, u32, u16)> {
    let (a, b) = match s.split_once(':') {
        Some((a, b)) => (a, b),
        None => (s, s),
    };
    let (r1, c1) = parse_a1_cell(a)?;
    let (r2, c2) = parse_a1_cell(b)?;
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

/// Parse a single A1 cell reference into 0-indexed (row, col).
pub fn parse_a1_cell(s: &str) -> Option<(u32, u16)> {
    let s = s.trim().trim_start_matches('$');
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    let letters = letters.trim_end_matches('$');
    if letters.is_empty() || digits.is_empty() {
        return None;
    }
    let mut col: u32 = 0;
    for ch in letters.chars() {
        if !ch.is_ascii_alphabetic() {
            return None;
        }
        col = col
            .checked_mul(26)?
            .checked_add(ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1)?;
    }
    let row: u32 = digits.parse().ok()?;
    if row == 0 || col == 0 {
        return None;
    }
    Some((row - 1, u16::try_from(col - 1).ok()?))
}

/// Write sheets and charts as one workbook.
///
/// Every range is validated against the sheet's real extent BEFORE
/// anything is written, and an out-of-range range is an error naming
/// itself. A chart pointed at cells that do not exist renders as an empty
/// frame, which looks like a temur bug to the user and tells the model
/// nothing; saying so is the whole point.
pub fn write_workbook(
    path: &Path,
    sheets: &[SheetSpec],
    charts: &[ChartSpec],
) -> Result<(), ToolError> {
    use rust_xlsxwriter::{Chart, ChartType, Workbook};

    if sheets.is_empty() {
        return Err(ToolError::failed(
            "A workbook needs at least one sheet; pass sheets: [{\"name\": ..., \"rows\": [...]}].",
        ));
    }
    let mut wb = Workbook::new();
    for spec in sheets {
        let ws = wb
            .add_worksheet()
            .set_name(&spec.name)
            .map_err(|e| ToolError::failed(format!("sheet \"{}\": {e}", spec.name)))?;
        for (r, row) in spec.rows.iter().enumerate() {
            let r32 = u32::try_from(r).map_err(|_| too_big())?;
            if r32 >= 1_048_576 {
                return Err(too_big());
            }
            for (c, cell) in row.iter().enumerate() {
                let c16 = u16::try_from(c).map_err(|_| too_big())?;
                if c16 >= 16_384 {
                    return Err(too_big());
                }
                match cell {
                    SheetCell::Number(n) => ws.write_number(r32, c16, *n).map(|_| ()),
                    SheetCell::Text(t) => ws.write_string(r32, c16, t).map(|_| ()),
                    SheetCell::Empty => Ok(()),
                }
                .map_err(|e| ToolError::failed(format!("sheet \"{}\": {e}", spec.name)))?;
            }
        }
    }

    for ch in charts {
        let target = sheets
            .iter()
            .find(|s| s.name == ch.sheet)
            .ok_or_else(|| {
                ToolError::failed(format!(
                    "chart names sheet \"{}\", which is not one of the sheets written.",
                    ch.sheet
                ))
            })?;
        let rows = target.rows.len() as u32;
        let cols = target.rows.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
        let check = |what: &str, r: (u32, u16, u32, u16)| -> Result<(), ToolError> {
            if r.2 >= rows || u32::from(r.3) >= cols {
                return Err(ToolError::failed(format!(
                    "chart {what} {} is outside sheet \"{}\", which holds {} rows by {} columns.",
                    a1(r),
                    target.name,
                    rows,
                    cols
                )));
            }
            Ok(())
        };
        if let Some(cat) = ch.categories {
            check("categories", cat)?;
        }
        for s in &ch.series {
            check("series", s.values)?;
        }

        let kind = match ch.kind.to_ascii_lowercase().as_str() {
            "line" => ChartType::Line,
            "column" => ChartType::Column,
            "bar" => ChartType::Bar,
            "scatter" => ChartType::Scatter,
            "pie" => ChartType::Pie,
            other => {
                return Err(ToolError::failed(format!(
                    "chart type \"{other}\" is not one of: line, column, bar, scatter, pie."
                )))
            }
        };
        let mut chart = Chart::new(kind);
        if let Some(t) = &ch.title {
            chart.title().set_name(t);
        }
        for s in &ch.series {
            let ser = chart.add_series();
            ser.set_values((ch.sheet.as_str(), s.values.0, s.values.1, s.values.2, s.values.3));
            if let Some(cat) = ch.categories {
                ser.set_categories((ch.sheet.as_str(), cat.0, cat.1, cat.2, cat.3));
            }
            if let Some(n) = &s.name {
                ser.set_name(n);
            }
        }
        let ws = wb
            .worksheet_from_name(&ch.sheet)
            .map_err(|e| ToolError::failed(format!("chart sheet \"{}\": {e}", ch.sheet)))?;
        ws.insert_chart(ch.anchor.0, ch.anchor.1, &chart)
            .map_err(|e| ToolError::failed(format!("chart: {e}")))?;
    }

    wb.save(path)
        .map_err(|e| ToolError::failed(format!("temur could not write this workbook: {e}")))?;
    Ok(())
}

/// Render a 0-indexed range back as A1, so an error names the range the
/// caller actually wrote rather than internal coordinates.
fn a1(r: (u32, u16, u32, u16)) -> String {
    format!("{}{}:{}{}", col_letters(r.1), r.0 + 1, col_letters(r.3), r.2 + 1)
}

fn col_letters(mut c: u16) -> String {
    let mut out = String::new();
    loop {
        out.insert(0, (b'A' + (c % 26) as u8) as char);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    out
}
