# Environment Setup — i686 Linux Rust project (from scratch)

Executable recipe: run the stages top to bottom on a fresh **Windows 11 (x64)**
machine to reproduce this project's build environment **including the security
boundary**. Each stage ends with a verification — run it before moving on.
Command sequences and file contents below were read from the reference
machine's live state on 2026-07-03.

> **SUBSTITUTIONS — read first.** These values are machine-specific; replace
> them with your own everywhere they appear:
>
> | Placeholder in this guide | Example value | Yours |
> |---|---|---|
> | `<WINUSER>` | `alice` | your Windows username |
> | `<PROJECT>` (Windows) | `C:\Users\alice\Projects\temur` | wherever you clone the repo |
> | `<PROJECT>` (WSL view) | `/mnt/c/Users/alice/Projects/temur` | `/mnt/c/...` equivalent |
>
> Avoid cloud-synced folders for the project tree if you can — build/tool
> churn thrashes sync clients. If the tree must live in one, keeping
> `target/` **off** the Windows mount entirely (stage 7) makes it tolerable;
> do that regardless of where the tree lives.

Fixed names you should **not** change (scripts and docs assume them): the WSL
users `dev` and `appsvc`, `/srv/rustcode-runtime`, `/srv/rustcode-secrets`,
`/home/dev/rustcode-target`.

## Stage 1 — WSL2 + Ubuntu 24.04 (Windows, admin PowerShell)

```powershell
wsl --install -d Ubuntu-24.04
```

At Ubuntu's first-boot prompt, create the initial user as **`dev`** (any
temporary password; it gets locked in stage 2).

*(Not verifiable from machine state: this stage predates the recorded setup;
the command is the standard current method. Verified end state: Ubuntu
24.04.3 LTS on kernel 6.6.87.2-microsoft-standard-WSL2.)*

**Verify:**

```powershell
wsl -d Ubuntu -- sh -c '. /etc/os-release && echo "$PRETTY_NAME"; uname -r'
# expect: Ubuntu 24.04.x LTS / 6.x-microsoft-standard-WSL2
```

## Stage 2 — lock down `dev` (root: `wsl -d Ubuntu -u root` from Windows)

`dev` must be a genuinely unprivileged builder: no sudo, no password.

```sh
deluser dev sudo        # first-boot user is in sudo by default; remove it
passwd -l dev           # lock the password: no su/sudo-by-password path
```

**Verify** (end state on the reference machine: `id dev` →
`uid=1000(dev) gid=1000(dev) groups=1000(dev)`; `passwd -S dev` → `dev L …`):

```sh
id dev                          # no sudo group
passwd -S dev                   # second field is L (locked)
grep -r dev /etc/sudoers /etc/sudoers.d/ || echo OK-no-sudoers-entry
```

## Stage 3 — system packages (root)

```sh
apt-get update
apt-get install -y build-essential gcc-multilib libc6-i386 libc6-dev-i386 podman
```

Verified with: gcc 13.3.0 (gcc-multilib 4:13.2.0-7ubuntu1), libc6-i386 /
libc6-dev-i386 2.39-0ubuntu8.7, podman 4.9.3.

**Deliberately NOT installed:** any 32-bit OpenSSL/libssl packages
(`libssl-dev:i386` etc.). The project uses a pure-Rust TLS stack (rustls);
the only libssl on the system should be Ubuntu's stock 64-bit `libssl3t64`
runtime, with no dev headers. `scripts/check.sh` enforces the absence of
`openssl-sys` in the dependency graph.

**Verify:**

```sh
dpkg -l | grep -i libssl        # expect ONLY libssl3t64:amd64
```

## Stage 4 — /etc/wsl.conf: systemd + default user (root, then Windows)

Write `/etc/wsl.conf` with exactly:

```ini
[boot]
systemd=true

[user]
default=dev
```

`systemd=true` is required for `loginctl enable-linger` (stage 6). Then, from
Windows, restart WSL so it takes effect:

```powershell
wsl --shutdown
```

**Verify:**

```powershell
wsl -d Ubuntu -- sh -c 'whoami; systemctl is-system-running || true'
# expect: dev / running (or degraded — fine)
```

