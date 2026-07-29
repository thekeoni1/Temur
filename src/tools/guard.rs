//! T18 layer 1: in-process key-file isolation for the file-touching tools.
//!
//! File modes cannot protect a key from the model, because tools run as the
//! key-owning uid. This guard closes that hole inside the process: it is
//! built ONCE at startup from the resolved config (every configured
//! `api_key_file` across the active selection and all named profiles, plus
//! the `APP_SECRET_FILE` path when set) and carried in `ToolCtx`. An empty
//! guard checks nothing, so keyless configs and every pre-T18 test behave
//! byte-identically.
//!
//! A candidate path is denied when any of three checks hit:
//!  (a) its lenient canonicalization equals a protected file (defeats
//!      symlinks and path spelling; a not-yet-existing write target is
//!      canonicalized at its deepest existing ancestor),
//!  (b) it lies under the PARENT DIRECTORY of a protected file (a secrets
//!      directory holds sibling keys; the directory prefix covers them),
//!  (c) its (st_dev, st_ino) identity equals a protected file's (defeats
//!      hardlinks and renames).
//! Protected identities are stat'ed once per SNAPSHOT (= once per tool
//! execution), never per candidate, so grep/glob walks stay cheap.

use super::ToolError;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct KeyGuard {
    /// Leniently canonicalized protected file paths, deduplicated.
    protected: Vec<PathBuf>,
    /// Canonical parent directories of the protected files.
    parents: Vec<PathBuf>,
}

impl KeyGuard {
    /// The no-op guard every `ToolCtx::new` starts with.
    pub fn empty() -> Self {
        KeyGuard::default()
    }

    /// Build from configured key-file paths. Canonicalization is lenient
    /// (see [`canonicalize_lenient`]) so a configured-but-missing key file
    /// still protects its path and parent directory.
    pub fn from_paths<I>(paths: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let canon: BTreeSet<PathBuf> =
            paths.into_iter().map(|p| canonicalize_lenient(&p)).collect();
        let parents: BTreeSet<PathBuf> = canon
            .iter()
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .collect();
        KeyGuard {
            protected: canon.into_iter().collect(),
            parents: parents.into_iter().collect(),
        }
    }

    /// Every configured key path for a resolved selection: the active
    /// selection's `api_key_file`, every named profile's, and the
    /// `APP_SECRET_FILE` path when the environment sets one. The ONE
    /// construction rule, shared by startup and doctor, so they cannot
    /// disagree about what is protected.
    pub fn from_selection(
        active: &crate::config::ResolvedProfile,
        profiles: &std::collections::BTreeMap<String, crate::config::ResolvedProfile>,
    ) -> Self {
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Some(p) = &active.api_key_file {
            paths.push(PathBuf::from(p));
        }
        paths.extend(
            profiles
                .values()
                .filter_map(|p| p.api_key_file.as_ref().map(PathBuf::from)),
        );
        if let Some(p) = std::env::var_os("APP_SECRET_FILE") {
            paths.push(PathBuf::from(p));
        }
        Self::from_paths(paths)
    }

    pub fn is_empty(&self) -> bool {
        self.protected.is_empty()
    }

    /// The protected files, leniently canonicalized (layer 2 masks these;
    /// doctor counts them).
    pub fn protected_files(&self) -> &[PathBuf] {
        &self.protected
    }

    /// Freeze the protected files' identities for one tool execution.
    /// Missing files simply have no identity (paths still deny via a/b).
    pub fn snapshot(&self) -> GuardSnapshot<'_> {
        use std::os::unix::fs::MetadataExt;
        let ids: Vec<(u64, u64)> = self
            .protected
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino())))
            .collect();
        GuardSnapshot { guard: self, ids }
    }

    /// One-candidate convenience for read/write/edit: snapshot + check.
    pub fn check(&self, candidate: &Path) -> Result<(), ToolError> {
        if self.is_empty() {
            return Ok(());
        }
        self.snapshot().check(candidate)
    }
}

/// A per-execution view: borrowed path rules + identities stat'ed once.
pub struct GuardSnapshot<'a> {
    guard: &'a KeyGuard,
    ids: Vec<(u64, u64)>,
}

impl GuardSnapshot<'_> {
    /// Does the guard deny this (already cwd-resolved) candidate path?
    pub fn denies(&self, candidate: &Path) -> bool {
        if self.guard.is_empty() {
            return false;
        }
        let canon = canonicalize_lenient(candidate);
        if self.guard.protected.iter().any(|p| p == &canon) {
            return true;
        }
        if self.guard.parents.iter().any(|d| canon.starts_with(d)) {
            return true;
        }
        // Identity: catches hardlinks and renamed keys that live OUTSIDE
        // every protected directory. metadata() follows symlinks, which is
        // exactly the identity that an open would reach.
        if !self.ids.is_empty() {
            use std::os::unix::fs::MetadataExt;
            if let Ok(m) = std::fs::metadata(candidate) {
                if self.ids.contains(&(m.dev(), m.ino())) {
                    return true;
                }
            }
        }
        false
    }

    /// [`GuardSnapshot::denies`] as a model-facing error. The message names
    /// the path and the policy, never any key material.
    pub fn check(&self, candidate: &Path) -> Result<(), ToolError> {
        if self.denies(candidate) {
            return Err(ToolError::failed(format!(
                "access to {} is blocked: configured key files are not readable by tools (key isolation)",
                candidate.display()
            )));
        }
        Ok(())
    }
}

