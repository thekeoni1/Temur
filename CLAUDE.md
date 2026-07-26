# CLAUDE.md: build-agent working rules

You are building a Rust CLI application (referred to here as "the product") that,
among other things, calls the Anthropic API. Read these rules before acting. They
encode hard constraints; when in doubt, stop and ask rather than guess.

You run as the non-root user `dev`. This is deliberate: some boundaries below are
enforced by the OS, not just by these rules. Do not try to escalate around them.

## Two separate auth identities: never cross them
- **You (this session, the builder)** authenticate via Anthropic account/subscription
  credentials only.
- **The product** authenticates with its own API key, injected at runtime by a human,
  which you never see, set, or handle.
- `ANTHROPIC_API_KEY` must **never** be set in this session's environment. If that
  variable ever appears in your shell, **stop and flag it**. Do not proceed. A key in
  your env silently redirects your own calls and is a leak.
- Never write, echo, print, log, hardcode, or commit an API key anywhere, in any file,
  script, comment, test fixture, or commit message.

## The runtime secret
- The product's credential lives only at `/srv/rustcode-secrets/credential`, owned by
  `appsvc`. **You (`dev`) cannot read it, and that is intentional**. Do not attempt to,
  and do not try to work around the permission boundary. It is currently a placeholder;
  the real value is injected by a human later.
- The product reads it **by path at startup** (via `APP_SECRET_FILE`), never as a build
  input, never as a CLI argument. Your build and tests must not depend on its contents.
- Nothing secret-related ever goes in the project tree or under `/mnt/c`.

## Paths (three roles, keep them separate)
- Project tree (source only): the repository checkout, machine-specific;
  `<PROJECT>` in `SETUP.md`.
- Runtime dir (built artifacts + launch script, owned by `appsvc`): `/srv/rustcode-runtime`
  (launcher is `/srv/rustcode-runtime/run-app.sh`, binary at `bin/app`). You may build
  into it as directed, but it is `appsvc`-owned runtime territory, not source.
- Secret dir: `/srv/rustcode-secrets` (see above; not yours to read).
- Your cargo/rust toolchain is at `/home/dev/.cargo` and `/home/dev/.rustup`.
- `/mnt/c` is drvfs and does not enforce POSIX permissions. Never rely on it for
  anything requiring real file modes, and keep nothing sensitive there.

## Testing discipline
- The product ships as an **`i686-unknown-linux-musl` static release** binary;
  `i686-unknown-linux-gnu` debug is the fast inner-loop build. Exercise both in the
  podman container (`docker.io/i386/debian:stable`), plus the bare busybox check,
  via `scripts/check.sh`, which is the "what we ship is what we test" gate.
- Never run the product against the live Anthropic API from your own build session.
  A trivial offline/smoke path is fine here; the real acceptance run happens as the
  separate runtime identity (`runuser`/`appsvc` via the launch script) with the injected
  credential, in/via the container.
- The podman "image platform (linux/386) does not match" warning is expected and
  harmless (containers share the 64-bit WSL2 kernel).

## TLS and 32-bit constraints
- Use a **pure-Rust TLS stack (rustls)**. Do **not** add OpenSSL / native-tls or pull in
  any 32-bit libssl. Avoiding 32-bit OpenSSL is deliberate.
- Mind 32-bit widths: `usize`/pointers are 32-bit. Use explicit `u64`/`i64` for file
  offsets, sizes, and byte counts, not `usize`. Watch for large-allocation and
  integer-overflow assumptions that only hold on 64-bit.

## Reference material
- Any reference repository (e.g. an OpenCode clone) is **read-only, outside the project
  tree, study-only**. Do not build it, run it, execute it against any API, or let its
  files get swept into this repo or a commit. Read it as a spec, nothing more.

## Working style
- Before any system-level change (installs, container ops, anything needing elevation),
  print a short plan of what you'll run and what needs elevation, then proceed. You do
  not have blanket sudo; if something needs elevation, stop and ask rather than working
  around it. Report results per stage.
- Test before proceeding; don't stack unverified changes.
- Scope (REVISED 2026-07-03): the v1 minimal slice (agent loop + seven tools,
  Anthropic-only, line REPL) is **complete and live-verified**. The project is now a
  **general OpenCode clone for 32-bit Linux** for any task: no single workflow is an
  acceptance gate. Post-v1 milestones and the OPEN prioritization question are in
  `ROADMAP.md`. Still: build one milestone at a time; ask before reordering priorities
  or flipping defaults (thinking off, model claude-sonnet-5).
