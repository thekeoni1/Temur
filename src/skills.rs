//! Skill resolution + enumeration.
//!
//! A "skill" is an instruction file the model loads on demand via the `skill`
//! tool: `<skill-dir>/<name>/SKILL.md`, with optional `playbooks/` and assets
//! beside it. This module resolves the effective skill-directory search list,
//! enumerates installed skills (parsing minimal YAML frontmatter for `name`
//! and `description`), and renders the `<available_skills>` block advertised in
//! the system prompt.
//!
//! No YAML library — the minimal frontmatter parser keeps this a small binary.

use std::path::{Path, PathBuf};

/// Directory searched under the workspace cwd and under `$HOME`. Matches the
/// layout an external CLI skill ships in, so real skills drop in unmodified.
const SKILLS_SUBDIR: &str = ".temur/skills";
/// Pre-rename layout, still searched (after the primary) for one release.
const LEGACY_SKILLS_SUBDIR: &str = ".opencode/skills";

/// Metadata for one installed skill.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// The skill's own directory: `<skill-dir>/<name>`.
    pub dir: PathBuf,
}

fn add_unique(dirs: &mut Vec<PathBuf>, p: PathBuf) {
    if !dirs.iter().any(|d| d == &p) {
        dirs.push(p);
    }
}

/// Resolve the effective skill-directory search list.
///
/// Order: explicit `override_list` entries (`:`-separated, blanks skipped) in
/// the order given, then `<cwd>/.temur/skills` and its legacy fallback
/// `<cwd>/.opencode/skills`, then the same pair under `<home>` — each appended
/// only if not already present (dedup, first occurrence wins). The defaults
/// are always searched.
pub fn skill_dirs(override_list: Option<&str>, cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(list) = override_list {
        for entry in list.split(':') {
            if !entry.is_empty() {
                add_unique(&mut dirs, PathBuf::from(entry));
            }
        }
    }
    add_unique(&mut dirs, cwd.join(SKILLS_SUBDIR));
    add_unique(&mut dirs, cwd.join(LEGACY_SKILLS_SUBDIR));
    if let Some(h) = home {
        add_unique(&mut dirs, h.join(SKILLS_SUBDIR));
        add_unique(&mut dirs, h.join(LEGACY_SKILLS_SUBDIR));
    }
    dirs
}

/// Parse minimal YAML frontmatter. Returns `(name, description)` — either may
/// be empty when absent. Returns `None` only when a frontmatter block is
/// opened (leading `---`) but never closed (treated as malformed).
pub fn parse_frontmatter(md: &str) -> Option<(String, String)> {
    let rest = match md
        .strip_prefix("---\n")
        .or_else(|| md.strip_prefix("---\r\n"))
    {
        Some(r) => r,
        None => return Some((String::new(), String::new())), // no frontmatter block
    };
    let mut name = String::new();
    let mut description = String::new();
    let mut closed = false;
    for line in rest.lines() {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            closed = true;
            break;
        }
        if let Some(v) = trimmed.strip_prefix("name:") {
            name = unquote(v);
        } else if let Some(v) = trimmed.strip_prefix("description:") {
            description = unquote(v);
        }
    }
    if !closed {
        return None; // unterminated frontmatter — malformed
    }
    Some((name, description))
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"')
            || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// The one file that makes a directory a skill.
pub const SKILL_FILE: &str = "SKILL.md";

/// Enumerate installed skills across `dirs`. For each `<dir>/<name>/SKILL.md`,
/// parse `name`/`description` from frontmatter (falling back to the directory
/// name when `name:` is absent). Dedup by name, first-dir-wins (same
/// precedence as loading). Malformed SKILL.md files are skipped with a stderr
/// warning; enumeration never fails.
pub fn enumerate(dirs: &[PathBuf]) -> Vec<SkillInfo> {
    let mut out: Vec<SkillInfo> = Vec::new();
    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue, // dir absent — normal
        };
        let mut subdirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        subdirs.sort(); // deterministic order within a directory
        for sub in subdirs {
            let md = sub.join(SKILL_FILE);
            let content = match std::fs::read_to_string(&md) {
                Ok(c) => c,
                Err(_) => continue, // no SKILL.md here — not a skill dir
            };
            let dirname = sub
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let (name, description) = match parse_frontmatter(&content) {
                Some(fm) => fm,
                None => {
                    eprintln!(
                        "temur: skipping malformed skill (unterminated frontmatter): {}",
                        md.display()
                    );
                    continue;
                }
            };
            let name = if name.is_empty() { dirname } else { name };
            if name.is_empty() {
                continue;
            }
            if out.iter().any(|s| s.name == name) {
                continue; // first-dir-wins
            }
            out.push(SkillInfo {
                name,
                description,
                dir: sub,
            });
        }
    }
    out
}