/// Canonicalize, tolerating a not-yet-existing tail: resolve the deepest
/// existing ancestor and re-append the remaining names. Defeats symlinked
/// directories in a write target's path. A tail containing `..` past a
/// missing directory stops the split (`file_name()` is `None`); the
/// original path is returned and the component-wise prefix rule still
/// applies to it.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    let mut base = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while let Some(parent) = base.parent() {
        match base.file_name() {
            Some(name) => rest.push(name.to_os_string()),
            None => break,
        }
        let parent = parent.to_path_buf();
        if let Ok(c) = parent.canonicalize() {
            let mut out = c;
            for name in rest.iter().rev() {
                out.push(name);
            }
            return out;
        }
        base = parent;
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholder_key(dir: &Path) -> PathBuf {
        let secrets = dir.join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        let key = secrets.join("api.key");
        std::fs::write(&key, "placeholder-not-a-real-key\n").unwrap();
        key
    }

    #[test]
    fn empty_guard_denies_nothing() {
        let g = KeyGuard::empty();
        assert!(g.is_empty());
        assert!(!g.snapshot().denies(Path::new("/etc/hostname")));
        assert!(g.check(Path::new("/etc/hostname")).is_ok());
    }

    #[test]
    fn direct_path_and_parent_dir_deny() {
        let tmp = tempfile::tempdir().unwrap();
        let key = placeholder_key(tmp.path());
        let g = KeyGuard::from_paths(vec![key.clone()]);
        let snap = g.snapshot();
        assert!(snap.denies(&key));
        // Sibling in the secrets dir: covered by the parent-dir rule.
        assert!(snap.denies(&key.parent().unwrap().join("other.key")));
        // A not-yet-existing path under the secrets dir: still denied.
        assert!(snap.denies(&key.parent().unwrap().join("new/deep/file")));
        // Outside the secrets dir: allowed.
        assert!(!snap.denies(&tmp.path().join("normal.txt")));
    }

    #[test]
    fn symlink_and_hardlink_deny() {
        let tmp = tempfile::tempdir().unwrap();
        let key = placeholder_key(tmp.path());
        let g = KeyGuard::from_paths(vec![key.clone()]);

        let link = tmp.path().join("innocent-link.txt");
        std::os::unix::fs::symlink(&key, &link).unwrap();
        assert!(g.snapshot().denies(&link), "symlink to a key must deny");

        let hard = tmp.path().join("innocent-hard.txt");
        std::fs::hard_link(&key, &hard).unwrap();
        assert!(g.snapshot().denies(&hard), "hardlink to a key must deny");

        // A symlinked DIRECTORY route to the key (lenient canonicalization
        // resolves the dir even when the leaf spelling differs).
        let dirlink = tmp.path().join("dirlink");
        std::os::unix::fs::symlink(key.parent().unwrap(), &dirlink).unwrap();
        assert!(g.snapshot().denies(&dirlink.join("api.key")));
    }

    #[test]
    fn missing_key_file_still_protects_path_and_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ghost = tmp.path().join("keys").join("future.key");
        let g = KeyGuard::from_paths(vec![ghost.clone()]);
        assert!(!g.is_empty());
        let snap = g.snapshot();
        assert!(snap.denies(&ghost), "configured-but-missing key path denies");
        assert!(snap.denies(&tmp.path().join("keys/sibling")), "its dir denies");
        assert!(!snap.denies(&tmp.path().join("elsewhere")));
    }

    #[test]
    fn denial_message_names_path_and_policy_only() {
        let tmp = tempfile::tempdir().unwrap();
        let key = placeholder_key(tmp.path());
        let g = KeyGuard::from_paths(vec![key.clone()]);
        let err = g.check(&key).unwrap_err().to_string();
        assert!(err.contains("key isolation"), "{err}");
        assert!(err.contains(&key.display().to_string()), "{err}");
        assert!(!err.contains("placeholder-not-a-real-key"), "no key material: {err}");
    }

    #[test]
    fn snapshot_freezes_identities_once() {
        // The walk-scale contract: protected identities are stat'ed at
        // SNAPSHOT time, never per candidate. Proof: snapshot while the key
        // file does not exist yet (no identity to record), then create it
        // and hardlink it outside every protected dir. The old snapshot
        // cannot deny the hardlink (frozen, empty identity set); a fresh
        // one does. (Deliberately NOT remove-and-recreate: filesystems
        // reuse inode numbers, which would make that variant flaky.)
        let tmp = tempfile::tempdir().unwrap();
        let secrets = tmp.path().join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        let key = secrets.join("api.key");
        let g = KeyGuard::from_paths(vec![key.clone()]);
        let old_snap = g.snapshot();

        std::fs::write(&key, "placeholder-not-a-real-key\n").unwrap();
        let outside = tmp.path().join("outside-hard.txt");
        std::fs::hard_link(&key, &outside).unwrap();

        assert!(
            !old_snap.denies(&outside),
            "old snapshot must not re-stat protected files per candidate"
        );
        assert!(g.snapshot().denies(&outside), "a fresh snapshot catches it");
        // Path rules still hold on the stale snapshot regardless.
        assert!(old_snap.denies(&key));
    }

    #[test]
    fn canonicalize_lenient_resolves_existing_prefix_of_missing_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().canonicalize().unwrap();
        let missing = tmp.path().join("a/b/c.txt");
        assert_eq!(canonicalize_lenient(&missing), real.join("a/b/c.txt"));
        // Fully existing paths canonicalize exactly.
        assert_eq!(canonicalize_lenient(tmp.path()), real);
    }
}
