//! Pure recovery helpers for weak models (T4): JSON argument repair and
//! detection of tool calls emitted as plain text. No I/O, no regex, no new
//! dependencies — string-aware scanning only.

use serde_json::Value;

/// A successfully repaired argument object.
#[derive(Debug, Clone, PartialEq)]
pub enum Repaired {
    /// Repairs that cannot change meaning: fence/backtick stripping and
    /// trailing-comma removal. Safe to execute.
    Lossless(Value),
    /// Truncation completion — the result is schema-valid but the model's
    /// intent was cut off, so the value may be semantically wrong. Callers
    /// must NOT execute these.
    Lossy(Value),
}

/// Try to repair a raw argument string that failed to parse as JSON.
/// Transforms are applied in order; the first stage that yields a valid JSON
/// **object** wins. Anything still unparseable — or parseable but not an
/// object — is `None`.
pub fn repair_json(raw: &str) -> Option<Repaired> {
    // Stage 0: plain parse of the trimmed input. Callers only reach here
    // after a parse failure, but trimming alone can fix (BOM-free) padding.
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return object_only(v).map(Repaired::Lossless);
    }
    // Stage 1 (lossless): strip markdown fences / backtick wrapping.
    let unfenced = strip_fences(trimmed);
    if let Ok(v) = serde_json::from_str::<Value>(&unfenced) {
        return object_only(v).map(Repaired::Lossless);
    }
    // Stage 2 (lossless): remove trailing commas before } or ].
    let decommaed = remove_trailing_commas(&unfenced);
    if let Ok(v) = serde_json::from_str::<Value>(&decommaed) {
        return object_only(v).map(Repaired::Lossless);
    }
    // Stage 3 (LOSSY): complete a truncated document — close an open string,
    // then close open brackets (with a trailing-comma cleanup, since a cut
    // directly after a comma would otherwise re-break the parse).
    let completed = complete_truncation(&decommaed)?;
    let completed = remove_trailing_commas(&completed);
    match serde_json::from_str::<Value>(&completed) {
        Ok(v) => object_only(v).map(Repaired::Lossy),
        Err(_) => None,
    }
}

fn object_only(v: Value) -> Option<Value> {
    if v.is_object() {
        Some(v)
    } else {
        None
    }
}

/// Strip a ```lang … ``` fence (info string dropped up to the first
/// newline) or a plain backtick wrapping. Truncated input may lack the
/// closing fence; that is fine.
fn strip_fences(s: &str) -> String {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```") {
        let body = match rest.find('\n') {
            Some(i) => &rest[i + 1..],
            None => rest,
        };
        let body = body.trim_end();
        let body = body.strip_suffix("```").unwrap_or(body);
        return body.trim().to_string();
    }
    s.trim_matches('`').trim().to_string()
}