/// Load a skill's SKILL.md by name. Returns `(skill_dir, content)` for the
/// first matching `<dir>/<name>/SKILL.md`. `None` if not found in any dir.
pub fn load(dirs: &[PathBuf], name: &str) -> Option<(PathBuf, String)> {
    for dir in dirs {
        let sub = dir.join(name);
        let md = sub.join(SKILL_FILE);
        if let Ok(content) = std::fs::read_to_string(&md) {
            return Some((sub, content));
        }
    }
    None
}

// ------------------------------------------------- T28: minify + section scan
//
// Everything below is a PURE function of the SKILL.md bytes, recomputed on
// every call. Nothing is cached, persisted, or carried in session state, so
// an index can never be stale: editing a skill file changes the next call's
// answer by construction, and there is no invalidation machinery to get
// wrong. That is the whole reason this layer is shaped as functions rather
// than as a store.

/// Split a leading YAML frontmatter block off `md`.
///
/// Returns `(block, rest)` where `block` includes both `---` delimiters and
/// the newline after the closing one, so `block` + `rest` is the input
/// byte-for-byte. An absent or UNTERMINATED block yields `(None, md)`: a
/// malformed block is body text here, never silently swallowed.
fn split_frontmatter(md: &str) -> (Option<&str>, &str) {
    let after_open = match md.strip_prefix("---\n") {
        Some(r) => r,
        None => match md.strip_prefix("---\r\n") {
            Some(r) => r,
            None => return (None, md),
        },
    };
    let open_len = md.len() - after_open.len();
    let mut off = open_len;
    for line in after_open.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        off += line.len();
        if content.trim_end() == "---" {
            return (Some(&md[..off]), &md[off..]);
        }
    }
    (None, md) // never closed: malformed, so not a frontmatter block
}

/// Whether a frontmatter block carries nothing the model still needs.
///
/// `name:` and `description:` are ALREADY relayed to the model in the
/// `<available_skills>` system-prompt block, so repeating them inside the
/// tool result is pure duplication. Any other key (allowed-tools, version,
/// license, a nested block) might matter and is nobody's to judge here, so
/// one unrecognized non-blank line keeps the whole block verbatim.
fn frontmatter_is_redundant(block: &str) -> bool {
    let mut inner = block.lines();
    inner.next(); // the opening ---
    for line in inner {
        let t = line.trim_end();
        if t == "---" {
            break; // the closing delimiter
        }
        if t.trim().is_empty() {
            continue;
        }
        if !(t.starts_with("name:") || t.starts_with("description:")) {
            return false;
        }
    }
    true
}

/// A fenced-code opener: the line's first non-space run is at least three
/// backticks or tildes. Returns the fence character and its length.
fn opening_fence(line: &str) -> Option<(char, usize)> {
    let s = line.trim_start_matches(' ');
    let c = s.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let n = s.chars().take_while(|&x| x == c).count();
    (n >= 3).then_some((c, n))
}

/// A closer for the open fence: same character, at least as long, and
/// nothing but whitespace after the run.
fn is_closing_fence(line: &str, fence_char: char, fence_len: usize) -> bool {
    let s = line.trim_start_matches(' ');
    let n = s.chars().take_while(|&x| x == fence_char).count();
    n >= fence_len && s[n..].trim().is_empty()
}

