//! Virtual file system for the pre-compiler daemon.
//!
//! The VFS is the daemon's single source of truth for file contents. It stores
//! every file the semantic front-end has read in memory, and change packs are
//! pushed into it from a change source (an LSP `didChange`, an OS file-watcher,
//! or a polling fallback). Each change is **content-hashed** before being
//! applied: if the hash is unchanged the update is suppressed, which is exactly
//! how `rust-analyzer` avoids spurious recompilation on `git checkout`.
//!
//! [`ChangePack`] application is **transactional within a pack**: multiple
//! changes to the same path are coalesced (last write wins) and the resulting
//! [`ChangeSet`] reports only the files whose content actually changed, so the
//! upstream query engine can invalidate precisely.
#![cfg_attr(not(test), warn(missing_docs))]
#![cfg_attr(not(test), warn(rust_2018_idioms))]

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// A 64-bit content hash. Deterministic across process restarts (unlike a
/// `RandomState`-seeded hasher) so a cold restart can compare against a
/// persisted hash. Collisions are astronomically unlikely for the
/// de-duplication use case and are never trusted for correctness: a hash match
/// only suppresses a *re-derivation*, never a semantic decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(pub u64);

/// Compute the content hash of a string using `DefaultHasher` (SipHash-1-3 with
/// fixed keys). Intentionally *not* cryptographically secure; it is a fast
/// change-detector for de-duplication.
pub fn hash_content(content: &str) -> ContentHash {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut h);
    ContentHash(h.finish())
}

/// A single file-content update.
#[derive(Debug, Clone)]
pub struct Change {
    /// Virtual path of the file being updated.
    pub path: PathBuf,
    /// New full content of the file.
    pub content: String,
}

impl Change {
    /// Create a change.
    pub fn new(path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        Self { path: path.into(), content: content.into() }
    }
}

/// A batch of changes to apply atomically.
#[derive(Debug, Clone, Default)]
pub struct ChangePack {
    /// The changes. If multiple share a path the last one wins (coalesced).
    pub changes: Vec<Change>,
    /// Whether this pack originated from a bulk operation (e.g. `git checkout`)
    /// that touched many files at once. Bulk packs may warrant different
    /// debounce/scheduling behaviour upstream.
    pub bulk: bool,
}

impl ChangePack {
    /// An empty change pack.
    pub fn empty() -> Self {
        Self::default()
    }

    /// A single-file change pack.
    pub fn one(path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        Self { changes: vec![Change::new(path, content)], bulk: false }
    }
}

/// Record of one file's transition during a [`ChangePack::apply`].
#[derive(Debug, Clone)]
pub struct AppliedChange {
    /// File path.
    pub path: PathBuf,
    /// Hash before the apply, or `None` if the file was newly created.
    pub old_hash: Option<ContentHash>,
    /// Hash after the apply.
    pub new_hash: ContentHash,
    /// Whether the content actually changed (hash differed).
    pub changed: bool,
}

/// The result of applying a [`ChangePack`]: the per-file transitions and the
/// new monotonic workspace revision the VFS advanced to.
#[derive(Debug, Clone)]
pub struct ChangeSet {
    /// Per-file transitions, in the order paths were first seen in the pack.
    pub applied: Vec<AppliedChange>,
    /// Monotonic revision after this apply. Bumped only when at least one file
    /// actually changed.
    pub revision: u64,
}

impl ChangeSet {
    /// `true` if no file in this change set changed content.
    pub fn is_empty(&self) -> bool {
        self.applied.iter().all(|c| !c.changed)
    }

    /// Count of files whose content actually changed.
    pub fn changed_count(&self) -> usize {
        self.applied.iter().filter(|c| c.changed).count()
    }
}

/// An in-memory virtual file system.
#[derive(Debug, Default)]
pub struct Vfs {
    files: HashMap<PathBuf, VfsFile>,
    revision: u64,
}

#[derive(Debug, Clone)]
struct VfsFile {
    content: String,
    hash: ContentHash,
}

