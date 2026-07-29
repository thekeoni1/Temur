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

/// The anthropic template's curated profile set (T16): one profile per
/// current Anthropic model tier, all sharing the ONE key file the wizard
/// asks for. NAME order on purpose: profiles is a BTreeMap downstream, so
/// this is also the order every listing shows.
const ANTHROPIC_PROFILES: [(&str, &str); 4] = [
    ("fable", "claude-fable-5"),
    ("haiku", "claude-haiku-4-5"),
    ("opus", "claude-opus-5"),
    ("sonnet", "claude-sonnet-5"),
];

/// The startup profile the anthropic template defaults to. Keeps the
/// pre-T16 default model (claude-sonnet-5): no default flip.
const ANTHROPIC_DEFAULT_PROFILE: &str = "sonnet";

const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";

/// The hosted openai-compat templates' fixed endpoints, one lookup shared
/// by the fresh render and `init --add` so a new template lands in both.
fn compat_base_url(template_name: &str) -> &'static str {
    match template_name {
        "openai" => OPENAI_BASE_URL,
        "gemini" => GEMINI_BASE_URL,
        other => unreachable!("template {other} has no fixed base URL"),
    }
}

/// How many listed model ids the picker prints before folding the rest
/// into an "... and N more" line (a number still selects any of them).
const MODEL_LIST_CAP: usize = 20;

/// Baked model shortlist (T15 P4), printed ONLY when the local template's
/// picker could not run (no server to ask). A hand-kept SUMMARY of
/// docs/OFFLINE.md, section "Recommended small models", which stays
/// canonical: update that table first and mirror the top rows here.
/// When the picker works, the server's real listing wins and this never
/// prints.
const MODEL_SHORTLIST: &[&str] = &[
    "Known-good small models:",
    "  Qwen3-1.7B Q4_K_M (~2.1 GB RAM at 8k context; the primary recommendation)",
    "  Qwen3-4B-Instruct-2507 Q4_K_M (~3.4 GB RAM)",
    "Larger is better when RAM allows; 7B+ is qualitatively different.",
    "See docs/OFFLINE.md, section \"Recommended small models\".",
];

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
                    "{{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": {{ \"base_url\": {b},\n                     \"model\": {m}, \"context_window\": 8192 }}\n}}\n"
                )
            }
            _ => format!(
                "{{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": {{ \"model\": {m}, \"context_window\": 8192 }}\n}}\n"
            ),
        },
        "anthropic" => {
            // T16: the curated profile set. `model` carries the answered
            // STARTUP PROFILE NAME (from pick_startup_profile), not a model
            // id; every profile reads the same key file.
            let k = serde_json::to_string(key_file.expect("anthropic is keyed"))
                .expect("string serializes");
            let mut s = String::from("{\n  \"profiles\": {\n");
            for (i, (name, model_id)) in ANTHROPIC_PROFILES.iter().enumerate() {
                let comma = if i + 1 == ANTHROPIC_PROFILES.len() { "" } else { "," };
                let label = format!("\"{name}\":");
                s.push_str(&format!(
                    "    {label:<9} {{ \"provider\": \"anthropic\", \"model\": \"{model_id}\",\n                \"api_key_file\": {k} }}{comma}\n"
                ));
            }
            s.push_str(&format!("  }},\n  \"profile\": {m}\n}}\n"));
            s
        }
        "openai" | "gemini" => {
            let base = compat_base_url(template.name);
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

/// The local template's flow (T15): ask for the base URL, try the keyless
/// listing there and run the picker, falling back to the free-text model
/// question (after the baked shortlist) when the listing fails or is empty.
/// Returns `(base_url, model)`. Shared by the fresh wizard and `init --add`.
fn ask_local_base_and_model(
    template: &Template,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    list_models: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
) -> Result<(String, String), crate::error::Error> {
    let base = ask(
        input,
        out,
        "Base URL",
        crate::config::DEFAULT_OPENAI_COMPAT_BASE_URL,
    )?;
    let picked = match list_models(&base) {
        Ok(ids) if !ids.is_empty() => pick_model(&ids, template.default_model, &base, input, out)?,
        outcome => {
            let why = match outcome {
                Ok(_) => "the server returned an empty listing".to_string(),
                Err(e) => e.to_string(),
            };
            writeln!(out, "could not list models from {base}: {why}")?;
            for line in MODEL_SHORTLIST {
                writeln!(out, "{line}")?;
            }
            ask(input, out, "Model id", template.default_model)?
        }
    };
    Ok((base, picked))
}

/// The key FILE PATH question for keyed templates. The key itself never
/// passes through temur, in any direction. Shared by the fresh wizard and
/// `init --add`.
fn ask_key_file(
    slug: &str,
    home: Option<&Path>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<PathBuf, crate::error::Error> {
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
            "init: no HOME to derive a default key file path; enter one explicitly".into(),
        ));
    }
    Ok(expand_tilde(&answer, home))
}