/// An ATX heading: up to three leading spaces, one to six `#`, then a space
/// or end of line. Four spaces of indent is code, not a heading. Setext
/// headings (the `===` / `---` underline form) are deliberately NOT indexed:
/// `---` is ambiguous with a frontmatter delimiter and a thematic break, and
/// guessing wrong would slice a skill in the wrong place.
fn atx_heading(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let s = &line[indent..];
    let level = s.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &s[level..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((level, rest.trim().to_string()))
}

/// One ATX-headed section of a markdown document.
///
/// `start`/`end` are BYTE offsets into the exact string the scan ran over,
/// so [`Section::text`] must be given that same string.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    /// 1 for `#`, 6 for `######`.
    pub level: usize,
    /// Heading text with the `#` run and surrounding whitespace removed.
    pub title: String,
    /// Byte offset of the heading line's first byte.
    pub start: usize,
    /// Byte offset one past the section's last byte.
    pub end: usize,
}

impl Section {
    /// This section's full text, heading line included.
    pub fn text<'a>(&self, md: &'a str) -> &'a str {
        &md[self.start..self.end]
    }
}

/// Index every ATX heading in `md`, fence-aware.
///
/// A heading inside a fenced code block is never indexed (a shell transcript
/// full of `# comments` would otherwise shred the document), and an unclosed
/// fence swallows the rest of the file by design: that is what a reader sees
/// too. Extents are HIERARCHICAL, running from the heading to the next
/// heading of the same or a shorter level, so fetching a section brings its
/// subsections with it rather than a fragment ending mid-thought.
pub fn scan_sections(md: &str) -> Vec<Section> {
    let mut heads: Vec<(usize, usize, String)> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut off = 0usize;
    for line in md.split_inclusive('\n') {
        let start = off;
        off += line.len();
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        if let Some((fc, flen)) = fence {
            if is_closing_fence(content, fc, flen) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = opening_fence(content) {
            fence = Some(open);
            continue;
        }
        if let Some((level, title)) = atx_heading(content) {
            heads.push((start, level, title));
        }
    }
    let mut out = Vec::with_capacity(heads.len());
    for (i, (start, level, title)) in heads.iter().enumerate() {
        let end = heads[i + 1..]
            .iter()
            .find(|(_, l, _)| l <= level)
            .map(|(s, _, _)| *s)
            .unwrap_or(md.len());
        out.push(Section {
            level: *level,
            title: title.clone(),
            start: *start,
            end,
        });
    }
    out
}

/// The text before the first heading: frontmatter that survived minification,
/// a title paragraph, whatever the author put up top. The whole document when
/// it has no headings at all.
pub fn intro<'a>(md: &'a str, sections: &[Section]) -> &'a str {
    match sections.first() {
        Some(s) => &md[..s.start],
        None => md,
    }
}

/// The sections no other section contains. Their extents tile the document
/// after the intro exactly, which is what makes the reconstruction invariant
/// hold: intro + top-level texts == the whole input, byte for byte.
pub fn top_level(sections: &[Section]) -> Vec<&Section> {
    let mut out = Vec::new();
    let mut covered_to = 0usize;
    for s in sections {
        if s.start >= covered_to {
            out.push(s);
            covered_to = s.end;
        }
    }
    out
}

/// One numbered line per section for a skill index: the number the model
/// fetches by, the `#` run as the level mark, the title, and the section's
/// size so it can tell a one-liner from a chapter before asking for it.
pub fn render_index_lines(md: &str, sections: &[Section]) -> Vec<String> {
    sections
        .iter()
        .enumerate()
        .map(|(i, s)| {
            format!(
                "{}. {} {} ({} chars)",
                i + 1,
                "#".repeat(s.level),
                s.title,
                s.text(md).chars().count()
            )
        })
        .collect()
}

