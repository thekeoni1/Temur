//! `temur init` (T14): a line-based wizard that writes a starter config.
//!
//! Line-based on purpose: answers can be piped in, so tests (and scripts)
//! drive it exactly like a human would. Key handling follows the by-path
//! rule absolutely: for keyed templates the wizard creates the key file
//! EMPTY (mode 600, parent dir 700 if it has to create it) and tells the
//! user to paste the key in with their editor. It never accepts, reads,
//! echoes, or stores key material.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// One selectable starter template. `key_slug` is `Some` for keyed
/// templates and names the provider piece of the default key file path
/// (`~/.secrets/temur-<slug>-key`).
struct Template {
    number: &'static str,
    name: &'static str,
    describe: &'static str,
    default_model: &'static str,
    key_slug: Option<&'static str>,
}

const TEMPLATES: [Template; 4] = [
    Template {
        number: "1",
        name: "local",
        describe: "llama.cpp / Ollama / LM Studio (openai-compat, keyless)",
        default_model: "qwen3-1.7b",
        key_slug: None,
    },
    Template {
        number: "2",
        name: "anthropic",
        describe: "Anthropic API (key file)",
        default_model: "claude-sonnet-5",
        key_slug: Some("anthropic"),
    },
    Template {
        number: "3",
        name: "openai",
        describe: "OpenAI API (openai-compat, key file)",
        default_model: "gpt-4o-mini",
        key_slug: Some("openai"),
    },
    Template {
        number: "4",
        name: "gemini",
        describe: "Gemini API (openai-compat, key file)",
        default_model: "gemini-2.5-flash",
        key_slug: Some("gemini"),
    },
];

const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";

/// How many listed model ids the picker prints before folding the rest
/// into an "... and N more" line (a number still selects any of them).
const MODEL_LIST_CAP: usize = 20;

/// Render the config JSON for a template. Built by hand (not serde) so the
/// field order matches the README recipes byte for byte; user-supplied
/// strings go through serde_json escaping. `base_url` is the local
/// template's answered base URL (T15); when it is the default the render
/// stays byte-identical to the pre-T15 recipe.
fn render_config(
    template: &Template,
    model: &str,
    key_file: Option<&str>,
    base_url: Option<&str>,
) -> String {
    let m = serde_json::to_string(model).expect("string serializes");
    match template.name {
        "local" => match base_url {
            Some(b) if b != crate::config::DEFAULT_OPENAI_COMPAT_BASE_URL => {
                let b = serde_json::to_string(b).expect("string serializes");
                format!(
                    "{{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 1024,\n  \"openai_compat\": {{ \"base_url\": {b},\n                     \"model\": {m}, \"context_window\": 8192 }}\n}}\n"
                )
            }
            _ => format!(
                "{{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 1024,\n  \"openai_compat\": {{ \"model\": {m}, \"context_window\": 8192 }}\n}}\n"
            ),
        },
        "anthropic" => {
            let k = serde_json::to_string(key_file.expect("anthropic is keyed"))
                .expect("string serializes");
            format!(
                "{{\n  \"profiles\": {{\n    \"anthropic\": {{ \"provider\": \"anthropic\", \"model\": {m},\n                   \"api_key_file\": {k} }}\n  }},\n  \"profile\": \"anthropic\"\n}}\n"
            )
        }
        "openai" | "gemini" => {
            let base = if template.name == "openai" {
                OPENAI_BASE_URL
            } else {
                GEMINI_BASE_URL
            };
            let k = serde_json::to_string(key_file.expect("keyed template"))
                .expect("string serializes");
            format!(
                "{{\n  \"provider\": \"openai-compat\",\n  \"openai_compat\": {{ \"base_url\": \"{base}\",\n                     \"model\": {m},\n                     \"api_key_file\": {k} }}\n}}\n"
            )
        }
        other => unreachable!("unknown template {other}"),
    }
}

