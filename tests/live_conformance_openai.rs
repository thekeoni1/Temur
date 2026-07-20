//! T4 close-out: STRICT conformance over the frozen live OpenAI-compat SSE
//! captures in tests/fixtures/live-openai/ — taken against llama.cpp
//! server-b10068 serving a small local model (Qwen3-class), with and
//! without --jinja (the nojinja files; that build emitted structured
//! tool_calls either way).
//!
//! Same discipline as tests/live_conformance.rs: the runtime parser is
//! tolerant, this walker is not. Exact per-chunk key allowlists DERIVED
//! from the actual frozen captures, stream-sequence invariants, and a
//! zero-tolerance assembler pass — a structural difference is a test
//! failure, never silently absorbed.

use serde_json::Value;
use std::collections::BTreeSet;

fn live_files() -> Vec<std::path::PathBuf> {
    let dir = format!("{}/tests/fixtures/live-openai", env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/fixtures/live-openai must exist (frozen captures)")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "sse").unwrap_or(false))
        .collect();
    files.sort();
    files
}

/// Frame-level read: every non-blank line is a `data: ` line, the terminal
/// frame is exactly `[DONE]`, and nothing follows it.
fn chunks(path: &std::path::Path) -> Vec<Value> {
    let text = std::fs::read_to_string(path).unwrap();
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let mut payloads: Vec<&str> = vec![];
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let data = line
            .strip_prefix("data: ")
            .unwrap_or_else(|| panic!("{name}:{}: non-data line: {line:?}", i + 1));
        payloads.push(data.trim());
    }
    assert!(!payloads.is_empty(), "{name}: empty capture");
    assert_eq!(
        payloads.last().copied(),
        Some("[DONE]"),
        "{name}: stream must terminate with [DONE]"
    );
    let done_count = payloads.iter().filter(|p| **p == "[DONE]").count();
    assert_eq!(done_count, 1, "{name}: exactly one [DONE]");
    payloads[..payloads.len() - 1]
        .iter()
        .map(|d| serde_json::from_str::<Value>(d).unwrap_or_else(|e| panic!("{name}: bad JSON: {e}")))
        .collect()
}

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// Exact-key check: every required key present, no key outside required∪optional.
fn check_keys(ctx: &str, v: &Value, required: &[&str], optional: &[&str]) -> Result<(), String> {
    let got = keys(v);
    for r in required {
        if !got.contains(*r) {
            return Err(format!("{ctx}: missing required key '{r}' (got {got:?})"));
        }
    }
    let allowed: BTreeSet<&str> = required.iter().chain(optional).copied().collect();
    for k in &got {
        if !allowed.contains(k.as_str()) {
            return Err(format!("{ctx}: UNEXPECTED key '{k}' (allowed: {allowed:?})"));
        }
    }
    Ok(())
}

fn check_delta(ctx: &str, d: &Value) -> Result<(), String> {
    // Delta variants seen in the captures: {role, content:null} (opening),
    // {content}, {reasoning_content} (llama.cpp thinking stream),
    // {tool_calls}, and {} (the finish chunk).
    check_keys(ctx, d, &[], &["role", "content", "reasoning_content", "tool_calls"])?;
    if let Some(role) = d.get("role").filter(|r| !r.is_null()) {
        if role != "assistant" {
            return Err(format!("{ctx}: unexpected role {role}"));
        }
    }
    if let Some(calls) = d.get("tool_calls") {
        let arr = calls
            .as_array()
            .ok_or_else(|| format!("{ctx}: tool_calls not an array"))?;
        for (i, c) in arr.iter().enumerate() {
            let cctx = format!("{ctx}.tool_calls[{i}]");
            check_keys(&cctx, c, &["index", "function"], &["id", "type"])?;
            if let Some(t) = c.get("type") {
                if t != "function" {
                    return Err(format!("{cctx}: unexpected type {t}"));
                }
            }
            check_keys(&format!("{cctx}.function"), &c["function"], &[], &["name", "arguments"])?;
        }
    }
    Ok(())
}