/// Shrink a SKILL.md losslessly for model consumption.
///
/// Three reductions, none of which can change what the document SAYS: a
/// frontmatter block holding only `name:`/`description:` is dropped (the
/// model already has both from `<available_skills>`), trailing whitespace
/// goes, and runs of blank lines collapse to one. Leading and trailing blank
/// lines go entirely. Fenced code is copied byte-for-byte, because
/// whitespace is semantic in a heredoc, a diff, or Python.
///
/// Guarantees, both pinned by tests: the result is never longer than the
/// input, and minifying twice changes nothing. Honest about scale: on real
/// skill files this saves single-digit percent. The section index, not this,
/// is what makes a large skill fit.
pub fn minify(md: &str) -> String {
    let (front, body) = split_frontmatter(md);
    let keep_front = front.filter(|f| !frontmatter_is_redundant(f));
    let ends_nl = body.ends_with('\n');
    let mut src: Vec<&str> = body.split('\n').collect();
    if ends_nl {
        src.pop(); // the empty element after the final newline
    }
    let mut out: Vec<&str> = Vec::with_capacity(src.len());
    let mut fence: Option<(char, usize)> = None;
    for line in src {
        if let Some((fc, flen)) = fence {
            out.push(line); // inside a fence: byte-identical, no trimming
            let c = line.strip_suffix('\r').unwrap_or(line);
            if is_closing_fence(c, fc, flen) {
                fence = None;
            }
            continue;
        }
        let trimmed = line.trim_end();
        if let Some(open) = opening_fence(trimmed) {
            fence = Some(open);
            out.push(trimmed);
            continue;
        }
        if trimmed.is_empty() {
            // Collapse a blank run to one, and never open with blanks.
            if out.is_empty() || matches!(out.last(), Some(&"")) {
                continue;
            }
            out.push("");
            continue;
        }
        out.push(trimmed);
    }
    while matches!(out.last(), Some(&"")) {
        out.pop();
    }
    let mut s = String::with_capacity(md.len());
    if let Some(f) = keep_front {
        s.push_str(f); // kept blocks are verbatim: not ours to reformat
    }
    let joined = out.join("\n");
    s.push_str(&joined);
    if ends_nl && !joined.is_empty() {
        s.push('\n');
    }
    s
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Render the `<available_skills>` block for the system prompt. Returns `None`
/// when no skills are installed (nothing to advertise). Shape mirrors the
/// captured OpenCode prompt so behavior stays familiar to the model.
pub fn system_prompt_section(skills: &[SkillInfo]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut s = String::new();
    s.push_str("\n\nSkills provide specialized instructions and workflows for specific tasks.\n");
    s.push_str("Use the skill tool to load a skill when a task matches its description.\n");
    s.push_str("<available_skills>\n");
    for sk in skills {
        s.push_str("  <skill>\n");
        s.push_str(&format!("    <name>{}</name>\n", xml_escape(&sk.name)));
        s.push_str(&format!(
            "    <description>{}</description>\n",
            xml_escape(&sk.description)
        ));
        s.push_str(&format!("    <location>file://{}</location>\n", sk.dir.display()));
        s.push_str("  </skill>\n");
    }
    s.push_str("</available_skills>");
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_dedups_and_orders() {
        let cwd = Path::new("/work");
        let home = Path::new("/home/dev");
        let dirs = skill_dirs(Some("/a::/b:/work/.temur/skills"), cwd, Some(home));
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/work/.temur/skills"), // override entry == cwd default: kept once
                PathBuf::from("/work/.opencode/skills"),
                PathBuf::from("/home/dev/.temur/skills"),
                PathBuf::from("/home/dev/.opencode/skills"),
            ]
        );
    }

    #[test]
    fn resolution_defaults_only() {
        let dirs = skill_dirs(None, Path::new("/w"), None);
        // Primary layout first, legacy pre-rename fallback second.
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/w/.temur/skills"),
                PathBuf::from("/w/.opencode/skills"),
            ]
        );
    }

    #[test]
    fn frontmatter_parses_quoted_and_bare() {
        let (n, d) = parse_frontmatter("---\nname: \"foo\"\ndescription: bar baz\n---\nbody").unwrap();
        assert_eq!(n, "foo");
        assert_eq!(d, "bar baz");
    }

    #[test]
    fn frontmatter_absent_is_empty_not_error() {
        let (n, d) = parse_frontmatter("# just markdown\n").unwrap();
        assert!(n.is_empty() && d.is_empty());
    }

    #[test]
    fn frontmatter_unterminated_is_malformed() {
        assert!(parse_frontmatter("---\nname: foo\nno closing fence\n").is_none());
    }

    #[test]
    fn section_none_when_empty() {
        assert!(system_prompt_section(&[]).is_none());
    }

    // ------------------------------------------------------ T28: minify

    #[test]
    fn minify_drops_a_name_description_only_frontmatter() {
        let md = "---\nname: demo\ndescription: does a thing\n---\n\n# Title\n\nbody\n";
        assert_eq!(minify(md), "# Title\n\nbody\n");
    }

    #[test]
    fn minify_keeps_a_frontmatter_carrying_anything_else_verbatim() {
        // One unrecognized key and the whole block survives untouched,
        // including the padding this function would otherwise collapse.
        let md = "---\nname: demo\nallowed-tools:   bash\n\ndescription: d\n---\n# T\n\n\nbody\n";
        let got = minify(md);
        assert!(
            got.starts_with("---\nname: demo\nallowed-tools:   bash\n\ndescription: d\n---\n"),
            "{got:?}"
        );
        assert!(got.ends_with("# T\n\nbody\n"), "{got:?}");
    }

    #[test]
    fn minify_collapses_blanks_and_trailing_space_outside_fences_only() {
        let md = "# T   \n\n\n\ntext  \t\n\n```sh\nx=1   \n\n\n\ny=2\n```\n\n\nend   \n";
        assert_eq!(
            minify(md),
            // Outside: trailing whitespace gone, blank runs to one.
            // Inside the fence: every byte survives, blank run included.
            "# T\n\ntext\n\n```sh\nx=1   \n\n\n\ny=2\n```\n\nend\n"
        );
    }

    #[test]
    fn minify_is_idempotent_and_never_grows_any_input() {
        for md in CORPUS {
            let once = minify(md);
            assert!(
                once.len() <= md.len(),
                "grew: {} -> {} on {md:?}",
                md.len(),
                once.len()
            );
            assert_eq!(minify(&once), once, "not idempotent on {md:?}");
        }
    }

    /// Inputs the pure layer is asserted over as a set: empty, no trailing
    /// newline, CRLF, tilde fences, unclosed fences, headings in code,
    /// frontmatter of both kinds, and a heading-free document.
    const CORPUS: &[&str] = &[
        "",
        "\n",
        "no trailing newline",
        "# H\n\n\n\nbody\n",
        "---\nname: a\ndescription: b\n---\n# H\nbody\n",
        "---\nname: a\nversion: 2\n---\n# H\nbody\n",
        "---\nname: a\nunterminated\n# H\nbody\n",
        "# H\r\n\r\nbody   \r\n",
        "```\n# not a heading\n```\n# real\nx\n",
        "~~~\n# not a heading\n~~~\n# real\nx\n",
        "# a\n```\nunclosed\n# swallowed\n",
        "intro only, no headings at all\n",
        "# only a heading\n",
        "#### deep\n## shallower\n### deeper\n",
        "    # four spaces is code\n   # three is a heading\n",
    ];

    // ----------------------------------------------- T28: section scanner

    fn titles(md: &str) -> Vec<(usize, String)> {
        scan_sections(md)
            .into_iter()
            .map(|s| (s.level, s.title))
            .collect()
    }

    #[test]
    fn scanner_reads_all_six_levels() {
        let md = "# a\n## b\n### c\n#### d\n##### e\n###### f\n####### g\n";
        assert_eq!(
            titles(md),
            vec![
                (1, "a".into()),
                (2, "b".into()),
                (3, "c".into()),
                (4, "d".into()),
                (5, "e".into()),
                (6, "f".into()),
            ],
            "seven hashes is not a heading"
        );
    }

    #[test]
    fn scanner_indent_rule_is_three_spaces() {
        assert_eq!(titles("   # yes\n"), vec![(1, "yes".into())]);
        assert!(titles("    # no\n").is_empty());
    }

    #[test]
    fn scanner_needs_a_space_or_end_of_line_after_the_hashes() {
        assert!(titles("#nospace\n").is_empty());
        assert_eq!(titles("#\n"), vec![(1, String::new())], "bare # is a heading");
        assert_eq!(titles("# t\n"), vec![(1, "t".into())]);
    }

    #[test]
    fn scanner_ignores_headings_inside_backtick_and_tilde_fences() {
        let md = "# real\n```sh\n# comment\n```\n~~~\n# also a comment\n~~~\n## real two\n";
        assert_eq!(titles(md), vec![(1, "real".into()), (2, "real two".into())]);
    }

    #[test]
    fn scanner_closing_fence_must_match_char_and_length() {
        // A ``` inside a ```` block does not close it, so the heading
        // between them stays code.
        let md = "````\n```\n# still code\n````\n# real\n";
        assert_eq!(titles(md), vec![(1, "real".into())]);
        // Tildes never close backticks.
        let md2 = "```\n~~~\n# still code\n```\n# real\n";
        assert_eq!(titles(md2), vec![(1, "real".into())]);
    }

    #[test]
    fn scanner_unclosed_fence_swallows_the_rest() {
        let md = "# a\n\n```\n# b\n## c\n";
        assert_eq!(titles(md), vec![(1, "a".into())]);
    }

    #[test]
    fn scanner_does_not_index_setext_headings() {
        let md = "Title\n=====\n\nSub\n---\n\n# atx\n";
        assert_eq!(titles(md), vec![(1, "atx".into())]);
    }

    #[test]
    fn extents_are_hierarchical_and_include_children() {
        let md = "# A\na\n## A1\na1\n## A2\na2\n# B\nb\n";
        let s = scan_sections(md);
        assert_eq!(s[0].text(md), "# A\na\n## A1\na1\n## A2\na2\n", "A owns its children");
        assert_eq!(s[1].text(md), "## A1\na1\n");
        assert_eq!(s[2].text(md), "## A2\na2\n");
        assert_eq!(s[3].text(md), "# B\nb\n");
        // Top level is A and B: A1/A2 live inside A.
        let tops: Vec<&str> = top_level(&s).iter().map(|x| x.title.as_str()).collect();
        assert_eq!(tops, vec!["A", "B"]);
    }

    #[test]
    fn a_deeper_heading_before_a_shallower_one_is_still_top_level() {
        // No enclosing section exists yet, so #### deep is not nested.
        let md = "#### deep\nd\n## shallow\ns\n";
        let s = scan_sections(md);
        assert_eq!(s[0].text(md), "#### deep\nd\n");
        let tops: Vec<&str> = top_level(&s).iter().map(|x| x.title.as_str()).collect();
        assert_eq!(tops, vec!["deep", "shallow"]);
    }

    #[test]
    fn duplicate_titles_are_kept_as_distinct_sections() {
        let md = "## Setup\nfirst\n## Other\nx\n## Setup\nsecond\n";
        let s = scan_sections(md);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].title, "Setup");
        assert_eq!(s[2].title, "Setup");
        assert_eq!(s[0].text(md), "## Setup\nfirst\n");
        assert_eq!(s[2].text(md), "## Setup\nsecond\n");
    }

    #[test]
    fn intro_empty_file_heading_only_and_intro_only() {
        assert!(scan_sections("").is_empty());
        assert_eq!(intro("", &[]), "");

        let heading_only = "# H\n";
        let s = scan_sections(heading_only);
        assert_eq!(intro(heading_only, &s), "", "no intro before a leading heading");

        let intro_only = "just prose\nmore prose\n";
        let s = scan_sections(intro_only);
        assert!(s.is_empty());
        assert_eq!(intro(intro_only, &s), intro_only, "no headings: all intro");
    }

    #[test]
    fn index_lines_number_every_section_with_level_and_size() {
        let md = "# A\nxx\n## A1\nyyy\n";
        let s = scan_sections(md);
        assert_eq!(
            render_index_lines(md, &s),
            vec!["1. # A (17 chars)".to_string(), "2. ## A1 (10 chars)".to_string()]
        );
    }

    /// THE invariant behind "nothing is summarized or omitted": whatever the
    /// index offers, taking the intro plus every top-level section in order
    /// reconstructs the minified document byte for byte. If this holds, an
    /// index plus follow-up fetches can always recover the whole file.
    #[test]
    fn intro_plus_top_level_sections_reconstruct_the_document() {
        for md in CORPUS {
            let body = minify(md);
            let sections = scan_sections(&body);
            let mut rebuilt = String::from(intro(&body, &sections));
            for s in top_level(&sections) {
                rebuilt.push_str(s.text(&body));
            }
            assert_eq!(rebuilt, body, "reconstruction failed for {md:?}");
        }
    }
}
