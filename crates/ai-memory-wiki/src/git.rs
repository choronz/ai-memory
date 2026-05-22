//! Git versioning for the wiki tree.
//!
//! On `Wiki::new`, we lazily `git init` the wiki root if it isn't already
//! a repo. Auto-commits fire from the hook router on `SessionEnd` and
//! from the M7 consolidator. Author/email are fixed so the wiki history
//! can't accidentally leak the maintainer's git identity.

use std::path::{Path, PathBuf};

use git2::{IndexAddOption, Repository, Signature};
use tracing::{debug, warn};

use crate::error::{WikiError, WikiResult};

/// Author identity used for ai-memory's own commits. The user can
/// rewrite history with their own identity later if they care.
pub const COMMIT_AUTHOR_NAME: &str = "ai-memory";
/// Author email used for ai-memory's own commits.
pub const COMMIT_AUTHOR_EMAIL: &str = "ai-memory@local";

/// Thin handle over the wiki repo. Cheap to clone — internally a `PathBuf`.
#[derive(Clone)]
pub struct GitAdapter {
    root: PathBuf,
}

impl GitAdapter {
    /// Open or initialise the repo at `root`. Idempotent: if the
    /// directory is already a git repo, leaves it alone.
    ///
    /// # Errors
    /// Propagates any underlying libgit2 error.
    pub fn open_or_init(root: &Path) -> WikiResult<Self> {
        std::fs::create_dir_all(root)?;
        match Repository::open(root) {
            Ok(_) => debug!(root = %root.display(), "wiki repo already initialised"),
            Err(_) => {
                debug!(root = %root.display(), "initialising wiki repo");
                Repository::init(root).map_err(map_git_err)?;
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// Path of the wiki root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Stage *everything* in the wiki root, then commit with `message`.
    /// Returns `Ok(None)` if there were no changes to commit (working
    /// tree clean), or `Ok(Some(commit_oid))` on a successful commit.
    ///
    /// # Errors
    /// Propagates any underlying libgit2 error.
    pub fn commit_all(&self, message: &str) -> WikiResult<Option<git2::Oid>> {
        let repo = Repository::open(&self.root).map_err(map_git_err)?;

        // Stage everything (including deletions).
        let mut index = repo.index().map_err(map_git_err)?;
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .map_err(map_git_err)?;
        index.write().map_err(map_git_err)?;

        // If the index matches HEAD, there is nothing to commit.
        let tree_oid = index.write_tree().map_err(map_git_err)?;
        if let Ok(head) = repo.head()
            && let Some(target) = head.target()
            && let Ok(parent_commit) = repo.find_commit(target)
            && parent_commit.tree_id() == tree_oid
        {
            debug!("working tree clean; no commit");
            return Ok(None);
        }
        // Fresh repo with no HEAD yet: still skip the commit if there
        // is nothing staged. Otherwise we'd produce an "initial" commit
        // pointing at the empty tree, which surprises both `git log`
        // and our own callers.
        if repo.head().is_err() && index.len() == 0 {
            debug!("fresh repo, empty index; no commit");
            return Ok(None);
        }
        let tree = repo.find_tree(tree_oid).map_err(map_git_err)?;
        let sig = Signature::now(COMMIT_AUTHOR_NAME, COMMIT_AUTHOR_EMAIL).map_err(map_git_err)?;

        let parents: Vec<git2::Commit<'_>> = match repo.head() {
            Ok(head) => match head.target() {
                Some(oid) => vec![repo.find_commit(oid).map_err(map_git_err)?],
                None => Vec::new(),
            },
            Err(_) => Vec::new(),
        };
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .map_err(map_git_err)?;
        debug!(oid = %oid, "wiki commit");
        Ok(Some(oid))
    }

    /// Count commits reachable from HEAD. Returns 0 for an empty repo.
    /// Useful for the test suite + for `ai-memory status`.
    #[must_use]
    pub fn commit_count(&self) -> usize {
        let Ok(repo) = Repository::open(&self.root) else {
            return 0;
        };
        let Ok(mut walk) = repo.revwalk() else {
            return 0;
        };
        if walk.push_head().is_err() {
            return 0;
        }
        walk.count()
    }
}

fn map_git_err(e: git2::Error) -> WikiError {
    warn!(error = %e, "libgit2 error");
    WikiError::Io(std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_is_idempotent_and_creates_dotgit() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("wiki");
        let _adapter = GitAdapter::open_or_init(&root).unwrap();
        assert!(root.join(".git").is_dir());
        // Second open is a no-op.
        let _adapter2 = GitAdapter::open_or_init(&root).unwrap();
    }

    #[test]
    fn commit_all_returns_none_when_clean_some_when_dirty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("wiki");
        let adapter = GitAdapter::open_or_init(&root).unwrap();
        // No changes: returns None.
        assert!(adapter.commit_all("initial").unwrap().is_none());

        // Add a file -> commit -> Some(oid).
        std::fs::write(root.join("foo.md"), "hello").unwrap();
        let oid = adapter.commit_all("add foo").unwrap();
        assert!(oid.is_some());

        // Re-commit with no changes -> None again.
        assert!(adapter.commit_all("no changes").unwrap().is_none());
        assert_eq!(adapter.commit_count(), 1);
    }

    #[test]
    fn commit_all_captures_deletes_too() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("wiki");
        let adapter = GitAdapter::open_or_init(&root).unwrap();
        std::fs::write(root.join("a.md"), "first").unwrap();
        adapter.commit_all("first").unwrap();
        std::fs::remove_file(root.join("a.md")).unwrap();
        let oid = adapter.commit_all("remove a").unwrap();
        assert!(oid.is_some());
        assert_eq!(adapter.commit_count(), 2);
    }
}