fn check_usage(ctx: &str, u: &Value) -> Result<(), String> {
    check_keys(
        ctx,
        u,
        &["completion_tokens", "prompt_tokens", "total_tokens"],
        &["prompt_tokens_details"],
    )?;
    if let Some(d) = u.get("prompt_tokens_details").filter(|d| !d.is_null()) {
        check_keys(&format!("{ctx}.prompt_tokens_details"), d, &[], &["cached_tokens"])?;
    }
    Ok(())
}

/// llama.cpp's timing extension on the final chunk — enumerated exactly so
/// a new field is noticed, not absorbed.
fn check_timings(ctx: &str, t: &Value) -> Result<(), String> {
    check_keys(
        ctx,
        t,
        &[],
        &[
            "cache_n",
            "prompt_n",
            "prompt_ms",
            "prompt_per_token_ms",
            "prompt_per_second",
            "predicted_n",
            "predicted_ms",
            "predicted_per_token_ms",
            "predicted_per_second",
        ],
    )
}

fn strict_check_chunk(ctx: &str, v: &Value) -> Result<(), String> {
    check_keys(
        ctx,
        v,
        &["choices", "created", "id", "model", "system_fingerprint", "object"],
        &["usage", "timings"],
    )?;
    if v["object"] != "chat.completion.chunk" {
        return Err(format!("{ctx}: object is {}", v["object"]));
    }
    let choices = v["choices"]
        .as_array()
        .ok_or_else(|| format!("{ctx}: choices not an array"))?;
    if choices.is_empty() {
        // Usage-only final chunk.
        check_usage(&format!("{ctx}.usage"), &v["usage"])?;
        if let Some(t) = v.get("timings") {
            check_timings(&format!("{ctx}.timings"), t)?;
        }
        return Ok(());
    }
    if choices.len() != 1 {
        return Err(format!("{ctx}: {} choices (expected 1)", choices.len()));
    }
    let c = &choices[0];
    check_keys(&format!("{ctx}.choices[0]"), c, &["finish_reason", "index", "delta"], &[])?;
    if c["index"] != 0 {
        return Err(format!("{ctx}: choice index {}", c["index"]));
    }
    check_delta(&format!("{ctx}.delta"), &c["delta"])
}

/// Stream-level invariants over one capture.
fn check_sequence(name: &str, chunks: &[Value]) -> Result<(), String> {
    // First chunk opens with role: assistant.
    let first = chunks
        .first()
        .ok_or_else(|| format!("{name}: no chunks"))?;
    if first["choices"][0]["delta"]["role"] != "assistant" {
        return Err(format!("{name}: first chunk must carry role assistant"));
    }
    // Exactly one non-null finish_reason; only the empty-choices usage
    // chunk may follow it.
    let finish_positions: Vec<usize> = chunks
        .iter()
        .enumerate()
        .filter(|(_, v)| {
            v["choices"]
                .as_array()
                .and_then(|a| a.first())
                .map(|c| !c["finish_reason"].is_null())
                .unwrap_or(false)
        })
        .map(|(i, _)| i)
        .collect();
    if finish_positions.len() != 1 {
        return Err(format!(
            "{name}: expected exactly one finish_reason, got {}",
            finish_positions.len()
        ));
    }
    for (i, v) in chunks.iter().enumerate().skip(finish_positions[0] + 1) {
        if v["choices"].as_array().map(|a| !a.is_empty()).unwrap_or(true) {
            return Err(format!("{name}[{i}]: content chunk after finish_reason"));
        }
    }
    // One id/model per stream.
    let ids: BTreeSet<&str> = chunks.iter().filter_map(|v| v["id"].as_str()).collect();
    if ids.len() != 1 {
        return Err(format!("{name}: {} distinct chunk ids", ids.len()));
    }
    // Tool-call fragments: the first fragment of each index carries
    // id + type + function.name; later fragments only append arguments.
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    for (i, v) in chunks.iter().enumerate() {
        let Some(calls) = v["choices"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|c| c["delta"]["tool_calls"].as_array())
        else {
            continue;
        };
        for c in calls {
            let idx = c["index"]
                .as_u64()
                .ok_or_else(|| format!("{name}[{i}]: tool_call without numeric index"))?;
            if seen.insert(idx) {
                if c["id"].as_str().map(str::is_empty).unwrap_or(true)
                    || c["type"] != "function"
                    || c["function"]["name"].as_str().map(str::is_empty).unwrap_or(true)
                {
                    return Err(format!(
                        "{name}[{i}]: opening fragment of call {idx} lacks id/type/name"
                    ));
                }
            }
        }
    }
    // Exactly one usage chunk, terminal (immediately before [DONE]).
    let usage_positions: Vec<usize> = chunks
        .iter()
        .enumerate()
        .filter(|(_, v)| v["choices"].as_array().map(|a| a.is_empty()).unwrap_or(false))
        .map(|(i, _)| i)
        .collect();
    if usage_positions != vec![chunks.len() - 1] {
        return Err(format!(
            "{name}: expected exactly one terminal usage chunk, found at {usage_positions:?}"
        ));
    }
    Ok(())
}