/// Key file creation plus the paste instruction, shared by the fresh wizard
/// and `init --add`: created EMPTY with tight modes, and never touched if it
/// already exists (it may already hold a real key, which temur must not
/// read, truncate, or rewrite).
fn setup_key_file(key_path: &Path, out: &mut dyn Write) -> Result<(), crate::error::Error> {
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
    Ok(())
}

/// Startup-profile question for the anthropic template (T16). Prints the
/// curated profiles numbered and asks which one the config should start
/// on; the answer is a NUMBER into the listing or a profile name. Default
/// sonnet; anything else re-asks (EOF stays an error via `ask`).
fn pick_startup_profile(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<&'static str, crate::error::Error> {
    writeln!(out, "Profiles this template writes:")?;
    for (i, (name, model_id)) in ANTHROPIC_PROFILES.iter().enumerate() {
        writeln!(out, "  {}) {name:<7} {model_id}", i + 1)?;
    }
    loop {
        let answer = ask(
            input,
            out,
            "Startup profile (number or name)",
            ANTHROPIC_DEFAULT_PROFILE,
        )?;
        if answer.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = answer.parse::<usize>() {
                if (1..=ANTHROPIC_PROFILES.len()).contains(&n) {
                    return Ok(ANTHROPIC_PROFILES[n - 1].0);
                }
            }
        } else if let Some((name, _)) =
            ANTHROPIC_PROFILES.iter().find(|(name, _)| *name == answer)
        {
            return Ok(name);
        }
        writeln!(
            out,
            "unknown profile {answer:?} (expected 1-{} or a profile name)",
            ANTHROPIC_PROFILES.len()
        )?;
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
        let (base, picked) = ask_local_base_and_model(template, input, out, list_models)?;
        base_url = Some(base);
        picked
    } else if template.name == "anthropic" {
        // T16: the anthropic template writes a fixed profile set, so the
        // model question becomes a startup-profile question. `model` holds
        // the chosen profile NAME from here on (see render_config).
        pick_startup_profile(input, out)?.to_string()
    } else {
        ask(input, out, "Model id", template.default_model)?
    };

    // Keyed templates: ask for the key FILE PATH only. The key itself never
    // passes through temur, in any direction.
    let key_file: Option<PathBuf> = match template.key_slug {
        None => None,
        Some(slug) => Some(ask_key_file(slug, home, input, out)?),
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

    if let Some(key_path) = &key_file {
        setup_key_file(key_path, out)?;
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
    // T16: sessions discoverability — autosave was routinely discovered by
    // accident, so the wizard says it once at the end.
    writeln!(
        out,
        "Conversations are saved automatically per working directory; temur --continue\nresumes the last one."
    )?;
    Ok(())
}

/// `temur init --add <template>` (T17): merge one template into an EXISTING
/// config instead of writing a fresh one. Always merges AS PROFILES,
/// whatever shape the template's fresh render has: the base selection, the
/// startup `"profile"` key, and every other field in the file are left
/// alone (surgical `serde_json::Value` edit, same preserve_order +
/// temp-then-rename mechanics as [`crate::config::persist_model`]).
/// Fail-closed: if ANY profile name to be added already exists, the whole
/// merge aborts with the file untouched — never a silent overwrite of a
/// profile the user wrote by hand.
pub fn run_add(
    cfg_path: &Path,
    home: Option<&Path>,
    template_name: &str,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    list_models: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
) -> Result<(), crate::error::Error> {
    let template = TEMPLATES
        .iter()
        .find(|t| t.name == template_name)
        .ok_or_else(|| {
            let names: Vec<&str> = TEMPLATES.iter().map(|t| t.name).collect();
            crate::error::Error::Config(format!(
                "init --add: unknown template {template_name:?} (expected {})",
                names.join(", ")
            ))
        })?;
    let raw = match std::fs::read_to_string(cfg_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(crate::error::Error::Config(format!(
                "init --add: no config at {}; plain `temur init` creates one",
                cfg_path.display()
            )))
        }
        Err(e) => return Err(e.into()),
    };
    let mut v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| crate::error::Error::Config(format!("{}: {e}", cfg_path.display())))?;
    if !v.is_object() {
        return Err(crate::error::Error::Config(format!(
            "{}: not a JSON object",
            cfg_path.display()
        )));
    }
    if v.get("profiles").is_some_and(|p| !p.is_object()) {
        return Err(crate::error::Error::Config(format!(
            "{}: \"profiles\" is not a JSON object",
            cfg_path.display()
        )));
    }

    // Collisions are checked BEFORE any question runs, so a doomed merge
    // never wastes the user's answers.
    let adding: Vec<&'static str> = if template.name == "anthropic" {
        ANTHROPIC_PROFILES.iter().map(|(n, _)| *n).collect()
    } else {
        vec![template.name]
    };
    let collisions: Vec<&str> = adding
        .iter()
        .copied()
        .filter(|n| {
            v.get("profiles")
                .and_then(|p| p.as_object())
                .is_some_and(|p| p.contains_key(*n))
        })
        .collect();
    if !collisions.is_empty() {
        let plural = if collisions.len() == 1 { "" } else { "s" };
        let quoted: Vec<String> = collisions.iter().map(|n| format!("{n:?}")).collect();
        return Err(crate::error::Error::Config(format!(
            "init --add {}: profile{plural} {} already in {}; nothing was changed \
             (rename or remove the existing profile{plural} first)",
            template.name,
            quoted.join(", "),
            cfg_path.display()
        )));
    }

    writeln!(
        out,
        "temur init --add {}: merging into {}",
        template.name,
        cfg_path.display()
    )?;
    writeln!(out)?;

    // The template's questions, mirroring the fresh wizard: base URL +
    // picker for local, free-text model for hosted compat templates. The
    // anthropic model ids are fixed (T16) and the startup "profile" key is
    // never written here, so only the key question runs for it.
    let mut new_profiles: Vec<(String, serde_json::Value)> = Vec::new();
    let mut key_file: Option<PathBuf> = None;
    match template.name {
        "local" => {
            let (base, model) = ask_local_base_and_model(template, input, out, list_models)?;
            let mut p = serde_json::Map::new();
            p.insert("provider".to_string(), "openai-compat".into());
            p.insert("model".to_string(), model.into());
            if base != crate::config::DEFAULT_OPENAI_COMPAT_BASE_URL {
                p.insert("base_url".to_string(), base.into());
            }
            // The fresh local template's small-model limits, carried into
            // the profile so switching to it behaves like a fresh local
            // config (a profile's absent max_tokens would inherit the
            // global value instead).
            p.insert("max_tokens".to_string(), 4096.into());
            p.insert("context_window".to_string(), 8192.into());
            new_profiles.push((template.name.to_string(), p.into()));
        }
        "anthropic" => {
            let key = ask_key_file("anthropic", home, input, out)?;
            let k = key.display().to_string();
            for (name, model_id) in &ANTHROPIC_PROFILES {
                let mut p = serde_json::Map::new();
                p.insert("provider".to_string(), "anthropic".into());
                p.insert("model".to_string(), (*model_id).into());
                p.insert("api_key_file".to_string(), k.clone().into());
                new_profiles.push(((*name).to_string(), p.into()));
            }
            key_file = Some(key);
        }
        hosted => {
            let model = ask(input, out, "Model id", template.default_model)?;
            let key = ask_key_file(
                template.key_slug.expect("hosted templates are keyed"),
                home,
                input,
                out,
            )?;
            let mut p = serde_json::Map::new();
            p.insert("provider".to_string(), "openai-compat".into());
            p.insert("base_url".to_string(), compat_base_url(hosted).into());
            p.insert("model".to_string(), model.into());
            p.insert("api_key_file".to_string(), key.display().to_string().into());
            new_profiles.push((hosted.to_string(), p.into()));
            key_file = Some(key);
        }
    }

    {
        let root = v.as_object_mut().expect("checked above");
        let entry = root
            .entry("profiles".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let profs = entry.as_object_mut().expect("checked above");
        for (name, p) in &new_profiles {
            profs.insert(name.clone(), p.clone());
        }
    }
    crate::config::write_config_value(cfg_path, &v)?;

    writeln!(out)?;
    let quoted: Vec<String> = new_profiles.iter().map(|(n, _)| format!("\"{n}\"")).collect();
    writeln!(
        out,
        "Added profile{} {} to {}.",
        if quoted.len() == 1 { "" } else { "s" },
        quoted.join(", "),
        cfg_path.display()
    )?;
    if let Some(key_path) = &key_file {
        setup_key_file(key_path, out)?;
    }
    writeln!(out)?;
    if new_profiles.len() == 1 {
        let name = &new_profiles[0].0;
        writeln!(
            out,
            "/model {name} switches to it; set \"profile\": \"{name}\" in config.json to\nmake it the startup default."
        )?;
    } else {
        writeln!(
            out,
            "/model <name> switches to one; set \"profile\": \"<name>\" in config.json to\nmake it the startup default."
        )?;
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
            // The anthropic template's "model" answer is a startup profile
            // name (T16); its default profile still resolves the template's
            // default model, which is what the assertion below checks.
            let model_arg = if t.name == "anthropic" {
                ANTHROPIC_DEFAULT_PROFILE
            } else {
                t.default_model
            };
            let rendered = render_config(t, model_arg, key, None);
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
        let expect = "{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": { \"model\": \"qwen3-1.7b\", \"context_window\": 8192 }\n}\n";
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
    fn shortlist_prints_only_when_the_picker_could_not_run() {
        // Fallback path: every baked line, including the canonical pointer.
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            Err(crate::error::Error::Models("connection refused".into()))
        };
        let (_cfg, out) = run_wizard("\n\n\n", &list).unwrap();
        for line in MODEL_SHORTLIST {
            assert!(out.contains(line), "missing {line:?} in:\n{out}");
        }
        // Picker path: the server's listing wins, no shortlist.
        let list = |_: &str| Ok(ids(&["served-model"]));
        let (_cfg, out) = run_wizard("\n\n\n", &list).unwrap();
        assert!(!out.contains("Known-good small models"), "{out}");
        assert!(
            !out.contains("Recommended small models"),
            "no shortlist pointer when the picker ran (the closing Next \
             line's OFFLINE.md mention is separate and fine): {out}"
        );
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

    // ------------------------------------- T16: anthropic profile set wizard

    /// Drive the wizard for the anthropic template with an explicit key
    /// path (home is None in tests, so there is no default key path).
    fn run_anthropic_wizard(profile_answers: &str) -> (String, String) {
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            panic!("anthropic template must not attempt a listing")
        };
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let key_path = tmp.path().join("throwaway-key");
        let mut input = std::io::Cursor::new(
            format!("2\n{profile_answers}{}\n", key_path.display()).into_bytes(),
        );
        let mut out: Vec<u8> = Vec::new();
        run(&cfg_path, None, false, &mut input, &mut out, &list).unwrap();
        (
            std::fs::read_to_string(&cfg_path).unwrap(),
            String::from_utf8(out).unwrap(),
        )
    }

    #[test]
    fn anthropic_render_is_byte_exact_for_the_default_profile() {
        let t = &TEMPLATES[1]; // anthropic
        let rendered = render_config(t, "sonnet", Some("/home/u/.secrets/temur-anthropic-key"), None);
        let expect = "{\n  \"profiles\": {\n    \"fable\":  { \"provider\": \"anthropic\", \"model\": \"claude-fable-5\",\n                \"api_key_file\": \"/home/u/.secrets/temur-anthropic-key\" },\n    \"haiku\":  { \"provider\": \"anthropic\", \"model\": \"claude-haiku-4-5\",\n                \"api_key_file\": \"/home/u/.secrets/temur-anthropic-key\" },\n    \"opus\":   { \"provider\": \"anthropic\", \"model\": \"claude-opus-5\",\n                \"api_key_file\": \"/home/u/.secrets/temur-anthropic-key\" },\n    \"sonnet\": { \"provider\": \"anthropic\", \"model\": \"claude-sonnet-5\",\n                \"api_key_file\": \"/home/u/.secrets/temur-anthropic-key\" }\n  },\n  \"profile\": \"sonnet\"\n}\n";
        assert_eq!(rendered, expect);
    }

    #[test]
    fn anthropic_render_parses_with_four_profiles_sharing_the_key() {
        let t = &TEMPLATES[1];
        let rendered = render_config(t, "opus", Some("/tmp/k"), None);
        let cfg: crate::config::Config = serde_json::from_str(&rendered).unwrap();
        let profiles = cfg.resolved_profiles().expect("profiles validate");
        assert_eq!(
            profiles.keys().cloned().collect::<Vec<_>>(),
            vec!["fable", "haiku", "opus", "sonnet"],
            "name order"
        );
        for ((name, model_id), (key, resolved)) in ANTHROPIC_PROFILES.iter().zip(&profiles) {
            assert_eq!(key.as_str(), *name);
            assert_eq!(resolved.model, *model_id);
            assert_eq!(resolved.provider, "anthropic");
            assert_eq!(resolved.api_key_file.as_deref(), Some("/tmp/k"), "shared key");
        }
        let (active, resolved) = cfg.startup_selection(&profiles).expect("selection resolves");
        assert_eq!(active.as_deref(), Some("opus"));
        assert_eq!(resolved.model, "claude-opus-5");
    }

    #[test]
    fn anthropic_startup_profile_accepts_name_number_and_default() {
        // Default (empty answer): sonnet, no default flip.
        let (cfg, out) = run_anthropic_wizard("\n");
        assert!(cfg.contains("\"profile\": \"sonnet\""), "{cfg}");
        assert!(out.contains("  1) fable   claude-fable-5"), "{out}");
        assert!(out.contains("  4) sonnet  claude-sonnet-5"), "{out}");
        assert!(out.contains("[sonnet]"), "default shown: {out}");
        // By name.
        let (cfg, _) = run_anthropic_wizard("opus\n");
        assert!(cfg.contains("\"profile\": \"opus\""), "{cfg}");
        // By number (2 = haiku in name order).
        let (cfg, _) = run_anthropic_wizard("2\n");
        assert!(cfg.contains("\"profile\": \"haiku\""), "{cfg}");
    }

    #[test]
    fn anthropic_startup_profile_reasks_on_anything_else() {
        for bad in ["bogus", "9", "claude-opus-5"] {
            let (cfg, out) = run_anthropic_wizard(&format!("{bad}\nfable\n"));
            assert!(
                out.contains(&format!("unknown profile \"{bad}\"")),
                "{bad}: {out}"
            );
            assert!(cfg.contains("\"profile\": \"fable\""), "{bad}: {cfg}");
        }
    }

    // -------------------------------------------------- T17: temur init --add

    /// A byte-fixed local config in exactly the pretty 2-space form
    /// `write_config_value` emits, so a merge's output is predictable to
    /// the byte.
    const LOCAL_FIXED: &str = "{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": {\n    \"model\": \"qwen3-1.7b\",\n    \"context_window\": 8192\n  }\n}\n";

    fn no_listing(_: &str) -> Result<Vec<String>, crate::error::Error> {
        panic!("this template must not attempt a listing")
    }

    /// Write `cfg` into a tempdir and drive `run_add` with piped answers.
    /// Returns (result, config file content, printed output, tempdir).
    fn drive_add(
        cfg: &str,
        template: &str,
        answers: &str,
        list: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
    ) -> (
        Result<(), crate::error::Error>,
        String,
        String,
        tempfile::TempDir,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        std::fs::write(&cfg_path, cfg).unwrap();
        let mut input = std::io::Cursor::new(answers.as_bytes().to_vec());
        let mut out: Vec<u8> = Vec::new();
        let result = run_add(&cfg_path, None, template, &mut input, &mut out, list);
        (
            result,
            std::fs::read_to_string(&cfg_path).unwrap(),
            String::from_utf8(out).unwrap(),
            tmp,
        )
    }

    #[test]
    fn add_anthropic_golden_merge_touches_only_the_profiles_key() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        let (result, cfg, out, _tmp) = drive_add(
            LOCAL_FIXED,
            "anthropic",
            &format!("{}\n", key.display()),
            &no_listing,
        );
        result.unwrap();
        let k = key.display();
        let expect = format!(
            "{{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": {{\n    \"model\": \"qwen3-1.7b\",\n    \"context_window\": 8192\n  }},\n  \"profiles\": {{\n    \"fable\": {{\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-fable-5\",\n      \"api_key_file\": \"{k}\"\n    }},\n    \"haiku\": {{\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-haiku-4-5\",\n      \"api_key_file\": \"{k}\"\n    }},\n    \"opus\": {{\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-opus-5\",\n      \"api_key_file\": \"{k}\"\n    }},\n    \"sonnet\": {{\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-sonnet-5\",\n      \"api_key_file\": \"{k}\"\n    }}\n  }}\n}}\n"
        );
        assert_eq!(cfg, expect, "golden merge: only a profiles key appended");
        // The startup "profile" key was NOT invented: the base selection
        // still runs the config.
        let parsed: crate::config::Config = serde_json::from_str(&cfg).unwrap();
        assert!(parsed.profile.is_none(), "{cfg}");
        let profiles = parsed.resolved_profiles().unwrap();
        let (active, resolved) = parsed.startup_selection(&profiles).unwrap();
        assert!(active.is_none());
        assert_eq!(resolved.model, "qwen3-1.7b", "base selection untouched");
        // Key file created empty, mode 600; closing notice names /model.
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&key).unwrap().len(), 0);
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert!(
            out.contains("Added profiles \"fable\", \"haiku\", \"opus\", \"sonnet\""),
            "{out}"
        );
        assert!(out.contains("/model <name> switches to one"), "{out}");
        assert!(out.contains("Paste your key into"), "{out}");
    }

    #[test]
    fn add_openai_and_gemini_add_one_profile_each() {
        for (template, base, default_model) in [
            ("openai", OPENAI_BASE_URL, "gpt-4o-mini"),
            ("gemini", GEMINI_BASE_URL, "gemini-2.5-flash"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let key = tmp.path().join("k");
            // Answers: model default (empty), explicit key path (home None).
            let (result, cfg, out, _tmp) = drive_add(
                LOCAL_FIXED,
                template,
                &format!("\n{}\n", key.display()),
                &no_listing,
            );
            result.unwrap();
            let parsed: crate::config::Config = serde_json::from_str(&cfg).unwrap();
            let profiles = parsed.resolved_profiles().unwrap();
            let p = &profiles[template];
            assert_eq!(p.provider, "openai-compat", "{template}");
            assert_eq!(p.base_url, base, "{template}");
            assert_eq!(p.model, default_model, "{template}");
            assert_eq!(
                p.api_key_file.as_deref(),
                Some(key.display().to_string().as_str()),
                "{template}"
            );
            assert!(
                out.contains(&format!("Added profile \"{template}\"")),
                "{template}: {out}"
            );
            assert!(
                out.contains(&format!("/model {template} switches to it")),
                "{template}: {out}"
            );
            assert!(
                out.contains(&format!("\"profile\": \"{template}\"")),
                "{template}: {out}"
            );
        }
    }

    #[test]
    fn add_local_reuses_the_picker_and_stays_keyless() {
        let list = |base: &str| {
            assert_eq!(base, "http://10.0.0.9:11434/v1", "picker lists the ANSWERED base");
            Ok(ids(&["served-model"]))
        };
        let anthropic_base = "{\n  \"profiles\": {\n    \"sonnet\": {\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-sonnet-5\",\n      \"api_key_file\": \"/tmp/k\"\n    }\n  },\n  \"profile\": \"sonnet\"\n}\n";
        // Custom base URL, model by number.
        let (result, cfg, out, _tmp) =
            drive_add(anthropic_base, "local", "http://10.0.0.9:11434/v1\n1\n", &list);
        result.unwrap();
        let parsed: crate::config::Config = serde_json::from_str(&cfg).unwrap();
        let profiles = parsed.resolved_profiles().unwrap();
        let p = &profiles["local"];
        assert_eq!(p.provider, "openai-compat");
        assert_eq!(p.base_url, "http://10.0.0.9:11434/v1");
        assert_eq!(p.model, "served-model");
        assert!(p.api_key_file.is_none(), "local stays keyless");
        assert_eq!(p.max_tokens, 4096, "fresh local template's limit carried over");
        assert_eq!(p.context_window, Some(8192));
        // The startup profile key survives untouched.
        assert_eq!(parsed.profile.as_deref(), Some("sonnet"), "{cfg}");
        assert!(!out.contains("API key file"), "no key question: {out}");
        assert!(!out.contains("Paste your key"), "{out}");

        // Default base URL answered: the base_url key is omitted (profile
        // None = the openai-compat default, same meaning as the fresh
        // render's omission).
        let list = |_: &str| Ok(ids(&["served-model"]));
        let (result, cfg, _out, _tmp) = drive_add(anthropic_base, "local", "\n\n", &list);
        result.unwrap();
        assert!(!cfg.contains("base_url"), "{cfg}");
    }

    #[test]
    fn add_collision_fails_closed_naming_every_collision() {
        let cfg_before = "{\n  \"profiles\": {\n    \"opus\": {\n      \"provider\": \"anthropic\",\n      \"model\": \"my-opus\"\n    },\n    \"sonnet\": {\n      \"provider\": \"anthropic\",\n      \"model\": \"my-sonnet\"\n    }\n  }\n}\n";
        let (result, cfg_after, out, _tmp) =
            drive_add(cfg_before, "anthropic", "/never-asked\n", &no_listing);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("\"opus\", \"sonnet\""), "every collision named: {err}");
        assert!(err.contains("nothing was changed"), "{err}");
        assert_eq!(cfg_after, cfg_before, "file untouched on collision");
        assert!(out.is_empty(), "collision detected before any question: {out}");
    }

    #[test]
    fn add_requires_an_existing_config() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let mut input = std::io::Cursor::new(Vec::new());
        let mut out: Vec<u8> = Vec::new();
        let err = run_add(&cfg_path, None, "openai", &mut input, &mut out, &no_listing)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no config at"), "{err}");
        assert!(err.contains("temur init"), "points at the plain wizard: {err}");
        assert!(!cfg_path.exists(), "no config invented");
    }

    #[test]
    fn add_unknown_template_and_broken_config_are_clean_errors() {
        let (result, _cfg, _out, _tmp) =
            drive_add(LOCAL_FIXED, "bogus", "", &no_listing);
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown template \"bogus\"")
                && err.contains("local, anthropic, openai, gemini"),
            "{err}"
        );
        // A "profiles" key that is not an object fails closed.
        let bad = "{\n  \"profiles\": 7\n}\n";
        let (result, cfg_after, _out, _tmp) = drive_add(bad, "openai", "\n/k\n", &no_listing);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("\"profiles\" is not a JSON object"), "{err}");
        assert_eq!(cfg_after, bad);
    }

    #[test]
    fn add_preserves_unknown_fields_and_existing_profiles() {
        let cfg_before = "{\n  \"zeta_unknown\": true,\n  \"provider\": \"openai-compat\",\n  \"openai_compat\": {\n    \"model\": \"m\"\n  },\n  \"profiles\": {\n    \"mine\": {\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-opus-5\"\n    }\n  },\n  \"profile\": \"mine\"\n}\n";
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        let (result, cfg, _out, _tmp) = drive_add(
            cfg_before,
            "openai",
            &format!("\n{}\n", key.display()),
            &no_listing,
        );
        result.unwrap();
        // Key order preserved: the unknown field stays FIRST, the existing
        // profile stays, the new one is appended inside "profiles".
        assert!(cfg.starts_with("{\n  \"zeta_unknown\": true,"), "{cfg}");
        let mine = cfg.find("\"mine\"").unwrap();
        let openai = cfg.find("\"openai\"").unwrap();
        assert!(mine < openai, "existing profile first: {cfg}");
        let parsed: crate::config::Config = serde_json::from_str(&cfg).unwrap();
        assert_eq!(parsed.profile.as_deref(), Some("mine"), "startup key untouched");
        let profiles = parsed.resolved_profiles().unwrap();
        assert_eq!(profiles["mine"].model, "claude-opus-5");
        assert_eq!(profiles["openai"].model, "gpt-4o-mini");
    }
}
