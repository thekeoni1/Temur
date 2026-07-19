//! M6 close-out: STRICT conformance over the frozen live SSE captures
//! (tests/fixtures/live/) and the authored fixtures.
//!
//! Unlike the tolerant runtime parser, this walks every event's JSON against
//! exact per-event key allowlists (required + known-optional), enforces
//! stream-sequence invariants, and separately asserts the runtime parser
//! recognizes every live event (no `Unknown` fallbacks) — so a structural
//! difference is a test failure, never silently absorbed.

use serde_json::Value;
use std::collections::BTreeSet;

fn data_lines(path: &std::path::Path) -> Vec<Value> {
    let text = std::fs::read_to_string(path).unwrap();
    text.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|d| serde_json::from_str::<Value>(d.trim()).unwrap_or_else(|e| panic!("{}: bad JSON: {e}", path.display())))
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

fn check_usage(ctx: &str, u: &Value) -> Result<(), String> {
    check_keys(
        ctx,
        u,
        &["input_tokens", "output_tokens"],
        &[
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
            "cache_creation",
            "service_tier",
            "inference_geo",
            "output_tokens_details",
        ],
    )?;
    if let Some(cc) = u.get("cache_creation").filter(|c| !c.is_null()) {
        check_keys(
            &format!("{ctx}.cache_creation"),
            cc,
            &[],
            &["ephemeral_5m_input_tokens", "ephemeral_1h_input_tokens"],
        )?;
    }
    if let Some(otd) = u.get("output_tokens_details").filter(|c| !c.is_null()) {
        check_keys(&format!("{ctx}.output_tokens_details"), otd, &[], &["thinking_tokens"])?;
    }
    Ok(())
}

fn check_stop_details(ctx: &str, v: &Value) -> Result<(), String> {
    if v.is_null() {
        return Ok(());
    }
    check_keys(ctx, v, &["type"], &["category", "explanation"])
}

fn check_block(ctx: &str, b: &Value) -> Result<(), String> {
    match b["type"].as_str() {
        Some("text") => check_keys(ctx, b, &["type", "text"], &[]),
        Some("tool_use") => {
            check_keys(ctx, b, &["type", "id", "name", "input"], &["caller"])?;
            if let Some(caller) = b.get("caller").filter(|c| !c.is_null()) {
                check_keys(&format!("{ctx}.caller"), caller, &["type"], &[])?;
            }
            Ok(())
        }
        Some("thinking") => check_keys(ctx, b, &["type", "thinking"], &["signature"]),
        other => Err(format!("{ctx}: unexpected content_block type {other:?}")),
    }
}

fn strict_check_event(ctx: &str, v: &Value) -> Result<(), String> {
    match v["type"].as_str() {
        Some("message_start") => {
            check_keys(ctx, v, &["type", "message"], &[])?;
            let m = &v["message"];
            check_keys(
                &format!("{ctx}.message"),
                m,
                &["id", "type", "role", "model", "content", "stop_reason", "stop_sequence", "usage"],
                &["stop_details"],
            )?;
            if m["type"] != "message" || m["role"] != "assistant" {
                return Err(format!("{ctx}.message: bad type/role: {m}"));
            }
            if let Some(sd) = m.get("stop_details") {
                check_stop_details(&format!("{ctx}.message.stop_details"), sd)?;
            }
            check_usage(&format!("{ctx}.message.usage"), &m["usage"])
        }
        Some("content_block_start") => {
            check_keys(ctx, v, &["type", "index", "content_block"], &[])?;
            check_block(&format!("{ctx}.content_block"), &v["content_block"])
        }
        Some("content_block_delta") => {
            check_keys(ctx, v, &["type", "index", "delta"], &[])?;
            let d = &v["delta"];
            let dctx = format!("{ctx}.delta");
            match d["type"].as_str() {
                Some("text_delta") => check_keys(&dctx, d, &["type", "text"], &[]),
                Some("input_json_delta") => check_keys(&dctx, d, &["type", "partial_json"], &[]),
                Some("thinking_delta") => check_keys(&dctx, d, &["type", "thinking"], &[]),
                Some("signature_delta") => check_keys(&dctx, d, &["type", "signature"], &[]),
                other => Err(format!("{dctx}: unexpected delta type {other:?}")),
            }
        }
        Some("content_block_stop") => check_keys(ctx, v, &["type", "index"], &[]),
        Some("message_delta") => {
            check_keys(ctx, v, &["type", "delta", "usage"], &[])?;
            let d = &v["delta"];
            check_keys(
                &format!("{ctx}.delta"),
                d,
                &["stop_reason", "stop_sequence"],
                &["stop_details"],
            )?;
            if let Some(sd) = d.get("stop_details") {
                check_stop_details(&format!("{ctx}.delta.stop_details"), sd)?;
            }
            // message_delta usage: output_tokens always; the rest optional.
            check_keys(
                &format!("{ctx}.usage"),
                &v["usage"],
                &["output_tokens"],
                &[
                    "input_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                    "cache_creation",
                    "service_tier",
                    "inference_geo",
                    "output_tokens_details",
                ],
            )?;
            if let Some(otd) = v["usage"].get("output_tokens_details").filter(|c| !c.is_null()) {
                check_keys(&format!("{ctx}.usage.output_tokens_details"), otd, &[], &["thinking_tokens"])?;
            }
            Ok(())
        }
        Some("message_stop") => check_keys(ctx, v, &["type"], &[]),
        Some("ping") => check_keys(ctx, v, &["type"], &[]),
        Some("error") => {
            check_keys(ctx, v, &["type", "error"], &["request_id"])?;
            check_keys(&format!("{ctx}.error"), &v["error"], &["type", "message"], &[])
        }
        other => Err(format!("{ctx}: unexpected event type {other:?}")),
    }
}

