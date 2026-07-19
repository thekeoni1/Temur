# opencode-rust — Implementation Plan (v1)

Companion to `ROADMAP.md`. Operates under `CLAUDE.md` (auth split, secret-by-path,
fixture-only testing here, rustls, read-only reference). Reference implementation
studied at `/home/dev/reference/opencode` (sst/opencode v1.2.25, read-only).

## 1. Module layout (single binary crate)

```
Cargo.toml
src/
  main.rs              arg parsing (lexopt), config load, wiring, REPL entry
  config.rs            JSON config + defaults; secret path from APP_SECRET_FILE
  error.rs             top-level error enum (thiserror)
  secret.rs            read credential by path at startup; never logged/echoed
  provider/
    mod.rs             Provider trait; neutral ChatRequest, StreamEvent, Completion
    anthropic/
      mod.rs           AnthropicProvider: request build, headers, retry, stream drive
      types.rs         Messages API wire types (serde): content blocks (text,
                       thinking, tool_use, tool_result), stop reasons (end_turn,
                       tool_use, max_tokens, refusal, pause_turn,
                       model_context_window_exceeded), usage, error envelope
      sse.rs           incremental SSE parser: bytes → typed stream events
      transport.rs     Transport trait: ureq HTTPS impl + fixture-file impl (tests)
  agent/
    mod.rs             Session: history, turn loop, tool dispatch, guards, usage
    events.rs          AgentEvent enum consumed by the UI
  tools/
    mod.rs             Tool trait + Registry
    read.rs write.rs edit.rs bash.rs glob.rs grep.rs todo.rs
    prompts/*.txt      ported near-verbatim from OpenCode tool/*.txt (include_str!)
  ui/
    mod.rs             Ui trait (render AgentEvent, prompt for input)
    repl.rs            streaming line REPL (std stdin/stdout)
tests/
  fixtures/*.sse       captured/hand-built SSE payloads
  sse_parser.rs        fixture-driven parser tests
  provider.rs          request-shape + stream-assembly tests via fixture transport
  agent_loop.rs        MockProvider scripted-turn tests
scripts/
  check.sh             cargo build+test (i686) + container exercise (podman)
docs/RUNBOOK.md        operator steps: install, secret injection, live smoke (M6)
```

Key trait shapes (contracts, not code yet):

```
trait Provider {
    fn stream(&self, req: &ChatRequest,
              on_event: &mut dyn FnMut(StreamEvent)) -> Result<Completion, ProviderError>;
}
trait Tool {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;      // ported prompt text
    fn input_schema(&self) -> serde_json::Value; // JSON Schema
    fn execute(&self, input: serde_json::Value, ctx: &mut ToolCtx)
        -> Result<ToolOutput, ToolError>;        // errors → tool_result is_error:true
}
trait Ui {
    fn event(&mut self, ev: &AgentEvent);
    fn read_input(&mut self) -> Option<String>;  // None = EOF/quit
}
```

## 2. Agent loop (ported control flow)

Modeled on OpenCode `session/processor.ts`, mapped to native Anthropic SSE events:

1. Push user message; call `provider.stream()`.
2. Stream events → UI (text deltas live; tool inputs accumulated from
   `input_json_delta`; thinking summaries shown when present).
3. On `message_delta`/completion, branch on stop reason:
   - `tool_use` → execute every requested tool (validating input against schema;
     schema-violation and execution errors become `tool_result` with
     `is_error: true`, never a crash), append the assistant content + **one** user
     message containing **all** tool_results, loop.
   - `pause_turn` → re-send with the assistant content appended; loop.
   - `end_turn` → return control to REPL.
   - `max_tokens` / `model_context_window_exceeded` / `refusal` → surface clearly;
     don't auto-retry the same prompt.
4. Guards: doom-loop detector (3 identical consecutive tool calls → stop and tell
   the user, mirroring OpenCode's threshold) and a max-iterations-per-turn cap (~50).
5. Usage: accumulate `usage` fields from `message_start`/`message_delta`; REPL shows
   per-turn and session totals. No local tokenizer (API-reported only — also avoids
   a 32-bit-unfriendly dependency).

Tool behaviors port OpenCode's semantics: `read` (1-indexed offset/limit, 2000-line
default, per-line truncation at 2000 chars, 50 KB byte cap, extension+content binary
detection, directory listing mode); `write`; `edit` (exact unique match, `replace_all`
flag — OpenCode's fuzzy fallback matchers deferred); `bash` (sh -c, configurable
timeout, combined stdout/stderr, output truncation); `glob`/`grep` (gitignore-aware);
`todowrite`/`todoread` (in-memory session list, JSON echo). Central output truncation
in the registry wrapper, like OpenCode's `Tool.define`.

## 3. Provider details (Anthropic, Messages API)

- `POST {base_url}/v1/messages`, `stream: true`; headers `x-api-key` (from secret),
  `anthropic-version: 2023-06-01`, `content-type: application/json`.
