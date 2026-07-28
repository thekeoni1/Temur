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

/// Render the config JSON for a template. Built by hand (not serde) so the
/// field order matches the README recipes byte for byte; user-supplied
/// strings go through serde_json escaping.
fn render_config(template: &Template, model: &str, key_file: Option<&str>) -> String {
    let m = serde_json::to_string(model).expect("string serializes");
    match template.name {
        "local" => format!(
            "{{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 1024,\n  \"openai_compat\": {{ \"model\": {m}, \"context_window\": 8192 }}\n}}\n"
        ),
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

/// The wizard. Writes `cfg_path`; refuses to overwrite an existing config
/// unless `force`. Returns the lines it printed through `out`.
pub fn run(
    cfg_path: &Path,
    home: Option<&Path>,
    force: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
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

    let model = ask(input, out, "Model id", template.default_model)?;

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
            let rendered = render_config(t, t.default_model, key);
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
        let rendered = render_config(t, "we\"ird", Some("/k\"ey"));
        let cfg: crate::config::Config = serde_json::from_str(&rendered).expect("escaped");
        let r = cfg.resolve_base().unwrap();
        assert_eq!(r.model, "we\"ird");
        assert_eq!(r.api_key_file.as_deref(), Some("/k\"ey"));
    }
}