/// Stream-level invariants (skipped after a mid-stream `error` event).
fn check_sequence(path: &str, events: &[Value]) -> Result<(), String> {
    if events.iter().any(|e| e["type"] == "error") {
        return Ok(());
    }
    if events.first().map(|e| &e["type"]) != Some(&Value::from("message_start")) {
        return Err(format!("{path}: stream must begin with message_start"));
    }
    if events.last().map(|e| &e["type"]) != Some(&Value::from("message_stop")) {
        return Err(format!("{path}: stream must end with message_stop"));
    }
    let delta_positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e["type"] == "message_delta")
        .map(|(i, _)| i)
        .collect();
    if delta_positions.len() != 1 || delta_positions[0] != events.len() - 2 {
        return Err(format!(
            "{path}: expected exactly one message_delta immediately before message_stop"
        ));
    }
    // Block bookkeeping: deltas only against an open block of matching type.
    let mut open: std::collections::HashMap<u64, String> = Default::default();
    for (i, e) in events.iter().enumerate() {
        match e["type"].as_str().unwrap() {
            "content_block_start" => {
                let idx = e["index"].as_u64().unwrap();
                if open.contains_key(&idx) {
                    return Err(format!("{path}[{i}]: block {idx} started twice"));
                }
                open.insert(idx, e["content_block"]["type"].as_str().unwrap().to_string());
            }
            "content_block_stop" => {
                let idx = e["index"].as_u64().unwrap();
                if open.remove(&idx).is_none() {
                    return Err(format!("{path}[{i}]: stop for unopened block {idx}"));
                }
            }
            "content_block_delta" => {
                let idx = e["index"].as_u64().unwrap();
                let Some(block_type) = open.get(&idx) else {
                    return Err(format!("{path}[{i}]: delta for unopened block {idx}"));
                };
                let ok = matches!(
                    (block_type.as_str(), e["delta"]["type"].as_str().unwrap()),
                    ("text", "text_delta")
                        | ("tool_use", "input_json_delta")
                        | ("thinking", "thinking_delta")
                        | ("thinking", "signature_delta")
                );
                if !ok {
                    return Err(format!(
                        "{path}[{i}]: {} delta on {} block",
                        e["delta"]["type"], block_type
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn strict_check_file(path: &std::path::Path) -> Vec<String> {
    let events = data_lines(path);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let mut failures: Vec<String> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| strict_check_event(&format!("{name}[{i}]"), e).err())
        .collect();
    if let Err(e) = check_sequence(&name, &events) {
        failures.push(e);
    }
    failures
}

fn live_files() -> Vec<std::path::PathBuf> {
    let dir = format!("{}/tests/fixtures/live", env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/fixtures/live must exist (frozen captures)")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "sse").unwrap_or(false))
        .collect();
    files.sort();
    files
}

#[test]
fn live_captures_are_strictly_conformant() {
    let files = live_files();
    assert!(
        files.len() >= 8,
        "expected the 8 frozen Tier-1 captures, found {}",
        files.len()
    );
    let mut all: Vec<String> = vec![];
    for f in &files {
        all.extend(strict_check_file(f));
    }
    assert!(all.is_empty(), "strict conformance failures:\n{}", all.join("\n"));
}

#[test]
fn authored_fixtures_are_strictly_conformant() {
    // unknown_tolerance is deliberately non-conformant (it tests tolerance).
    let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut all: Vec<String> = vec![];
    for entry in std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()) {
        let p = entry.path();
        if p.extension().map(|x| x == "sse").unwrap_or(false)
            && !p.to_string_lossy().contains("unknown_tolerance")
        {
            all.extend(strict_check_file(&p));
        }
    }
    assert!(all.is_empty(), "strict conformance failures:\n{}", all.join("\n"));
}

#[test]
fn runtime_parser_fully_recognizes_live_streams() {
    use opencode_rust::provider::anthropic::sse::SseReader;
    use opencode_rust::provider::anthropic::types::*;
    for f in live_files() {
        let file = std::fs::File::open(&f).unwrap();
        let mut acc = MessageAccumulator::new();
        let mut count = 0;
        for item in SseReader::new(std::io::BufReader::new(file)) {
            let ev = item.unwrap_or_else(|e| panic!("{}: {e}", f.display()));
            assert!(
                !matches!(ev, SseEvent::Unknown),
                "{}: runtime parser fell back to Unknown — tolerance hid something",
                f.display()
            );
            if let SseEvent::ContentBlockDelta { delta, .. } = &ev {
                assert!(
                    !matches!(delta, Delta::Unknown),
                    "{}: unknown delta type absorbed",
                    f.display()
                );
            }
            if let SseEvent::ContentBlockStart { content_block, .. } = &ev {
                assert!(
                    !matches!(content_block, ContentBlock::Unknown),
                    "{}: unknown block type absorbed",
                    f.display()
                );
            }
            acc.push(&ev);
            count += 1;
        }
        assert!(count >= 5, "{}: suspiciously short stream", f.display());
        assert!(acc.error.is_none());
        let msg = acc.into_message().expect("complete message");
        let stop = msg.stop_reason.expect("stop reason");
        assert!(
            matches!(stop, StopReason::ToolUse | StopReason::EndTurn),
            "{}: unexpected stop reason {stop:?}",
            f.display()
        );
        // Completed tool_use blocks must carry fully-parsed JSON object inputs.
        if stop == StopReason::ToolUse {
            for b in &msg.content {
                if let ContentBlock::ToolUse { input, name, .. } = b {
                    assert!(input.is_object() && !input.as_object().unwrap().is_empty(),
                        "{}: tool {name} input not assembled: {input}", f.display());
                }
            }
        }
    }
}
