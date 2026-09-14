//! The default system prompts, one per [`crate::tools::PromptProfile`].
//!
//! These lived in `main.rs` through v0.29.1, where nothing outside the
//! binary could see them. T41 moved them here UNCHANGED, byte for byte,
//! because `temur doctor` has to weigh the real prompt to report a prompt
//! floor, and a doctor that measured its own approximation of the prompt
//! would be measuring nothing. `main`'s `rebuild_system` and the `/model`
//! prompt-profile swap read them through [`system_prompt_template`]; the
//! config `system_prompt` override still wins over either, in either
//! profile, and that rule stays in `main` where it always was.
//!
//! `{cwd}` and `{date}` are placeholders the caller fills with [`render`]:
//! the templates here are the raw text, so a caller that has no working
//! directory (doctor, when it estimates) can still weigh them.

/// Compact default system prompt for v1; overridable via config.
/// (`{cwd}` and `{date}` are substituted at startup.)
pub const DEFAULT_SYSTEM: &str = "You are temur, a terminal coding agent. You help with software \
engineering tasks: reading and editing code, running commands, and searching the codebase.\n\
Use the provided tools (read, write, edit, bash, glob, grep, todowrite, todoread, skill) to act; \
prefer tools over guessing. Keep responses concise and direct — this is a terminal. \
When you edit files, verify your changes. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
The user's files are usually already in the working directory; find them with glob or ls \
before asking for them, since there is no upload. \
Today's date is {date} (UTC). \
The current working directory is: {cwd}";

/// Shorter default system prompt used when `prompt_profile` resolves to
/// `"compact"` AND no config `system_prompt` override exists; an explicit
/// override always wins, in either profile.
pub const DEFAULT_SYSTEM_COMPACT: &str = "You are temur, a coding agent in a terminal. Act through \
the provided tools; always call them with valid JSON arguments — never write a tool call as \
plain text. Prefer tools over guessing, keep answers short, verify edits. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
The user's files are usually already in the working directory; find them with glob or ls \
before asking for them, since there is no upload. \
Today's date is {date} (UTC). \
Working directory: {cwd}";

/// The ONE assembly rule for a session's system prompt (T55).
///
/// Order is the contract: the base (a config `system_prompt` override, or
/// the profile template with `{cwd}` substituted), then the skills
/// section, then the project-instructions block. `main`'s `rebuild_system`
/// and `doctor`'s floor both call this, so the prompt doctor weighs is the
/// prompt a session sends; before this existed the two assembled the same
/// ingredients separately and could drift.
pub fn assemble(base: &str, skills: Option<&str>, project_block: &str) -> String {
    let mut out = String::with_capacity(base.len() + project_block.len() + 256);
    out.push_str(base);
    if let Some(s) = skills {
        out.push_str(s);
    }
    out.push_str(project_block);
    out
}

/// The default system-prompt template for a profile, placeholders unsubstituted.
/// The ONE place the profile-to-prompt mapping lives.
pub fn system_prompt_template(profile: crate::tools::PromptProfile) -> &'static str {
    match profile {
        crate::tools::PromptProfile::Full => DEFAULT_SYSTEM,
        crate::tools::PromptProfile::Compact => DEFAULT_SYSTEM_COMPACT,
    }
}

/// The date `--mock` sessions carry, so replayed requests stay byte-stable.
const MOCK_DATE: &str = "2026-01-01";

/// The date a session's prompt carries, as `YYYY-MM-DD` (T63), and a note
/// when `TEMUR_TODAY` was set but unusable.
///
/// `TEMUR_TODAY` wins when it is shaped `YYYY-MM-DD`; any other value is
/// ignored and reported. Under `--mock` the date is [`MOCK_DATE`].
/// Otherwise it is the UTC civil date of `now_secs`. The line says UTC
/// because reading the local zone database is not worth it for a sentence
/// whose job is to give the model the year.
pub fn prompt_date(mock: bool, env: Option<&str>, now_secs: u64) -> (String, Option<String>) {
    let mut note = None;
    if let Some(v) = env {
        if is_date_shaped(v) {
            return (v.to_string(), None);
        }
        note = Some(format!("TEMUR_TODAY={v:?} is not shaped YYYY-MM-DD; ignoring it"));
    }
    let date = if mock { MOCK_DATE.to_string() } else { utc_date(now_secs) };
    (date, note)
}

