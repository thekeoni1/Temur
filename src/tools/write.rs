use super::{parse_input, resolve_path, Tool, ToolCtx, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Params {
    #[serde(rename = "filePath")]
    file_path: String,
    content: String,
}

pub struct WriteTool;

impl Tool for WriteTool {
    /// T46: The registry asks before dispatch: this tool has one
    /// question and no second one to compose with.
    fn approval_site(&self) -> crate::tools::ApprovalSite {
        crate::tools::ApprovalSite::Registry
    }

    fn name(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> &'static str {
        include_str!("prompts/write.txt")
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": {"type": "string", "description": "The absolute path to the file to write (must be absolute, not relative)"},
                "content": {"type": "string", "description": "The content to write to the file"}
            },
            "required": ["filePath", "content"]
        })
    }

    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let p: Params = parse_input(input)?;
        let path = resolve_path(ctx, &p.file_path);
        // T18: writes deny too (overwriting a key is destruction and a
        // poisoning vector), and the check runs before create_dir_all so
        // nothing is ever created under a secrets dir.
        ctx.guard.check(&path)?;
        let existed = path.exists();
        // T19 read-first enforcement: the write prompt has always promised
        // this failure; for weak models the promise must be real. New files
        // are unaffected.
        if existed && !ctx.was_read(&path) {
            return Err(ToolError::failed(format!(
                "{} exists but has not been read in this session. Read it first, or use edit for targeted changes.",
                path.display()
            )));
        }
        // T30 (T29 queue finding 6, measured 2026-08-12): a weak model
        // grepped, read three files, then wrote 8 bytes over the 30-byte
        // file holding the answer and reported success. The read-first
        // guard permitted it correctly (the model HAD just read it), so
        // nothing here is a guard defect; what was missing is any trace
        // that content was destroyed. The successful result now says so
        // whenever there was content to lose, with no smallness threshold:
        // deciding which overwrites are worth mentioning is a judgment the
        // tool has no standing to make. u64 because a file length is a file
        // length, not a pointer width.
        let prior_bytes: u64 = if existed {
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ToolError::failed(e.to_string()))?;
        }
        // T54 P2 (D23): a .xlsx path means the content is CSV and the file
        // is a workbook. No new tool, no new parameter, no prompt cost:
        // the model already writes correct CSV today, it just had nowhere
        // to put it. Everything above this line (the guard, read-first,
        // the overwrite accounting) is untouched and applies unchanged.
        // T60: a .docx or .pdf path means the content is Markdown and the
        // file is a document, by the same rule. Same guard, same
        // read-first, same overwrite accounting above.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let as_document = super::office::writes_as_document(&ext);
        // T64 P0a (F10, Ruling T64-3), as sharpened by T65 P1 (review
        // finding 1, Ruling T65-4): an existing document moves aside to
        // <stem>.previous.<ext> before it is replaced, UNLESS it is temur's
        // own last write of that path in this session, which temur writes
        // over directly. The copy therefore holds the last version temur
        // did not write itself: the user's original survives a session of
        // iterating, and a file saved from Excel or Word since temur's
        // write is that user version and moves aside as before. The rule
        // keys on every extension read extracts, not only the three written
        // as documents: plain text over an .ods is the same loss.
        let is_document = super::office::is_document(&ext);
        let own = existed && is_document && ctx.is_own_document_write(&path);
        let previous = if existed && is_document && !own {
            let prev = super::office::previous_copy_path(&path);
            ctx.guard.check(&prev)?;
            super::office::move_aside(&path, &prev)?;
            Some(prev)
        } else {
            None
        };
        // How many characters the PDF's WinAnsi encoding could not hold
        // and wrote as `?`. Reported only when nonzero, so the model can
        // tell the user; every other path leaves it at zero.
        let mut outside_winansi: u64 = 0;
        let written = match ext.as_str() {
            "xlsx" => super::office::write_csv_as_xlsx(&path, &p.content),
            "docx" => super::office::write_markdown_as_docx(&path, &p.content),
            "pdf" => super::office::write_markdown_as_pdf(&path, &p.content).map(|n| outside_winansi = n),
            _ => std::fs::write(&path, &p.content).map_err(|e| ToolError::failed(e.to_string())),
        };
        if let Err(e) = written {
            // T65 P1: the write went straight over temur's own output, so
            // whatever is there now is temur's own too, whether the writer
            // left a partial document (the .docx writer streams into the
            // file) or never touched it. Re-recording it means the retry
            // replaces that partial directly instead of rotating the user's
            // copy away. There is nothing to put back: nothing moved.
            if own {
                ctx.record_document_write(&path);
            }
            return Err(match &previous {
                Some(prev) => super::office::put_back(&path, prev, e),
                None => e,
            });
        }
        // For a plain write those are the same number. For a workbook or a
        // document they are not, and reporting the source's length as the
        // file's size would be a number the model could not reconcile with
        // anything it later sees on disk.
        let written_bytes: u64 = if as_document {
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
        } else {
            p.content.len() as u64
        };
        // A successful write knows the file's content: overwrites of its own
        // output (e.g. iterating on a generated file) need no re-read.
        ctx.record_read(&path);
        // T65 P1: a document is remembered more precisely than that, by the
        // bytes it was left holding, so the next write over the same path
        // can tell temur's own output from the user's document.
        if is_document {
            ctx.record_document_write(&path);
        }
        let replaced = if prior_bytes > 0 {
            format!(", replaced {prior_bytes} bytes of prior content")
        } else {
            String::new()
        };
        let unencodable = if outside_winansi > 0 {
            format!(", {outside_winansi} characters outside WinAnsi replaced")
        } else {
            String::new()
        };
        let output = if let Some(prev) = &previous {
            format!(
                "Replaced {} ({written_bytes} bytes{unencodable}); the previous document is at {}",
                path.display(),
                prev.display()
            )
        } else if own {
            // T65 P1: nothing moved aside, and the copy beside the document
            // still holds the user's version unless the user deleted it.
            // temur's own writes never recreate it.
            let prev = super::office::previous_copy_path(&path);
            if prev.exists() {
                format!(
                    "Replaced {} ({written_bytes} bytes{unencodable}); the original is still at {}",
                    path.display(),
                    prev.display()
                )
            } else {
                format!("Replaced {} ({written_bytes} bytes{unencodable})", path.display())
            }
        } else {
            format!(
                "{} {} ({written_bytes} bytes{replaced}{unencodable})",
                if existed { "Overwrote" } else { "Created" },
                path.display()
            )
        };
        Ok(ToolOutput { title: p.file_path, output })
    }
}