- Default model **`claude-sonnet-5`** (Sonnet-class by default — the loop is chatty
  and runs on a metered key; Opus is a one-line config change, not the default).
  `max_tokens` default 32000 (streaming, so safe). **Thinking OFF by default for v1**
  (cheapest, most legible round-trips while the loop is brought up); the wire types
  and SSE parser support thinking blocks from M1, so enabling adaptive thinking later
  is a config flip (`thinking: adaptive`), not a refactor. Thinking blocks, when
  enabled, are echoed back verbatim in history.
- Static prompt-caching breakpoint: `cache_control: {"type":"ephemeral"}` on the last
  system block (tools+system cached together); deterministic tool order.
- SSE parser handles: `message_start`, `content_block_start/stop`,
  `content_block_delta` (`text_delta`, `input_json_delta`, `thinking_delta`),
  `message_delta` (stop_reason, usage), `message_stop`, `ping`, `error`.
- Retry: 429 (honor `retry-after`), 408/5xx/connection errors — exponential backoff,
  2 retries; other 4xx never retried. API error envelope surfaced with type+message.
- Base URL configurable (enables a future local mock endpoint and provider testing).
- **Secret handling**: the key is read from the file named by `APP_SECRET_FILE` at
  startup, trimmed, held in memory, used only in the header. The product deliberately
  does **not** read `ANTHROPIC_API_KEY` (per CLAUDE.md, to keep builder/product auth
  from ever cross-contaminating). Key never logged, echoed, or passed via argv.

## 4. Dependencies (with 32-bit / rustls rationale)

| Crate | Purpose | Rationale |
|---|---|---|
| `ureq` (rustls) | Blocking HTTPS | Single-user terminal agent needs one stream at a time; avoids tokio/async runtime on 32-bit (smaller binary, fewer deps). SSE parses naturally from a blocking `Read`. |
| `rustls` + **`ring`** provider | TLS | Pure Rust per constraint. Explicitly select the `ring` crypto provider: the newer default `aws-lc-rs` needs cmake + a C build and is riskier on i686; `ring` has mature i686 assembly support. |
| `webpki-roots` | CA roots | Baked-in Mozilla roots — no dependency on OS cert stores (the bare `i386/debian` runtime image has no `ca-certificates` package). |