/// Remove commas that directly precede (modulo whitespace) a closing brace
/// or bracket, outside of strings.
fn remove_trailing_commas(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            ',' => {
                let next = chars[i + 1..].iter().find(|ch| !ch.is_whitespace());
                if !matches!(next, Some('}') | Some(']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Complete a truncated JSON document: track strings and the bracket stack,
/// then append a closing quote (if cut mid-string) and the unclosed
/// brackets in reverse order. `None` when nothing is open (the input's
/// problem is not truncation) or when brackets are mismatched.
fn complete_truncation(s: &str) -> Option<String> {
    let mut stack: Vec<char> = vec![];
    let mut in_string = false;
    let mut escaped = false;
    for c in s.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.pop() != Some(c) {
                    return None;
                }
            }
            _ => {}
        }
    }
    if !in_string && stack.is_empty() {
        return None;
    }
    let mut out = String::from(s);
    if escaped {
        out.pop(); // dangling backslash inside the cut-off string
    }
    if in_string {
        out.push('"');
    }
    while let Some(c) = stack.pop() {
        out.push(c);
    }
    Some(out)
}

/// Detect a tool call the model wrote as plain text instead of invoking the
/// tool interface: known literal markers, or a fenced/leading-brace JSON
/// object that names a REGISTERED tool (the registered-name requirement is
/// the false-positive killer) alongside an arguments-like key.
pub fn detect_text_tool_call(text: &str, tool_names: &[String]) -> bool {
    const MARKERS: [&str; 5] = [
        "<tool_call>",
        "</tool_call>",
        "<function_call>",
        "[TOOL_CALL]",
        "[TOOL_REQUEST]",
    ];
    if MARKERS.iter().any(|m| text.contains(m)) {
        return true;
    }
    let t = text.trim();
    let body = if t.starts_with("```") {
        strip_fences(t)
    } else {
        t.to_string()
    };
    let body = body.trim();
    if !body.starts_with('{') {
        return false;
    }
    // Parse the whole body, or the first balanced object if prose follows.
    let obj = serde_json::from_str::<Value>(body)
        .ok()
        .or_else(|| {
            first_balanced_object(body)
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
        });
    let Some(obj) = obj else {
        return false;
    };
    let named = obj
        .get("name")
        .or_else(|| obj.get("tool"))
        .and_then(|v| v.as_str())
        .is_some_and(|n| tool_names.iter().any(|t| t.as_str() == n));
    let has_args = obj.get("arguments").is_some()
        || obj.get("input").is_some()
        || obj.get("parameters").is_some();
    named && has_args
}

/// The prefix of `s` forming the first balanced JSON object, string-aware.
fn first_balanced_object(s: &str) -> Option<&str> {
    let mut depth: u32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&s[..i + c.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repair_json_table() {
        let cases: Vec<(&str, Option<Repaired>)> = vec![
            // Lossless: fences and backticks.
            (
                "```json\n{\"filePath\": \"a.txt\", \"content\": \"hi\"}\n```",
                Some(Repaired::Lossless(
                    json!({"filePath": "a.txt", "content": "hi"}),
                )),
            ),
            (
                "`{\"command\": \"ls\"}`",
                Some(Repaired::Lossless(json!({"command": "ls"}))),
            ),
            (
                "  {\"a\": 1}  ",
                Some(Repaired::Lossless(json!({"a": 1}))),
            ),
            // Lossless: trailing commas (object and nested array).
            (
                "{\"a\": 1,}",
                Some(Repaired::Lossless(json!({"a": 1}))),
            ),
            (
                "{\"a\": [1, 2,],}",
                Some(Repaired::Lossless(json!({"a": [1, 2]}))),
            ),
            // Lossy: truncation completion (mid-string, mid-object).
            (
                "{\"filePath\": \"x.txt\", \"content\": \"abc",
                Some(Repaired::Lossy(
                    json!({"filePath": "x.txt", "content": "abc"}),
                )),
            ),
            (
                "{\"a\": {\"b\": 1",
                Some(Repaired::Lossy(json!({"a": {"b": 1}}))),
            ),
            (
                "{\"a\": 1,",
                Some(Repaired::Lossy(json!({"a": 1}))),
            ),
            (
                "```json\n{\"cmd\": \"make",
                Some(Repaired::Lossy(json!({"cmd": "make"}))),
            ),
            // None: not truncation, just broken.
            ("{\"filePath\" \"a.txt\"}", None),
            ("{\"a\": oops}", None),
            // None: valid but not an object.
            ("[1, 2, 3]", None),
            ("\"just a string\"", None),
            ("42", None),
            // None: cut before a value could be completed into an object.
            ("{\"filePath\": \"f\", \"conte", None),
            // None: empty / whitespace.
            ("", None),
            ("   ", None),
            // None: mismatched brackets.
            ("{\"a\": [1}", None),
        ];
        for (raw, expected) in cases {
            assert_eq!(repair_json(raw), expected, "raw: {raw:?}");
        }
    }

    #[test]
    fn detect_text_tool_call_table() {
        let tools: Vec<String> = vec!["read".into(), "write".into(), "bash".into()];
        let cases: Vec<(&str, bool)> = vec![
            // Literal markers, regardless of surrounding text.
            ("<tool_call>{\"name\": \"x\"}</tool_call>", true),
            ("I will call\n<function_call>read", true),
            ("[TOOL_CALL] read file", true),
            ("[TOOL_REQUEST] {\"name\": \"bash\"}", true),
            // Fenced JSON naming a registered tool with arguments.
            (
                "```json\n{\"name\": \"read\", \"arguments\": {\"filePath\": \"a\"}}\n```",
                true,
            ),
            // Leading-brace JSON, "tool" + "input" spelling.
            (
                "{\"tool\": \"bash\", \"input\": {\"command\": \"ls\"}}",
                true,
            ),
            // "parameters" spelling, prose after the object.
            (
                "{\"name\": \"write\", \"parameters\": {}} Let me know how it goes.",
                true,
            ),
            // Unregistered name: the false-positive killer.
            (
                "{\"name\": \"compile\", \"arguments\": {}}",
                false,
            ),
            // Registered name but no arguments-like key.
            ("{\"name\": \"read\"}", false),
            // Prose mentioning a tool, no JSON: not a call.
            ("You should use the read tool on a.txt.", false),
            // JSON not at the start (mid-prose): not a call shape we nudge.
            ("Here is the plan: {\"name\": \"read\", \"arguments\": {}}", false),
            // Plain answer.
            ("The answer is 4.", false),
            ("", false),
        ];
        for (text, expected) in cases {
            assert_eq!(
                detect_text_tool_call(text, &tools),
                expected,
                "text: {text:?}"
            );
        }
    }

    #[test]
    fn lossy_never_reported_as_lossless() {
        // The dispatch policy's safety hinges on this distinction: anything
        // that went through truncation completion must be Lossy.
        match repair_json("{\"filePath\": \"x\", \"content\": \"partial tex") {
            Some(Repaired::Lossy(_)) => {}
            other => panic!("expected Lossy, got {other:?}"),
        }
    }
}