fn is_date_shaped(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter()
            .enumerate()
            .all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

/// The UTC civil date of a Unix timestamp, as `YYYY-MM-DD`. Howard
/// Hinnant's days-to-civil algorithm in `i64`, so no date crate is needed
/// and nothing narrows to 32 bits.
pub fn utc_date(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A default template with both placeholders filled. `{date}` goes first:
/// a date is validated and cannot contain `{cwd}`, but a working directory
/// path can contain the text `{date}`, and that must stay as written.
pub fn render(template: &str, cwd: &str, date: &str) -> String {
    template.replace("{date}", date).replace("{cwd}", cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::PromptProfile;

    /// The byte lock on both templates. It began as T41's proof that
    /// moving them out of main.rs changed nothing, and it now pins the
    /// T63 strings, which add the date line; the version marker moved with them ON PURPOSE. The
    /// guard exists so a prompt edit has to change this test deliberately
    /// rather than drift past it, and D22's sentence is exactly such an
    /// edit.
    #[test]
    fn both_templates_are_byte_identical_to_the_pinned_strings() {
        let full_t63 = "You are temur, a terminal coding agent. You help with software \
engineering tasks: reading and editing code, running commands, and searching the codebase.\n\
Use the provided tools (read, write, edit, bash, glob, grep, todowrite, todoread, skill) to act; \
prefer tools over guessing. Keep responses concise and direct \u{2014} this is a terminal. \
When you edit files, verify your changes. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
The user's files are usually already in the working directory; find them with glob or ls \
before asking for them, since there is no upload. \
Today's date is {date} (UTC). \
The current working directory is: {cwd}";
        let compact_t63 = "You are temur, a coding agent in a terminal. Act through \
the provided tools; always call them with valid JSON arguments \u{2014} never write a tool call as \
plain text. Prefer tools over guessing, keep answers short, verify edits. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
The user's files are usually already in the working directory; find them with glob or ls \
before asking for them, since there is no upload. \
Today's date is {date} (UTC). \
Working directory: {cwd}";
        assert_eq!(system_prompt_template(PromptProfile::Full), full_t63);
        assert_eq!(
            system_prompt_template(PromptProfile::Compact),
            compact_t63
        );
        // The substitution the callers do, on the template they get back.
        assert!(system_prompt_template(PromptProfile::Full).contains("{cwd}"));
        assert!(system_prompt_template(PromptProfile::Compact).contains("{cwd}"));
        assert!(system_prompt_template(PromptProfile::Full).contains("{date}"));
        assert!(system_prompt_template(PromptProfile::Compact).contains("{date}"));
    }

    /// The order in [`assemble`] IS the contract: base, then skills, then
    /// project instructions. A model that reads the project's rules before
    /// it knows what tools exist is reading them out of context.
    #[test]
    fn assemble_orders_base_then_skills_then_project() {
        let out = assemble("BASE", Some("SKILLS"), "PROJECT");
        assert_eq!(out, "BASESKILLSPROJECT");
        assert_eq!(assemble("BASE", None, "PROJECT"), "BASEPROJECT");
        assert_eq!(assemble("BASE", None, ""), "BASE");
        assert_eq!(assemble("BASE", Some("SKILLS"), ""), "BASESKILLS");
    }

    /// Known days, checked against `date -u -d @<secs>`: the epoch, a
    /// last second of a day, two leap days, and 2100, which is not a leap
    /// year.
    #[test]
    fn utc_date_matches_known_days() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_789_257_600), "2026-09-13");
        assert_eq!(utc_date(1_789_257_600 + 86_399), "2026-09-13");
        assert_eq!(utc_date(1_757_721_600), "2025-09-13");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(1_709_164_800), "2024-02-29");
        assert_eq!(utc_date(4_107_542_400 - 1), "2100-02-28");
        assert_eq!(utc_date(4_107_542_400), "2100-03-01");
    }

    #[test]
    fn prompt_date_prefers_a_well_shaped_override_then_mock_then_the_clock() {
        let now = 1_789_257_600;
        assert_eq!(prompt_date(false, None, now), ("2026-09-13".to_string(), None));
        assert_eq!(prompt_date(true, None, now), ("2026-01-01".to_string(), None));
        assert_eq!(
            prompt_date(false, Some("2031-05-06"), now),
            ("2031-05-06".to_string(), None)
        );
        assert_eq!(
            prompt_date(true, Some("2031-05-06"), now),
            ("2031-05-06".to_string(), None),
            "TEMUR_TODAY wins under --mock too"
        );
        for bad in ["", "2026-9-13", "13-09-2026", "2026/09/13", "2026-09-13 ", "tomorrow"] {
            let (date, note) = prompt_date(false, Some(bad), now);
            assert_eq!(date, "2026-09-13", "{bad:?} must fall back to the clock");
            assert!(note.expect("a note").contains("TEMUR_TODAY"), "{bad:?}");
            let (date, _) = prompt_date(true, Some(bad), now);
            assert_eq!(date, "2026-01-01", "{bad:?} under --mock falls back to the mock date");
        }
    }

    #[test]
    fn render_fills_the_date_before_the_cwd() {
        assert_eq!(render("d={date} c={cwd}", "/p", "2026-01-01"), "d=2026-01-01 c=/p");
        assert_eq!(
            render("d={date} c={cwd}", "/p/{date}", "2026-01-01"),
            "d=2026-01-01 c=/p/{date}",
            "a cwd containing the text {{date}} is left as written"
        );
        for profile in [PromptProfile::Full, PromptProfile::Compact] {
            let out = render(system_prompt_template(profile), "/w", "2026-01-01");
            assert!(out.contains("Today's date is 2026-01-01 (UTC)."), "{profile:?}");
            assert!(!out.contains('{'), "{profile:?} left a placeholder: {out}");
        }
    }

    /// The reason the compact profile is worth having at all, in bytes.
    #[test]
    fn the_compact_template_is_the_shorter_one() {
        assert!(
            system_prompt_template(PromptProfile::Compact).len()
                < system_prompt_template(PromptProfile::Full).len()
        );
    }
}
