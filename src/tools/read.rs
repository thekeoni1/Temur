//! Behavioral port of OpenCode's read tool: 1-indexed offset/limit (default
//! 2000 lines), per-line truncation at 2000 chars, 28 KB rendered-output cap,
//! binary detection, directory-listing mode, numbered output.
//!
//! The byte cap counts the *rendered* line (number prefix included) and sits
//! below the registry's central 30,000-char truncation so this tool's own
//! pagination footer ("Use offset=N to continue") always survives intact —
//! otherwise the model loses the continuation hint and re-reads blindly.

use super::{parse_input, resolve_path, Tool, ToolCtx, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, Read};

const DEFAULT_READ_LIMIT: u64 = 2000;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_BYTES: u64 = 28 * 1024;

#[derive(Deserialize)]
struct Params {
    #[serde(rename = "filePath")]
    file_path: String,
    // `default` is required alongside `deserialize_with`: naming a
    // deserializer turns off serde's implicit "missing Option is None"
    // (T33), and an absent offset/limit must keep meaning absent.
    #[serde(default, deserialize_with = "super::coerce::lenient_opt_u64")]
    offset: Option<u64>,
    #[serde(default, deserialize_with = "super::coerce::lenient_opt_u64")]
    limit: Option<u64>,
}

pub struct ReadTool;

impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }
    fn description(&self) -> &'static str {
        include_str!("prompts/read.txt")
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": {"type": "string", "description": "The absolute path to the file or directory to read"},
                "offset": {"type": "number", "description": "The line number to start reading from (1-indexed)"},
                "limit": {"type": "number", "description": "The maximum number of lines to read (defaults to 2000)"}
            },
            "required": ["filePath"]
        })
    }

    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let p: Params = parse_input(input)?;
        let offset = p.offset.unwrap_or(1);
        if offset < 1 {
            return Err(ToolError::InvalidInput(
                "offset must be greater than or equal to 1".into(),
            ));
        }
        let limit = p.limit.unwrap_or(DEFAULT_READ_LIMIT);
        let path = resolve_path(ctx, &p.file_path);
        let title = p.file_path.clone();

        // T18: before ANY open (is_binary opens the file too, and even
        // metadata leaks existence under a secrets dir).
        ctx.guard.check(&path)?;

        let meta = std::fs::metadata(&path)
            .map_err(|_| ToolError::failed(format!("File not found: {}", path.display())))?;

        if meta.is_dir() {
            return read_dir(&path, offset, limit, title);
        }
        if is_binary(&path)? {
            return Err(ToolError::failed(format!(
                "Cannot read binary file: {}. {}",
                path.display(),
                binary_hint(&path)
            )));
        }

        let file = std::fs::File::open(&path).map_err(|e| ToolError::failed(e.to_string()))?;
        let reader = std::io::BufReader::new(file);
        let mut raw: Vec<String> = Vec::new();
        let mut bytes: u64 = 0;
        let mut lines: u64 = 0;
        let mut truncated_by_bytes = false;
        let mut has_more = false;
        for line in reader.split(b'\n') {
            let line = line.map_err(|e| ToolError::failed(e.to_string()))?;
            lines += 1;
            if lines < offset {
                continue;
            }
            if raw.len() as u64 >= limit {
                has_more = true;
                continue;
            }
            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end_matches('\r');
            let line_out = if text.chars().count() > MAX_LINE_LENGTH {
                let cut: String = text.chars().take(MAX_LINE_LENGTH).collect();
                format!("{cut}... (line truncated to {MAX_LINE_LENGTH} chars)")
            } else {
                text.to_string()
            };
            // Count the line as rendered: "N: " prefix + text + newline, so
            // MAX_BYTES bounds the actual output body (see module docs).
            let lineno = offset + raw.len() as u64;
            let size = lineno.to_string().len() as u64 + 2 + line_out.len() as u64 + 1;
            if bytes + size > MAX_BYTES {
                truncated_by_bytes = true;
                has_more = true;
                break;
            }
            bytes += size;
            raw.push(line_out);
        }

        if lines < offset && !(lines == 0 && offset == 1) {
            return Err(ToolError::failed(format!(
                "Offset {offset} is out of range for this file ({lines} lines)"
            )));
        }

        let mut output = format!("<path>{}</path>\n<type>file</type>\n<content>\n", path.display());
        for (i, line) in raw.iter().enumerate() {
            output.push_str(&format!("{}: {line}\n", i as u64 + offset));
        }
        let last = offset + raw.len() as u64 - 1;
        if truncated_by_bytes {
            output.push_str(&format!(
                "\n(Output capped at {} KB. Showing lines {offset}-{last}. Use offset={} to continue.)\n",
                MAX_BYTES / 1024,
                last + 1
            ));
        } else if has_more {
            output.push_str(&format!(
                "\n(Showing lines {offset}-{last} of {lines}. Use offset={} to continue.)\n",
                last + 1
            ));
        } else {
            output.push_str(&format!("\n(End of file - total {lines} lines)\n"));
        }
        output.push_str("</content>");
        // T19: a successful file read arms write's read-first check.
        ctx.record_read(&path);
        Ok(ToolOutput { title, output })
    }
}