impl Vfs {
    /// Create an empty VFS.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a single file's content, returning the per-file change.
    ///
    /// This is a convenience for single-file updates; batch updates should use
    /// [`apply`](Self::apply) so that coalescing and the shared revision bump
    /// happen atomically.
    pub fn set_file(
        &mut self,
        path: impl Into<PathBuf>,
        content: impl Into<String>,
    ) -> AppliedChange {
        let path = path.into();
        let content = content.into();
        let new_hash = hash_content(&content);
        let old_hash = self.files.get(&path).map(|f| f.hash);
        let changed = old_hash != Some(new_hash);
        if changed {
            self.revision += 1;
        }
        self.files.insert(path.clone(), VfsFile { content, hash: new_hash });
        AppliedChange { path, old_hash, new_hash, changed }
    }

    /// Apply a change pack transactionally: coalesce by path (last wins), hash,
    /// and only advance the revision when something actually changed.
    pub fn apply(&mut self, pack: ChangePack) -> ChangeSet {
        // Coalesce: last write to each path wins; preserve first-seen order.
        let mut ordered: Vec<PathBuf> = Vec::new();
        let mut coalesced: HashMap<PathBuf, String> = HashMap::new();
        for change in pack.changes {
            if !coalesced.contains_key(&change.path) {
                ordered.push(change.path.clone());
            }
            coalesced.insert(change.path, change.content);
        }

        let mut applied = Vec::with_capacity(ordered.len());
        for path in &ordered {
            let content = coalesced.remove(path).expect("coalesced entry must exist");
            let new_hash = hash_content(&content);
            let old_hash = self.files.get(path).map(|f| f.hash);
            let changed = old_hash != Some(new_hash);
            if changed {
                self.revision += 1;
                self.files.insert(path.clone(), VfsFile { content, hash: new_hash });
            }
            applied.push(AppliedChange { path: path.clone(), old_hash, new_hash, changed });
        }

        ChangeSet { applied, revision: self.revision }
    }

    /// Read a file's content, if present.
    pub fn read(&self, path: &Path) -> Option<&str> {
        self.files.get(path).map(|f| f.content.as_str())
    }

    /// Look up a file's content hash.
    pub fn hash(&self, path: &Path) -> Option<ContentHash> {
        self.files.get(path).map(|f| f.hash)
    }

    /// `true` if the file is tracked.
    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    /// Current monotonic workspace revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Number of tracked files.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// `true` if no files are tracked.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(hash_content("hello"), hash_content("hello"));
        assert_ne!(hash_content("hello"), hash_content("world"));
    }

    #[test]
    fn set_file_reports_change_then_noop() {
        let mut vfs = Vfs::new();
        let first = vfs.set_file("src/main.rs", "fn main() {}");
        assert!(first.changed);
        assert_eq!(first.old_hash, None);
        let rev_after_first = vfs.revision();

        let second = vfs.set_file("src/main.rs", "fn main() {}");
        assert!(!second.changed);
        assert_eq!(vfs.revision(), rev_after_first);
        assert_eq!(second.old_hash, Some(first.new_hash));
    }

    #[test]
    fn apply_coalesces_same_path_and_suppresses_noops() {
        let mut vfs = Vfs::new();
        vfs.set_file("a.rs", "v1");

        let pack = ChangePack {
            changes: vec![
                Change::new("a.rs", "v2"),
                Change::new("a.rs", "v3"),
                Change::new("b.rs", "fresh"),
            ],
            bulk: false,
        };
        let changeset = vfs.apply(pack);

        assert_eq!(changeset.applied.len(), 2);
        let a = &changeset.applied[0];
        assert_eq!(a.path, std::path::PathBuf::from("a.rs"));
        assert!(a.changed);
        assert_eq!(a.new_hash, hash_content("v3"));
        let b = &changeset.applied[1];
        assert_eq!(b.path, std::path::PathBuf::from("b.rs"));
        assert!(b.changed);
        assert_eq!(vfs.revision(), 3); // set_file bump (1) + batch apply bumps (2)
        assert_eq!(changeset.changed_count(), 2);

        let rev = vfs.revision();
        let noop = vfs.apply(ChangePack::one("a.rs", "v3"));
        assert_eq!(noop.revision, rev);
        assert!(noop.is_empty());
    }

    #[test]
    fn read_and_contains() {
        let mut vfs = Vfs::new();
        vfs.apply(ChangePack::one("src/lib.rs", "pub fn x() {}"));
        let p = Path::new("src/lib.rs");
        assert!(vfs.contains(p));
        assert_eq!(vfs.read(p), Some("pub fn x() {}"));
        assert!(!vfs.is_empty());
        assert_eq!(vfs.len(), 1);
    }
}