/// Ask one question and read one line. Empty answer = `default`. EOF is an
/// error: with piped answers a short script is a bug, not a choice.
fn ask(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    prompt: &str,
    default: &str,
) -> Result<String, crate::error::Error> {
    write!(out, "{prompt} [{default}]: ")?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Err(crate::error::Error::Config(
            "init: unexpected end of input (the wizard needs an answer per question)".into(),
        ));
    }
    let ans = line.trim();
    Ok(if ans.is_empty() {
        default.to_string()
    } else {
        ans.to_string()
    })
}

/// Expand a leading `~/` against `home`. Anything else passes through.
fn expand_tilde(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(path),
    }
}

/// Model picker over a live server listing (T15). Prints the ids numbered
/// (capped at [`MODEL_LIST_CAP`]; a number still selects any entry) and
/// asks one question whose answer is a NUMBER into the listing or a
/// free-text model id. Default: the template's default model when the
/// server lists it, else the first listed id.
fn pick_model(
    ids: &[String],
    template_default: &str,
    base_url: &str,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<String, crate::error::Error> {
    writeln!(out, "Models on {base_url}:")?;
    for (i, id) in ids.iter().take(MODEL_LIST_CAP).enumerate() {
        writeln!(out, "  {}) {id}", i + 1)?;
    }
    if ids.len() > MODEL_LIST_CAP {
        writeln!(out, "  ... and {} more", ids.len() - MODEL_LIST_CAP)?;
    }
    let default = if ids.iter().any(|i| i == template_default) {
        template_default.to_string()
    } else {
        ids[0].clone()
    };
    let answer = ask(input, out, "Model (number or id)", &default)?;
    if answer.chars().all(|c| c.is_ascii_digit()) {
        match answer.parse::<usize>() {
            Ok(n) if (1..=ids.len()).contains(&n) => Ok(ids[n - 1].clone()),
            _ => Err(crate::error::Error::Config(format!(
                "init: model number {answer} is out of range (1-{})",
                ids.len()
            ))),
        }
    } else {
        Ok(answer)
    }
}

/// The wizard. Writes `cfg_path`; refuses to overwrite an existing config
/// unless `force`. Returns the lines it printed through `out`.
///
/// `list_models` is the ONE network call the wizard may make (T15): an
/// unauthenticated keyless listing GET, injected from main (the real
/// [`crate::provider::list_models_keyless`]) so tests script listings
/// without a network. It is only ever called for the keyless local
/// template; keyed templates stay free-text — their key files are created
/// EMPTY below, so no authenticated listing is possible at init time even
/// in principle, and init never reads keys.
pub fn run(
    cfg_path: &Path,
    home: Option<&Path>,
    force: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    list_models: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
) -> Result<(), crate::error::Error> {
    if cfg_path.exists() && !force {
        return Err(crate::error::Error::Config(format!(
            "config already exists at {}; rerun with --force to overwrite it",
            cfg_path.display()
        )));
    }

    writeln!(out, "temur init: guided starter config")?;
    writeln!(out, "Config will be written to: {}", cfg_path.display())?;
    writeln!(out)?;
    writeln!(out, "Templates:")?;
    for t in &TEMPLATES {
        writeln!(out, "  {}) {:<10} {}", t.number, t.name, t.describe)?;
    }
    let choice = ask(input, out, "Template", "1")?;
    let template = TEMPLATES
        .iter()
        .find(|t| t.number == choice || t.name == choice)
        .ok_or_else(|| {
            crate::error::Error::Config(format!(
                "init: unknown template {choice:?} (expected 1-4 or a template name)"
            ))
        })?;

    // Local (keyless) template: ask where the server lives, then try its
    // listing so the model question offers real ids instead of a blind
    // free-text guess. Any listing problem falls back to exactly the old
    // free-text question after a one-line note — the wizard must complete
    // offline. Keyed templates: free text, unchanged (see `list_models`).
    let mut base_url: Option<String> = None;
    let model = if template.key_slug.is_none() {
        let base = ask(
            input,
            out,
            "Base URL",
            crate::config::DEFAULT_OPENAI_COMPAT_BASE_URL,
        )?;
        let picked = match list_models(&base) {
            Ok(ids) if !ids.is_empty() => {
                pick_model(&ids, template.default_model, &base, input, out)?
            }
            outcome => {
                let why = match outcome {
                    Ok(_) => "the server returned an empty listing".to_string(),
                    Err(e) => e.to_string(),
                };
                writeln!(out, "could not list models from {base}: {why}")?;
                ask(input, out, "Model id", template.default_model)?
            }
        };
        base_url = Some(base);
        picked
    } else {
        ask(input, out, "Model id", template.default_model)?
    };

    // Keyed templates: ask for the key FILE PATH only. The key itself never
    // passes through temur, in any direction.
    let key_file: Option<PathBuf> = match template.key_slug {
        None => None,
        Some(slug) => {
            let default = match home {
                Some(h) => h
                    .join(".secrets")
                    .join(format!("temur-{slug}-key"))
                    .display()
                    .to_string(),
                None => String::new(),
            };
            let answer = ask(input, out, "API key file", &default)?;
            if answer.is_empty() {
                return Err(crate::error::Error::Config(
                    "init: no HOME to derive a default key file path; enter one explicitly"
                        .into(),
                ));
            }
            Some(expand_tilde(&answer, home))
        }
    };

    // Write the config (parent dir as needed; the config holds no secret,
    // so default directory modes are fine).
    if let Some(dir) = cfg_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let rendered = render_config(
        template,
        &model,
        key_file.as_ref().map(|p| p.display().to_string()).as_deref(),
        base_url.as_deref(),
    );
    std::fs::write(cfg_path, &rendered)?;
    writeln!(out)?;
    writeln!(out, "Wrote {}", cfg_path.display())?;

    // Key file: created EMPTY with tight modes, and never touched if it
    // already exists (it may already hold a real key, which temur must not
    // read, truncate, or rewrite).
    if let Some(key_path) = &key_file {
        if key_path.exists() {
            writeln!(out, "Key file {} already exists; left untouched.", key_path.display())?;
        } else {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
            if let Some(dir) = key_path.parent() {
                if !dir.exists() {
                    std::fs::DirBuilder::new()
                        .recursive(true)
                        .mode(0o700)
                        .create(dir)?;
                    // Modes pass through umask at creation; pin them exact.
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
                }
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(key_path)?;
            std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))?;
            writeln!(out, "Created empty key file {} (mode 600).", key_path.display())?;
        }
        writeln!(out)?;
        writeln!(
            out,
            "Paste your key into {} with your editor. temur reads it only by\npath at startup and never accepts, echoes, or stores key material.",
            key_path.display()
        )?;
    }

    writeln!(out)?;
    if template.key_slug.is_none() {
        writeln!(
            out,
            "Next: start your local server (see docs/OFFLINE.md), run temur doctor\nto check the setup, then temur to start."
        )?;
    } else {
        writeln!(out, "Next: temur doctor to check the setup, then temur to start.")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expansion_only_rewrites_a_leading_tilde_slash() {
        let home = PathBuf::from("/home/u");
        assert_eq!(
            expand_tilde("~/.secrets/k", Some(&home)),
            PathBuf::from("/home/u/.secrets/k")
        );
        assert_eq!(expand_tilde("/abs/k", Some(&home)), PathBuf::from("/abs/k"));
        assert_eq!(expand_tilde("rel/k", Some(&home)), PathBuf::from("rel/k"));
        // No home: the literal survives (the caller rejects empty answers).
        assert_eq!(expand_tilde("~/.k", None), PathBuf::from("~/.k"));
    }

    #[test]
    fn every_template_renders_parseable_config_selecting_the_right_provider() {
        for t in &TEMPLATES {
            let key = t.key_slug.map(|_| "/tmp/k");
            let rendered = render_config(t, t.default_model, key, None);
            let cfg: crate::config::Config =
                serde_json::from_str(&rendered).unwrap_or_else(|e| {
                    panic!("template {} renders invalid config: {e}\n{rendered}", t.name)
                });
            let profiles = cfg.resolved_profiles().expect("profiles validate");
            let (_, resolved) = cfg.startup_selection(&profiles).expect("selection resolves");
            assert_eq!(resolved.model, t.default_model, "template {}", t.name);
            match t.name {
                "local" => {
                    assert_eq!(resolved.provider, "openai-compat");
                    assert!(resolved.api_key_file.is_none(), "local stays keyless");
                }
                "anthropic" => {
                    assert_eq!(resolved.provider, "anthropic");
                    assert_eq!(resolved.api_key_file.as_deref(), Some("/tmp/k"));
                }
                "openai" | "gemini" => {
                    assert_eq!(resolved.provider, "openai-compat");
                    assert_eq!(resolved.api_key_file.as_deref(), Some("/tmp/k"));
                    let expect = if t.name == "openai" {
                        OPENAI_BASE_URL
                    } else {
                        GEMINI_BASE_URL
                    };
                    assert_eq!(resolved.base_url, expect);
                }
                other => panic!("unknown template {other}"),
            }
        }
    }

    #[test]
    fn model_and_path_strings_are_json_escaped() {
        let t = &TEMPLATES[2]; // openai
        let rendered = render_config(t, "we\"ird", Some("/k\"ey"), None);
        let cfg: crate::config::Config = serde_json::from_str(&rendered).expect("escaped");
        let r = cfg.resolve_base().unwrap();
        assert_eq!(r.model, "we\"ird");
        assert_eq!(r.api_key_file.as_deref(), Some("/k\"ey"));
    }

    // ------------------------------------------- T15: base URL + model picker

    #[test]
    fn local_render_default_base_url_is_byte_identical_to_the_readme_recipe() {
        let t = &TEMPLATES[0]; // local
        let expect = "{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 1024,\n  \"openai_compat\": { \"model\": \"qwen3-1.7b\", \"context_window\": 8192 }\n}\n";
        // Both the no-answer path and an answered default render the recipe.
        assert_eq!(render_config(t, "qwen3-1.7b", None, None), expect);
        assert_eq!(
            render_config(
                t,
                "qwen3-1.7b",
                None,
                Some(crate::config::DEFAULT_OPENAI_COMPAT_BASE_URL)
            ),
            expect
        );
    }

    #[test]
    fn local_render_custom_base_url_survives_and_parses() {
        let t = &TEMPLATES[0];
        let rendered = render_config(t, "m", None, Some("http://10.0.0.9:11434/v1"));
        let cfg: crate::config::Config = serde_json::from_str(&rendered).unwrap();
        let r = cfg.resolve_base().unwrap();
        assert_eq!(r.base_url, "http://10.0.0.9:11434/v1");
        assert_eq!(r.model, "m");
        assert!(r.api_key_file.is_none());
    }

    /// Drive the whole wizard with piped answers and a scripted listing.
    fn run_wizard(
        answers: &str,
        list: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
    ) -> Result<(String, String), crate::error::Error> {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let mut input = std::io::Cursor::new(answers.as_bytes().to_vec());
        let mut out: Vec<u8> = Vec::new();
        run(&cfg_path, None, false, &mut input, &mut out, list)?;
        Ok((
            std::fs::read_to_string(&cfg_path).unwrap(),
            String::from_utf8(out).unwrap(),
        ))
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn picker_number_selects_from_the_listing() {
        let list = |_: &str| Ok(ids(&["alpha", "beta", "gamma"]));
        // Template default, base URL default, model by number.
        let (cfg, out) = run_wizard("\n\n2\n", &list).unwrap();
        assert!(cfg.contains("\"model\": \"beta\""), "{cfg}");
        assert!(out.contains("Models on "), "{out}");
        assert!(out.contains("  1) alpha") && out.contains("  3) gamma"), "{out}");
        // Default base URL answered: the recipe render, no base_url key.
        assert!(!cfg.contains("base_url"), "{cfg}");
    }

    #[test]
    fn picker_free_text_id_and_custom_base_url_survive() {
        let list = |base: &str| {
            assert_eq!(base, "http://10.0.0.9:11434/v1", "picker lists the ANSWERED base");
            Ok(ids(&["served-model"]))
        };
        let (cfg, _out) =
            run_wizard("\nhttp://10.0.0.9:11434/v1\nmy-custom\n", &list).unwrap();
        assert!(cfg.contains("\"model\": \"my-custom\""), "{cfg}");
        assert!(cfg.contains("\"base_url\": \"http://10.0.0.9:11434/v1\""), "{cfg}");
    }

    #[test]
    fn picker_default_is_template_default_when_listed_else_first() {
        // Template default present in the listing: empty answer picks it.
        let list = |_: &str| Ok(ids(&["other", "qwen3-1.7b"]));
        let (cfg, out) = run_wizard("\n\n\n", &list).unwrap();
        assert!(cfg.contains("\"model\": \"qwen3-1.7b\""), "{cfg}");
        assert!(out.contains("[qwen3-1.7b]"), "default shown: {out}");
        // Absent: the first listed id becomes the default.
        let list = |_: &str| Ok(ids(&["first-served", "second"]));
        let (cfg, out) = run_wizard("\n\n\n", &list).unwrap();
        assert!(cfg.contains("\"model\": \"first-served\""), "{cfg}");
        assert!(out.contains("[first-served]"), "{out}");
    }

    #[test]
    fn picker_caps_the_printed_listing_and_numbers_still_reach_the_tail() {
        let many: Vec<String> = (1..=25).map(|i| format!("m{i:02}")).collect();
        let list = move |_: &str| Ok(many.clone());
        let (cfg, out) = run_wizard("\n\n25\n", &list).unwrap();
        assert!(out.contains("  20) m20"), "{out}");
        assert!(!out.contains("m21"), "listing capped: {out}");
        assert!(out.contains("... and 5 more"), "{out}");
        assert!(cfg.contains("\"model\": \"m25\""), "a number past the cap selects: {cfg}");
    }

    #[test]
    fn picker_out_of_range_number_is_a_clean_error() {
        let list = |_: &str| Ok(ids(&["only"]));
        let err = run_wizard("\n\n7\n", &list).unwrap_err().to_string();
        assert!(err.contains("7") && err.contains("out of range (1-1)"), "{err}");
    }

    #[test]
    fn listing_failure_or_empty_falls_back_to_free_text_with_a_note() {
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            Err(crate::error::Error::Models("connection refused".into()))
        };
        let (cfg, out) = run_wizard("\n\n\n", &list).unwrap();
        assert!(out.contains("could not list models from"), "{out}");
        assert!(out.contains("connection refused"), "{out}");
        assert!(out.contains("Model id"), "free-text question asked: {out}");
        assert!(cfg.contains("\"model\": \"qwen3-1.7b\""), "{cfg}");

        let list = |_: &str| Ok(Vec::<String>::new());
        let (_cfg, out) = run_wizard("\n\ncustom\n", &list).unwrap();
        assert!(out.contains("empty listing"), "{out}");
    }

    #[test]
    fn keyed_templates_never_call_the_listing() {
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            panic!("keyed templates must not attempt a listing")
        };
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let key_path = tmp.path().join("some-key");
        let mut input =
            std::io::Cursor::new(format!("2\n\n{}\n", key_path.display()).into_bytes());
        let mut out: Vec<u8> = Vec::new();
        // home None + explicit key path; the wizard completes without ever
        // touching `list`.
        run(&cfg_path, None, false, &mut input, &mut out, &list).unwrap();
        let cfg = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(cfg.contains("\"model\": \"claude-sonnet-5\""), "{cfg}");
        let printed = String::from_utf8(out).unwrap();
        assert!(!printed.contains("Base URL"), "keyed asks no base URL: {printed}");
    }
}