fn read_dir(path: &std::path::Path, offset: u64, limit: u64, title: String) -> Result<ToolOutput, ToolError> {
    let mut entries: Vec<String> = std::fs::read_dir(path)
        .map_err(|e| ToolError::failed(e.to_string()))?
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    entries.sort();
    let total = entries.len();
    let start = (offset - 1) as usize;
    let sliced: Vec<String> = entries.into_iter().skip(start).take(limit as usize).collect();
    let shown = sliced.len();
    let note = if start + shown < total {
        format!(
            "\n(Showing {shown} of {total} entries. Use 'offset' parameter to read beyond entry {})",
            offset + shown as u64
        )
    } else {
        format!("\n({total} entries)")
    };
    Ok(ToolOutput {
        title,
        output: format!(
            "<path>{}</path>\n<type>directory</type>\n<entries>\n{}\n{note}\n</entries>",
            path.display(),
            sliced.join("\n")
        ),
    })
}

/// What to do INSTEAD, by file type. T31 (D3, operator dogfood
/// 2026-08-14): the binary refusal worked live (qwen3-4b stopped trying to
/// read a PDF as text), but the generic hint sent it toward `unzip -l` on a
/// PDF. A remedy the model can act on is worth a table this small. Unknown
/// types keep the generic sentence, byte-identical to the pre-T31 message.
fn binary_hint(path: &std::path::Path) -> &'static str {
    const GENERIC: &str = "Inspect it with bash instead (e.g. file, unzip -l, strings).";
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return GENERIC;
    };
    match ext.to_ascii_lowercase().as_str() {
        "pdf" => {
            "Its text is compressed, so convert it with bash first (e.g. pdftotext file.pdf -) \
             or ask the user for the text."
        }
        "zip" | "jar" | "war" => "List its contents with bash (e.g. unzip -l).",
        "gz" => "Decompress it with bash (e.g. zcat, or gunzip -c for a copy).",
        "tar" => "List its contents with bash (e.g. tar -tf).",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => {
            "It is image content and temur cannot see images, so ask the user to describe it."
        }
        _ => GENERIC,
    }
}

fn is_binary(path: &std::path::Path) -> Result<bool, ToolError> {
    const BINARY_EXTS: &[&str] = &[
        "zip", "tar", "gz", "exe", "dll", "so", "class", "jar", "war", "7z", "doc", "docx",
        "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "bin", "dat", "obj", "o", "a",
        "lib", "wasm", "pyc", "pyo",
    ];
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if BINARY_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
            return Ok(true);
        }
    }
    let mut file = std::fs::File::open(path).map_err(|e| ToolError::failed(e.to_string()))?;
    let mut buf = [0u8; 4096];
    let n = file.read(&mut buf).map_err(|e| ToolError::failed(e.to_string()))?;
    if n == 0 {
        return Ok(false);
    }
    let mut non_printable = 0usize;
    for &b in &buf[..n] {
        if b == 0 {
            return Ok(true);
        }
        if b < 9 || (b > 13 && b < 32) {
            non_printable += 1;
        }
    }
    Ok(non_printable * 10 > n * 3) // >30% non-printable
}