#[test]
fn live_openai_captures_are_strictly_conformant() {
    let files = live_files();
    assert!(
        files.len() >= 14,
        "expected the 14 frozen T4 captures (11 jinja + 3 nojinja), found {}",
        files.len()
    );
    let mut all: Vec<String> = vec![];
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let cs = chunks(f);
        for (i, c) in cs.iter().enumerate() {
            if let Err(e) = strict_check_chunk(&format!("{name}[{i}]"), c) {
                all.push(e);
            }
        }
        if let Err(e) = check_sequence(&name, &cs) {
            all.push(e);
        }
    }
    assert!(all.is_empty(), "strict conformance failures:\n{}", all.join("\n"));
}

#[test]
fn runtime_assembler_fully_consumes_live_streams() {
    // Zero-tolerance assembler pass: every capture assembles into a
    // complete neutral message with a stop reason, and every tool call
    // carries either fully-parsed JSON-object arguments or (never in these
    // captures, but the contract) input_raw.
    use temur::provider::openai_compat::types::{Chunk, ChunkAccumulator};
    use temur::provider::sse::SseFrames;
    use temur::provider::types::{ContentBlock, StopReason};

    for f in live_files() {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let file = std::fs::File::open(&f).unwrap();
        let mut acc = ChunkAccumulator::new();
        let mut count = 0u32;
        for frame in SseFrames::new(std::io::BufReader::new(file)) {
            let data = frame.unwrap_or_else(|e| panic!("{name}: frame error: {e}"));
            if data.trim() == "[DONE]" {
                break;
            }
            let chunk: Chunk =
                serde_json::from_str(&data).unwrap_or_else(|e| panic!("{name}: chunk parse: {e}"));
            acc.push(&chunk, &mut |_| {});
            count += 1;
        }
        assert!(count >= 3, "{name}: suspiciously short stream");
        assert!(acc.error.is_none(), "{name}: unexpected error envelope");
        let msg = acc
            .into_message("fallback-model")
            .unwrap_or_else(|| panic!("{name}: no message assembled"));
        let stop = msg
            .stop_reason
            .unwrap_or_else(|| panic!("{name}: missing stop reason"));
        assert!(
            matches!(stop, StopReason::EndTurn | StopReason::ToolUse),
            "{name}: unexpected stop reason {stop:?}"
        );
        assert!(!msg.content.is_empty(), "{name}: empty assembled content");
        let mut tool_calls = 0;
        for b in &msg.content {
            if let ContentBlock::ToolUse {
                name: tool,
                input,
                input_raw,
                ..
            } = b
            {
                tool_calls += 1;
                let parsed_ok = input.is_object()
                    && !input.as_object().unwrap().is_empty()
                    && input_raw.is_none();
                let raw_preserved = input == &serde_json::json!({}) && input_raw.is_some();
                assert!(
                    parsed_ok || raw_preserved,
                    "{name}: tool {tool} arguments neither parsed nor preserved: {input} / {input_raw:?}"
                );
            }
        }
        if stop == StopReason::ToolUse {
            assert!(tool_calls > 0, "{name}: ToolUse stop without tool calls");
        }
    }
}
