//! T54 P3 (D23): the one new tool surface, for charts.
//!
//! Everything else in T54 rides an existing tool on purpose: a tool
//! definition is prompt tokens on every turn for every model, and weak
//! models degrade as the tool list grows (T29/T30). A chart is the one
//! thing the write tool's single string of content cannot express, so it
//! is the one thing that earns a surface of its own.

use super::office::{self, ChartSpec, SeriesSpec, SheetCell, SheetSpec};
use super::{parse_input, resolve_path, Tool, ToolCtx, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Params {
    #[serde(rename = "filePath")]
    file_path: String,
    sheets: Vec<SheetIn>,
    #[serde(default)]
    charts: Vec<ChartIn>,
}

#[derive(Deserialize)]
struct SheetIn {
    name: String,
    rows: Vec<Vec<Value>>,
}

#[derive(Deserialize)]
struct ChartIn {
    sheet: String,
    // DECLARED as "chartType", ACCEPTED as either. T34 pins that no tool
    // schema may carry a union type, because some chat templates stringify
    // a schema by dict lookup on "type" and a mismatch there turns into
    // HTTP 400 on every turn; a property literally NAMED "type" invites
    // exactly that confusion in exactly those templates. Per T33 the
    // tolerance lives at the argument boundary instead, so a model that
    // writes the obvious "type" is still understood.
    #[serde(rename = "chartType", alias = "type")]
    kind: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    categories: Option<String>,
    series: Vec<SeriesIn>,
    #[serde(default)]
    anchor: Option<String>,
}

#[derive(Deserialize)]
struct SeriesIn {
    #[serde(default)]
    name: Option<String>,
    values: String,
}

pub struct SpreadsheetTool;

impl Tool for SpreadsheetTool {
    /// It writes a file, so it asks exactly where write and edit ask.
    fn approval_site(&self) -> super::ApprovalSite {
        super::ApprovalSite::Registry
    }

    fn name(&self) -> &'static str {
        "spreadsheet"
    }
    fn description(&self) -> &'static str {
        include_str!("prompts/spreadsheet.txt")
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": {"type": "string", "description": "Absolute path of the .xlsx file to write"},
                "sheets": {
                    "type": "array",
                    "description": "One or more sheets, each with a name and rows of values",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"},
                            "rows": {
                                "type": "array",
                                "items": {
                                    "type": "array",
                                    "items": {"description": "a cell value: number, text, true/false, or null for an empty cell"}
                                }
                            }
                        },
                        "required": ["name", "rows"]
                    }
                },
                "charts": {
                    "type": "array",
                    "description": "Optional charts over the sheets above",
                    "items": {
                        "type": "object",
                        "properties": {
                            "sheet": {"type": "string"},
                            "chartType": {"type": "string", "description": "line, column, bar, scatter or pie"},
                            "title": {"type": "string"},
                            "categories": {"type": "string", "description": "A1 range for the category axis, e.g. A2:A12"},
                            "series": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "name": {"type": "string"},
                                        "values": {"type": "string", "description": "A1 range, e.g. B2:B12"}
                                    },
                                    "required": ["values"]
                                }
                            },
                            "anchor": {"type": "string", "description": "Cell for the chart's top-left corner, e.g. D2"}
                        },
                        "required": ["sheet", "chartType", "series"]
                    }
                }
            },
            "required": ["filePath", "sheets"]
        })
    }

    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let p: Params = parse_input(input)?;
        let path = resolve_path(ctx, &p.file_path);
        // T60 P2b: this tool wrote a workbook to summary.pdf in three eval
        // runs across two binaries, and the model reported a PDF. Only a
        // .xlsx name is a workbook; anything else is refused before the
        // guard and before anything exists, with the tool that does write
        // the document named.
        let is_xlsx = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"));
        if !is_xlsx {
            return Err(ToolError::InvalidInput(
                "The spreadsheet tool writes .xlsx workbooks only. For a .docx or .pdf, call write with the document path and Markdown content.".to_string(),
            ));
        }
        // T18, in the same order the write tool uses it: before anything is
        // created, so nothing lands under a secrets directory.
        ctx.guard.check(&path)?;
        let existed = path.exists();
        // T19 read-first: overwriting a workbook this session has not read
        // is the same destruction it is for any other file.
        if existed && !ctx.was_read(&path) {
            return Err(ToolError::failed(format!(
                "{} exists but has not been read in this session. Read it first, or use edit for targeted changes.",
                path.display()
            )));
        }

        let sheets: Vec<SheetSpec> = p
            .sheets
            .iter()
            .map(|s| SheetSpec {
                name: s.name.clone(),
                rows: s.rows.iter().map(|r| r.iter().map(cell).collect()).collect(),
            })
            .collect();

        let mut charts: Vec<ChartSpec> = Vec::new();
        for c in &p.charts {
            let categories = match &c.categories {
                Some(r) => Some(range(r, "categories")?),
                None => None,
            };
            let mut series = Vec::new();
            for s in &c.series {
                series.push(SeriesSpec {
                    name: s.name.clone(),
                    values: range(&s.values, "series values")?,
                });
            }
            if series.is_empty() {
                return Err(ToolError::InvalidInput(
                    "a chart needs at least one series".into(),
                ));
            }
            let anchor = match &c.anchor {
                Some(a) => office::parse_a1_cell(a).ok_or_else(|| {
                    ToolError::InvalidInput(format!(
                        "anchor \"{a}\" is not a cell reference like D2"
                    ))
                })?,
                // Clear of a typical data block, and deterministic.
                None => (0, 4),
            };
            charts.push(ChartSpec {
                sheet: c.sheet.clone(),
                kind: c.kind.clone(),
                title: c.title.clone(),
                categories,
                series,
                anchor,
            });
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ToolError::failed(e.to_string()))?;
        }
        // T64 P0a: the existing workbook moves aside first, as in write.
        let previous = if existed {
            let prev = office::previous_copy_path(&path);
            ctx.guard.check(&prev)?;
            office::move_aside(&path, &prev)?;
            Some(prev)
        } else {
            None
        };
        if let Err(e) = office::write_workbook(&path, &sheets, &charts) {
            return Err(match &previous {
                Some(prev) => office::put_back(&path, prev, e),
                None => e,
            });
        }
        ctx.record_read(&path);

        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let sheet_count = sheets.len();
        let chart_count = charts.len();
        let counts = format!(
            "({sheet_count} sheet{}, {chart_count} chart{}, {bytes} bytes)",
            if sheet_count == 1 { "" } else { "s" },
            if chart_count == 1 { "" } else { "s" }
        );
        let output = match &previous {
            Some(prev) => format!(
                "Replaced {} {counts}; the previous document is at {}",
                path.display(),
                prev.display()
            ),
            None => format!("Created {} {counts}", path.display()),
        };
        Ok(ToolOutput { title: p.file_path, output })
    }
}

/// Values only. A JSON number is a number; everything else is the text it
/// prints as, and null is an empty cell.
fn cell(v: &Value) -> SheetCell {
    match v {
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.is_finite() => SheetCell::Number(f),
            _ => SheetCell::Text(n.to_string()),
        },
        Value::String(s) => SheetCell::Text(s.clone()),
        Value::Bool(b) => SheetCell::Text(b.to_string()),
        Value::Null => SheetCell::Empty,
        other => SheetCell::Text(other.to_string()),
    }
}

fn range(s: &str, what: &str) -> Result<(u32, u16, u32, u16), ToolError> {
    office::parse_a1_range(s).ok_or_else(|| {
        ToolError::InvalidInput(format!(
            "{what} \"{s}\" is not an A1 range like A2:A12"
        ))
    })
}