## Stage 5 — Rust toolchain (as `dev`; no elevation needed)

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- --profile minimal -y
. "$HOME/.cargo/env"
rustup target add i686-unknown-linux-gnu i686-unknown-linux-musl
```

Verified with: rustup 1.29.0, rustc/cargo 1.96.1 stable, in `/home/dev/.cargo`
and `/home/dev/.rustup` (profile `minimal`, default toolchain stable, read
from the machine's `~/.rustup/settings.toml`). Login shells get cargo on PATH
automatically via the installer's profile hook.

**Verify:**

```sh
rustc -V && cargo -V
rustup target list --installed   # must include i686-unknown-linux-gnu AND i686-unknown-linux-musl
```

## Stage 6 — rootless podman (the fiddliest part on WSL2)

Rootless podman needs: a subuid/subgid range for `dev`, and linger so the
user session infrastructure exists without an interactive login. As **root**:

```sh
usermod --add-subuids 100000-165535 --add-subgids 100000-165535 dev
loginctl enable-linger dev       # requires systemd=true from stage 4
```

End state to match (`/etc/subuid` and `/etc/subgid`, one line each):

```
dev:100000:65536
```

This exact range is the reference machine's — a fresh Ubuntu may already
have assigned the default user a different range, and the requirement is
that `dev` has *any* valid range not overlapping another user's, not that
specific one.

Then as **`dev`**, pull the validation images (they go into dev's rootless
storage under `/home/dev/.local/share/containers/storage`) — the debian image
is the main test environment, busybox is the bare near-scratch container the
musl-static gate loads the shipped binary in:

```sh
podman pull docker.io/i386/debian:stable
podman pull docker.io/library/busybox:stable
```

**Verify** (as `dev`):

```sh
podman info | grep -A1 rootless        # rootless: true
podman run --rm docker.io/i386/debian:stable dpkg --print-architecture   # i386
podman run --rm docker.io/i386/debian:stable linux32 uname -m            # i686
```

Notes: podman prints `image platform (linux/386) does not match` on every
run — expected and harmless (containers share the 64-bit WSL2 kernel, which
is also why plain `uname -m` reports `x86_64` in-container). Podman was
chosen over Docker deliberately: daemonless, no Docker Desktop dependency.

## Stage 7 — project tree + build config (as `dev`)

Clone the repo to `<PROJECT>` (on the Windows side or via WSL — the tree
lives on `/mnt/c`, source only). The repo already carries
`.cargo/config.toml`:

```toml
[build]
target-dir = "/home/dev/rustcode-target"
target = "i686-unknown-linux-gnu"
```

Build output goes to **native ext4**, never the drvfs `/mnt/c` mount (slow,
and thrashes any sync client). The default target is the fast inner-loop
build; the shipped artifact is the `i686-unknown-linux-musl` static release,
which `scripts/check.sh` builds explicitly. No action needed beyond having
`/home/dev` exist — cargo creates the target dir.

**Verify** (as `dev`, in `<PROJECT>`):

```sh
cargo build
file /home/dev/rustcode-target/i686-unknown-linux-gnu/debug/temur
# expect: ELF 32-bit LSB pie executable, Intel 80386, …
#         interpreter /lib/ld-linux.so.2, for GNU/Linux 3.2.0
```

## Stage 8 — the appsvc security boundary (root; order matters)

The runtime identity `appsvc` owns the built artifact and the secret; `dev`
(the builder) must be able to read **neither** the credential **nor** the
installed binary's directory contents beyond listing. Everything here is on
ext4 — `/mnt/c` (drvfs) does not enforce POSIX permissions and must never
hold anything sensitive.

```sh
# 1. system user (uid/gid auto-allocated; 999/989 on the reference machine —
#    the exact numbers don't matter, the names do)
adduser --system --group --home /srv/rustcode-runtime \
        --shell /usr/sbin/nologin appsvc

# 2. directories (adduser created the home; assert modes explicitly)
install -d -o appsvc -g appsvc -m 755 /srv/rustcode-runtime
install -d -o appsvc -g appsvc -m 755 /srv/rustcode-runtime/bin
install -d -o appsvc -g appsvc -m 755 /srv/rustcode-runtime/work
install -d -o appsvc -g appsvc -m 700 /srv/rustcode-secrets

# 3. placeholder credential (real value injected later, stage 9)
sh -c 'umask 077 && echo "PLACEHOLDER - real credential is injected by a human, see stage 9" \
    > /srv/rustcode-secrets/credential'
chown appsvc:appsvc /srv/rustcode-secrets/credential
chmod 600 /srv/rustcode-secrets/credential
```

4. Write `/srv/rustcode-runtime/run-app.sh` with exactly these contents
(reproduced verbatim from the reference machine):

```sh
#!/bin/sh
# Launch stub for the app (runs as user: appsvc).
# The secret is provided BY PATH via APP_SECRET_FILE; this script never
# reads, echoes, logs, or passes the secret value itself. The app is
# responsible for reading the file at the given path at startup.
set -eu

