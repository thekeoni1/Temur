use super::{parse_input, Tool, ToolCtx, ToolError, ToolOutput};
use crate::skills;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

/// How many sections a self-healing error message lists before folding the
/// rest into a count. An error that dumps 200 headings is its own problem.
const ERROR_LIST_CAP: usize = 30;

#[derive(Deserialize)]
struct SkillParams {
    name: String,
    /// T28: which section to return. Absent means the whole skill (today's
    /// behavior). Deliberately a raw `Value`: models write `"section": 2` as
    /// often as `"section": "2"`, and refusing one of those spellings would
    /// be a schema argument with the model in the middle of a task.
    #[serde(default)]
    section: Option<Value>,
}

/// Loads a named skill's instructions into context. Holds the resolved skill
/// search dirs (fixed at startup), so it needs nothing from the session ctx.
pub struct SkillTool {
    dirs: Vec<PathBuf>,
}

impl SkillTool {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        SkillTool { dirs }
    }
}

/// Normalize a heading or query for matching: drop any leading `#` run, fold
/// case, and remove ALL whitespace. Forgiving on purpose, since the model is
/// copying a title out of an index it was just shown and may or may not
/// bring the hashes along.
fn match_key(s: &str) -> String {
    s.trim_start_matches('#')
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// The `section` argument as a string, whatever JSON shape it arrived in.
fn section_ref(v: &Value) -> Result<String, ToolError> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        other => Err(ToolError::InvalidInput(format!(
            "skill: 'section' must be a section number or heading text, got {other}"
        ))),
    }
}

/// The `Base directory for this skill: <path>` line, or nothing at all.
///
/// T30 (T29 queue finding 8, measured 2026-08-12): that line was
/// unconditional, and two of the three models observed against an over-cap
/// skill were pulled off the index by it. Qwen3-4B went to grep the
/// directory instead of asking for a section and gave up; Qwen3-1.7B
/// answered correctly from section 5 and then wrote its answer INTO that
/// directory rather than the cwd. The line is genuinely useful for a skill
/// that ships playbooks and assets, so it is kept where it points at
/// something and dropped where it does not: a skill that is only a
/// SKILL.md now names no path, and nothing invites a filesystem detour.
///
/// One `read_dir` at render time, like everything else here recomputed per
/// call rather than cached. A directory that cannot be listed is treated as
/// having nothing besides the skill file: the line's whole justification is
/// assets we can see.
fn base_dir_line(dir: &std::path::Path) -> String {
    let has_more = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .any(|e| e.file_name() != std::ffi::OsStr::new(skills::SKILL_FILE)),
        Err(_) => false,
    };
    if has_more {
        format!("Base directory for this skill: {}\n", dir.display())
    } else {
        String::new()
    }
}

/// The header block every mode opens with: the base-directory line when it
/// earns its place, then `note` (already newline-terminated, or empty),
/// then the blank line separating the header from the body. Empty when
/// there is nothing to say, so a bare skill's wrapper sits directly against
/// its content.
fn header(dir: &std::path::Path, note: &str) -> String {
    let mut h = base_dir_line(dir);
    h.push_str(note);
    if !h.is_empty() {
        h.push('\n');
    }
    h
}

fn list_sections(body: &str, sections: &[skills::Section]) -> String {
    let lines = skills::render_index_lines(body, sections);
    let shown = lines.len().min(ERROR_LIST_CAP);
    let mut s = lines[..shown].join("\n");
    if lines.len() > shown {
        s.push_str(&format!("\n... and {} more", lines.len() - shown));
    }
    s
}

impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "skill"
    }
    fn description(&self) -> &'static str {
        include_str!("prompts/skill.txt")
    }
    fn description_compact(&self) -> &'static str {
        include_str!("prompts/compact/skill.txt")
    }
    /// T28: the default advice (grep, head/tail) is nonsense here. A skill is
    /// one document the model asked for by name, and the way to see the rest
    /// of it is the section index, not a shell pipeline.
    fn truncation_hint(&self) -> &'static str {
        "call skill again with a \"section\" argument to read one part of this skill in full"
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the skill to load (its directory name), exactly as listed in <available_skills>."
                },
                // T34 interop (2026-08-17/18): this was `["string", "number"]`,
                // and that union type broke real servers. SkillTool is
                // registered unconditionally (mod.rs `standard_with_skills`,
                // sole call site main.rs), so the union rode in EVERY tools
                // array temur sends. llama.cpp renders chat templates on every
                // request when no specialized handler matches, and the shipped
                // Hermes-2-Pro template's `json_to_python_type()` macro opens
                // with a dict lookup keyed on the schema's "type"; a list key
                // is unhashable, so the render throws and the server answers
                // HTTP 400 before the model sees anything. Stock Jinja2 raises
                // the same. Evidence:
                // ~/temur-eval-archive/template-experiment-2026-08-17/
                // E2/a1-hermes-root-cause.txt.
                //
                // The declared type is now plainly "string". Nothing about the
                // execute path changed: `SkillParams::section` is still a raw
                // Value and `section_ref` still accepts a JSON number, because
                // tolerance at the ARGUMENT boundary is the contract for
                // non-string spellings (T33), not a union in the schema. The
                // schema is what a template has to render; coercion is what we
                // owe the model.
                "section": {
                    "type": "string",
                    "description": "Optional. One section to return instead of the whole skill: either its number from a <skill_index> listing, or its heading text. Omit this to load the skill."
                }
            },
            "required": ["name"]
        })
    }

    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let p: SkillParams = parse_input(input)?;
        let name = p.name.trim();
        if name.is_empty() {
            return Err(ToolError::InvalidInput("skill: 'name' is empty".into()));
        }
        // A skill name is a single directory component: reject any path
        // separators or parent refs so the model can't escape the skill dirs.
        // `section` needs no such guard and gets none: it is matched against
        // the scanned heading list and never reaches the filesystem.
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(ToolError::InvalidInput(format!(
                "skill: invalid name '{name}' (must be a bare skill name, no path)"
            )));
        }
        let (dir, raw) = skills::load(&self.dirs, name).ok_or_else(|| {
            ToolError::failed(format!("skill '{name}' not found in any skill directory"))
        })?;
        // Recomputed from the file's current bytes on every call: an index
        // handed out earlier cannot describe a file that has since changed,
        // because nothing from that call was kept.
        let body = skills::minify(&raw);
        let sections = skills::scan_sections(&body);
        let title = format!("skill: {name}");

        if let Some(want) = &p.section {
            let want = section_ref(want)?;
            return self.one_section(name, &dir, &body, &sections, &want, title);
        }

        let full = format!(
            "<skill_content name=\"{name}\">\n{}{body}\n</skill_content>",
            header(&dir, "")
        );
        // At or under the cap: today's behavior exactly, minified. The
        // comparison is over the WRAPPED output because that is what the cap
        // is applied to, so "fits" here means the same thing it means there.
        if full.chars().count() <= ctx.output_cap || sections.is_empty() {
            // A skill with no headings cannot be indexed. It falls through to
            // the central truncation, which now at least advises usefully.
            return Ok(ToolOutput {
                title,
                output: full,
            });
        }
        let index = self.index(name, &dir, &body, &sections, ctx.output_cap);
        // An index that does not itself fit is not an improvement: that skill
        // has more prose before its first heading than the session can carry,
        // so it behaves like the heading-less case above.
        if index.chars().count() > ctx.output_cap {
            return Ok(ToolOutput {
                title,
                output: full,
            });
        }
        Ok(ToolOutput {
            title,
            output: index,
        })
    }
}

impl SkillTool {
    /// The over-cap answer: what this skill contains, and how to ask for any
    /// part of it. Never a summary, and never a network or model call.
    fn index(
        &self,
        name: &str,
        dir: &std::path::Path,
        body: &str,
        sections: &[skills::Section],
        cap: usize,
    ) -> String {
        let mut s = format!("<skill_index name=\"{name}\">\n{}", header(dir, ""));
        s.push_str(&format!(
            "This skill is {} chars, over this session's {cap}-char tool output limit, so it is returned as a section index instead of being cut off in the middle. Nothing is summarized and nothing is omitted: every section listed below is available in full. Fetch one with {{\"name\": \"{name}\", \"section\": \"<number or heading>\"}}, using either the number or the heading text.\n\n",
            body.chars().count()
        ));
        let intro = skills::intro(body, sections).trim();
        if !intro.is_empty() {
            s.push_str(intro);
            s.push_str("\n\n");
        }
        s.push_str("Sections:\n");
        s.push_str(&skills::render_index_lines(body, sections).join("\n"));
        s.push_str("\n</skill_index>");
        s
    }

    /// One section, by number or by heading text.
    fn one_section(
        &self,
        name: &str,
        dir: &std::path::Path,
        body: &str,
        sections: &[skills::Section],
        want: &str,
        title: String,
    ) -> Result<ToolOutput, ToolError> {
        if sections.is_empty() {
            return Err(ToolError::failed(format!(
                "skill '{name}' has no headings to select from, so it has no sections. Call skill with {{\"name\": \"{name}\"}} and no 'section' to load it."
            )));
        }
        let want = want.trim();
        let by_number = want
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=sections.len()).contains(n))
            .map(|n| n - 1);
        let key = match_key(want);
        let matches: Vec<usize> = (0..sections.len())
            .filter(|&i| match_key(&sections[i].title) == key)
            .collect();
        let idx = match by_number.or_else(|| matches.first().copied()) {
            Some(i) => i,
            None => {
                return Err(ToolError::failed(format!(
                    "skill '{name}' has no section '{want}'. Its sections are:\n{}\nFetch one with {{\"name\": \"{name}\", \"section\": \"<number or heading>\"}}.",
                    list_sections(body, sections)
                )))
            }
        };
        let sec = &sections[idx];
        // Duplicate headings are legitimate (every "## Options" under a
        // different command). First wins, and the model is told how to reach
        // the others rather than left guessing which one it got.
        let dup = if by_number.is_none() && matches.len() > 1 {
            format!(
                "Note: {} sections share this heading; this is the first (number {}). Use a section number to select another: {}.\n",
                matches.len(),
                idx + 1,
                matches
                    .iter()
                    .map(|i| (i + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            String::new()
        };
        let text = sec.text(body);
        let sep = if text.ends_with('\n') { "" } else { "\n" };
        Ok(ToolOutput {
            title,
            output: format!(
                "<skill_section name=\"{name}\" number=\"{}\" title=\"{}\">\n{}{text}{sep}</skill_section>",
                idx + 1,
                sec.title,
                header(dir, &dup)
            ),
        })
    }
}