**Prove-it gate (M0):** ring-on-i686 and the webpki-roots handshake are validated, not
assumed. M0 ships a `tls-probe` check — ureq+rustls(ring)+webpki-roots completing a real
TLS handshake against a neutral public endpoint (e.g. crates.io; **not** the Anthropic
API, which stays untouched from this session) — run as i686 on the host **and** inside
the container. Any ring i686 build issue gets surfaced immediately, with fallback
options evaluated then (pinning versions, or rustls' other pure-Rust providers), before
any provider code is written.
| `serde`, `serde_json` | JSON | Settled. |
| `thiserror` | Error types | Lightweight derive. |
| `log` + `env_logger` | Logging | Pure Rust; stderr, off by default. |
| `regex`, `globset`, `walkdir`, `ignore` | grep/glob tools | ripgrep's pure-Rust building blocks; gitignore-aware walking. |
| `wait-timeout` | bash tool | Child-process timeout on a blocking `Child`. |
| `lexopt` | CLI args | Tiny; clap deferred until the surface grows. |

32-bit discipline: file sizes/offsets/byte counts are `u64` (never `usize`); reads are
capped (read tool 50 KB, bash output truncation) so no large-allocation assumptions;
`scripts/check.sh` asserts `cargo tree -i openssl-sys` resolves to nothing.

## 5. Test strategy (fixtures only in this session)

- **SSE fixtures** (`tests/fixtures/*.sse`) — provenance in three layers:
  1. *Hand-authored from the current Messages API streaming reference*, enumerating:
     `message_start`, `content_block_start` (text / `tool_use` / thinking),
     `content_block_delta` (`text_delta`, `input_json_delta`, `thinking_delta`),
     `content_block_stop`, `message_delta` (stop_reason + cumulative usage),
     `message_stop`, interleaved `ping`, mid-stream `error`; stop reasons `end_turn`,
     `tool_use` (incl. parallel blocks), `max_tokens`,
     `model_context_window_exceeded`, `pause_turn`, and `refusal` (pre-output empty
     content AND mid-stream partial).
  2. *Cross-checked in M1 against Anthropic's official SDK test suites*
     (anthropic-sdk-python / -typescript streaming test fixtures, fetched read-only)
     for event ordering and field shapes; divergences resolve in favor of the SDK
     fixtures.
  3. *One-time live capture at M6*: before the Tier-1 smoke the operator runs
     `--capture-sse` (or a `curl -N` fallback) recording one real tool-use turn's SSE
     body (bodies carry no credentials — the key exists only in a request header,
     never written). The transcript is frozen into `tests/fixtures/live/` and a
     conformance test replays the parser over it, anchoring the offline suite to a
     real wire capture and turning future API drift into an offline test failure.
  Parser tests assert the exact event sequence and the assembled final message.
  **Structural mitigation**: production types tolerate unknown event types and JSON
  fields (log + skip, never fatal), per Anthropic's versioning policy; a strict mode
  used only over the live capture flags unknown fields so drift is detected without
  being fatal. `pause_turn`/`refusal` parsing is an M1 exit criterion; their loop
  semantics (resume / surface-and-stop) are an M4 exit criterion — not deferred.

### Status after M6 close-out (2026-07-03) — fixture provenance, as landed

- Layer 3 is DONE: 8 live SSE captures from the Tier-1 smoke are frozen in
  `tests/fixtures/live/`, with a strict conformance suite
  (`tests/live_conformance.rs`) that walks exact per-event key allowlists, enforces
  stream-sequence invariants, and asserts the runtime parser produces zero `Unknown`
  fallbacks over the live files. It runs in `check.sh` on host and in the container.
- The authored fixtures were **enriched to the live wire shape** during close-out
  (full cumulative `message_delta` usage incl. `output_tokens_details`; nested
  `cache_creation`, `service_tier`, `inference_geo` in `message_start` usage;
  explicit `stop_details: null`; `caller` on tool_use blocks). The live
  reconciliation required **no runtime code changes** — fixtures/tests only.
- **KNOWN GAP — offline-correct but NOT live-verified:** `pause_turn` and `refusal`
  never occurred in the Tier-1 smoke. Their coverage is docs + official-SDK-fixture
  provenance only (the refusal shape incl. `stop_details` is SDK-fixture-confirmed).
  A future session should not assume these are live-confirmed. A refusal is cheap for
  an operator to elicit deliberately if live confirmation is wanted; `pause_turn`
  effectively requires server-side tools v1 doesn't use.
- **Transport seam**: `Transport` trait so provider tests run the full
  request→stream→completion path against fixture files; the ureq impl is the only
  code not covered offline (exercised in the live Tier-1 handoff).
- **MockProvider** (implements `Provider`, scripted): agent-loop tests for
  tool round-trips, one-user-message-per-batch tool_results, doom-loop guard,
  pause_turn resume, usage accumulation.
- **Tool tests**: temp dirs (std `tempdir` pattern under `/tmp`, native ext4);
  cover truncation caps, binary detection, edit uniqueness errors, bash timeout.
- **Every milestone**: `scripts/check.sh` = `cargo build --target i686… && cargo test
  --target i686…` on the host, then `podman run --rm -v /home/dev/rustcode-target/...`
  in `i386/debian:stable` running the binary's offline self-check / `--mock` replay.
  Tests run as 32-bit binaries in both places — "what we ship is what we test".
- **No live Anthropic calls from this session, ever** — enforced by simply having no
  credential: the builder cannot read `/srv/rustcode-secrets/credential`, and dummy
  key files used in container smoke runs only exercise non-network paths (mock mode).

## 6. Live-verification handoff (appsvc, human-triggered)

A deliberate security consequence discovered in setup: `dev` **cannot and must not**
write to `/srv/rustcode-runtime/bin` — if the builder could replace the binary that
`appsvc` executes, it could trivially exfiltrate the secret, nullifying the boundary.
Deployment is therefore operator-mediated, like secret injection:

1. **Builder (dev)**: `cargo build --release --target i686-unknown-linux-gnu`; copy to
   staging `~/dist/opencode-rust` (ext4); record sha256 in `docs/RUNBOOK.md`.
2. **Operator (root, `wsl -d Ubuntu -u root`)**:
   `install -o appsvc -g appsvc -m 755 /home/dev/dist/opencode-rust /srv/rustcode-runtime/bin/app`
   and inject the real credential per `SETUP.md` (if not already done).
3. **Operator runs Tier-1 smoke**: `runuser -u appsvc -- /srv/rustcode-runtime/run-app.sh`
   (launcher exports `APP_SECRET_FILE`; binary runs natively — it's i686 ELF on the
   multilib host). RUNBOOK provides the scripted smoke prompts: read a file, run a
   shell command, edit/write a file, one coherent streamed answer with ≥1 tool
   round-trip — plus expected-output checklist.
4. Results (transcript + exit status) come back to the builder for triage; the
   builder never sees or handles the credential.

Container-hosted *live* runs (secret mounted into `i386/debian`) are a later
refinement; v1 acceptance runs on the host as `appsvc`, while the container remains
the build-validation environment.

## 7. The CLI-workflow reference task (REFRAMED 2026-07-03 — no longer an acceptance gate)

Per the revised framing in `ROADMAP.md`: opencode-rust is a general OpenCode clone
for 32-bit Linux, and the reference workflow (driving an external tabular-data CLI to
reproduce a report as an XLSX file) is **one useful end-to-end test among many**, run
whenever its inputs are available — not a blocking milestone. Nothing waits on it.
Mechanically it still plugs in as: a task prompt + the existing `bash` tool (to invoke
the external CLI) + `read`/`write`; if a dedicated skill/prompt-injection mechanism is
wanted, it lands as a config-loaded system-prompt fragment, not task logic in the core.