APP_SECRET_FILE=/srv/rustcode-secrets/credential
APP_BIN=/srv/rustcode-runtime/bin/app   # populated later by the build/deploy step

if [ ! -r "$APP_SECRET_FILE" ]; then
    echo "run-app.sh: secret file missing or unreadable at $APP_SECRET_FILE" >&2
    exit 1
fi
if [ ! -x "$APP_BIN" ]; then
    echo "run-app.sh: app binary not installed at $APP_BIN" >&2
    exit 1
fi

export APP_SECRET_FILE
exec "$APP_BIN"
```

```sh
chown appsvc:appsvc /srv/rustcode-runtime/run-app.sh
chmod 750 /srv/rustcode-runtime/run-app.sh
```

Target end state (`ls -la`):

```
drwxr-xr-x appsvc appsvc /srv/rustcode-runtime
-rwxr-x--- appsvc appsvc /srv/rustcode-runtime/run-app.sh
drwxr-xr-x appsvc appsvc /srv/rustcode-runtime/bin
drwxr-xr-x appsvc appsvc /srv/rustcode-runtime/work
drwx------ appsvc appsvc /srv/rustcode-secrets
-rw------- appsvc appsvc /srv/rustcode-secrets/credential
```

The app binary is **not** installed here by the builder — deployment is
operator-mediated (`docs/RUNBOOK.md`): if `dev` could replace the binary
`appsvc` executes, it could exfiltrate the secret, nullifying the boundary.

**Verify the boundary actively restrains `dev`** (run as `dev` — all three
must fail):

```sh
cat /srv/rustcode-secrets/credential   # Permission denied
ls /srv/rustcode-secrets               # Permission denied
sudo -n true                           # sudo: a password is required
```

And as root, confirm `appsvc` itself can read it:

```sh
runuser -u appsvc -- cat /srv/rustcode-secrets/credential >/dev/null && echo appsvc-read-OK
```

## Stage 9 — inject the real credential (human, root; later)

Write the real credential as the **entire file content**, replacing the
placeholder:

```sh
install -o appsvc -g appsvc -m 600 /path/to/real-credential /srv/rustcode-secrets/credential
# or edit in place, then re-assert ownership/mode:
#   chown appsvc:appsvc /srv/rustcode-secrets/credential && chmod 600 /srv/rustcode-secrets/credential
```

Do not put the credential in shell history (avoid `echo SECRET > file`), in
the project tree, or in any file under `/mnt/c`. The app reads it by path via
`APP_SECRET_FILE` (exported by `run-app.sh`); it never appears in argv, env
listings of other users, or logs, and the app does not read
`ANTHROPIC_API_KEY` at all.

## Stage 10 — full verification

As `dev`, in `<PROJECT>`:

```sh
scripts/check.sh
```

This is the standing per-change gate, in two paths. Path 1 (gnu-debug, fast
inner loop): i686 build + tests on the host, forbidden-dep scan (no
openssl-sys / aws-lc-sys), 32-bit ELF assertion, TLS probe on host and in the
container, all test suites inside `i386/debian:stable`, and the mock REPL +
TUI pty smokes. Path 2 (musl-release, the acceptance gate for the shipped
artifact): `--release --target i686-unknown-linux-musl` build, staticness
assertions (`readelf -l` shows no INTERP, `readelf -d` shows no NEEDED),
the same suites and smokes in the container against the musl binary, and a
`--version` + mock-REPL smoke in the bare busybox container, where a dynamic
binary could not even load. It must end with `== ALL CHECKS PASSED ==`.

## Residual caveats (understand the boundary's real limits)

- **Root bypass:** normal operation is genuinely restrained — `dev` has no
  sudo, a locked password, and cannot read the secret. But anyone on the
  Windows side can get root with `wsl -d Ubuntu -u root` (WSL by design lets
  the Windows user act as any distro user), and Windows admin access to the
  WSL VHD bypasses everything. Treat Windows-side access as trusted-operator
  territory; the boundary's job is to keep the *builder identity and its
  tooling* away from the secret, not to survive a hostile host.
- `/mnt/c` is drvfs: no real POSIX permissions. Nothing sensitive there,
  ever; nothing that needs actual file modes.
- Two auth identities, never crossed: the build session authenticates to
  Anthropic with account credentials only; the product uses the injected
  credential, read by path. `ANTHROPIC_API_KEY` must never be set in the
  build environment (see `CLAUDE.md`).

---

**This environment is not carried in the git repository.** The repo carries
code and docs only; users, permissions, packages, images, and the secret
boundary live on the machine and must be rebuilt from this guide on every new
machine.
