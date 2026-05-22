//! Filesystem watcher with debouncing and a periodic reconciliation pass.
//!
//! Two parts work together:
//!
//! 1. **Debounced events** via [`notify_debouncer_full`]. When a markdown
//!    file under the wiki root is created or modified, we read it from
//!    disk, parse the frontmatter, and `reindex_page` against the store.
//!    Own-writes are absorbed by the store's sha256 short-circuit, so
//!    the loop terminates after one no-op reindex.
//! 2. **Reconciliation tick** every 30s walks the entire wiki tree and
//!    reindexes every markdown file. Catches any events the OS dropped
//!    (basic-memory #580 — file watchers go stale under FSEvents buffer
//!    overflow, hidden-dir globs, etc.). Hidden-directory paths are
//!    explicitly NOT skipped (#798 lesson).
//!
//! The watcher never *writes* to disk — that loop would be unbounded.
//! External writes drive store updates; internal writes drive disk +
//! store updates via [`Wiki::write_page`].

use std::path::Path;
use std::time::Duration;

use ai_memory_core::{PagePath, ProjectId, WorkspaceId};
use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::error::{WikiError, WikiResult};
use crate::wiki::Wiki;

/// Reconciliation tick interval.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// Debounce window for filesystem events.
pub const DEBOUNCE_WINDOW: Duration = Duration::from_millis(300);

/// Handle representing an active watcher; drop to stop.
pub struct WatcherHandle {
    _debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl WatcherHandle {
    /// Start watching `wiki.root()` recursively. Spawns one tokio task
    /// that consumes debounced events and runs the reconciliation timer.
    ///
    /// # Errors
    /// Propagates any notify error encountered when installing the OS
    /// watcher.
    pub fn start(wiki: Wiki, workspace_id: WorkspaceId, project_id: ProjectId) -> WikiResult<Self> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let mut debouncer = new_debouncer(
            DEBOUNCE_WINDOW,
            None,
            move |result: DebounceEventResult| match result {
                Ok(events) => {
                    for event in events {
                        let _ = event_tx.send(event);
                    }
                }
                Err(errors) => {
                    for e in errors {
                        warn!(error = %e, "notify error");
                    }
                }
            },
        )
        .map_err(|e| WikiError::Io(std::io::Error::other(e.to_string())))?;

        debouncer
            .watch(wiki.root(), RecursiveMode::Recursive)
            .map_err(|e| WikiError::Io(std::io::Error::other(e.to_string())))?;

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(run_loop(
            wiki,
            workspace_id,
            project_id,
            event_rx,
            shutdown_rx,
        ));

        Ok(Self {
            _debouncer: debouncer,
            shutdown: Some(shutdown_tx),
            task: Some(task),
        })
    }

    /// Stop the watcher and wait for the event loop to drain.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.task.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

async fn run_loop(
    wiki: Wiki,
    ws: WorkspaceId,
    proj: ProjectId,
    mut rx: mpsc::UnboundedReceiver<notify_debouncer_full::DebouncedEvent>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let mut tick = tokio::time::interval(RECONCILE_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // First tick fires immediately; consume it so we don't reconcile at boot.
    tick.tick().await;

    // Track consecutive failures of the reconciliation pass so we can
    // surface a clear "watcher is degraded" event after a streak, in
    // addition to the per-failure error log. Without this, a broken
    // disk → store bridge can stay broken indefinitely with only a
    // line per 30s in the warn stream — easy to miss in busy logs.
    let mut consecutive_failures: u32 = 0;
    const DEGRADED_AFTER: u32 = 5;

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                debug!("watcher shutting down");
                return;
            }
            Some(event) = rx.recv() => {
                handle_event(&wiki, ws, proj, event).await;
            }
            _ = tick.tick() => {
                match reconcile(&wiki, ws, proj).await {
                    Ok(()) => {
                        if consecutive_failures > 0 {
                            tracing::info!(
                                prior_failures = consecutive_failures,
                                "reconciliation recovered after consecutive failures",
                            );
                            consecutive_failures = 0;
                        }
                    }
                    Err(e) => {
                        consecutive_failures += 1;
                        tracing::error!(
                            error = %e,
                            consecutive_failures,
                            "reconciliation failed",
                        );
                        if consecutive_failures == DEGRADED_AFTER {
                            tracing::error!(
                                consecutive_failures,
                                event = "watcher_degraded",
                                "wiki↔store reconciliation has failed {DEGRADED_AFTER} \
                                 times in a row; the disk and SQLite index may now be \
                                 out of sync. Investigate disk permissions, DB lock \
                                 contention, or filesystem health. The watcher will \
                                 keep retrying every {RECONCILE_INTERVAL:?}.",
                            );
                        }
                    }
                }
            }
            else => return,
        }
    }
}

async fn handle_event(
    wiki: &Wiki,
    ws: WorkspaceId,
    proj: ProjectId,
    event: notify_debouncer_full::DebouncedEvent,
) {
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Other
    ) {
        return;
    }
    for raw_path in &event.paths {
        if !is_markdown(raw_path) {
            continue;
        }
        if is_tempfile(raw_path) {
            continue;
        }
        let Some(page_path) = page_path_relative_to(wiki.root(), raw_path) else {
            continue;
        };
        if !raw_path.is_file() {
            // Likely a transient state (mv, atomic rename in flight).
            continue;
        }
        match wiki.reindex_page(ws, proj, page_path.clone()).await {
            Ok(_) => debug!(path = %page_path, "reindexed via watcher"),
            Err(e) => warn!(path = %page_path, error = %e, "watcher reindex failed"),
        }
    }
}

async fn reconcile(wiki: &Wiki, ws: WorkspaceId, proj: ProjectId) -> WikiResult<()> {
    let root = wiki.root().to_path_buf();
    let paths = tokio::task::spawn_blocking(move || walk_markdown(&root))
        .await
        .map_err(|e| WikiError::Io(std::io::Error::other(e.to_string())))??;

    let count = paths.len();
    for path in paths {
        if let Err(e) = wiki.reindex_page(ws, proj, path.clone()).await {
            warn!(path = %path, error = %e, "reconcile reindex failed");
        }
    }
    info!(count, "reconciliation pass complete");
    Ok(())
}

fn walk_markdown(root: &Path) -> WikiResult<Vec<PagePath>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(WikiError::Io(e)),
        };
        for entry in read {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            // Skip symlinks entirely. An attacker with write access to
            // the wiki/ dir could otherwise plant a symlink to /etc/hosts,
            // /home/user/.ssh/id_ed25519 etc. and have the watcher
            // index the target's content. The sanitiser would still
            // scrub credentials, but we'd be reading files we
            // shouldn't be reading. (Audit critical #3.)
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file()
                && is_markdown(&path)
                && !is_tempfile(&path)
                && let Some(pp) = page_path_relative_to(root, &path)
            {
                out.push(pp);
            }
        }
    }
    Ok(out)
}

fn is_markdown(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "md")
}

fn is_tempfile(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(".ai-memory-tmp."))
}

fn page_path_relative_to(root: &Path, abs: &Path) -> Option<PagePath> {
    let rel: &Path = abs.strip_prefix(root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    PagePath::new(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_memory_store::Store;
    use tempfile::TempDir;

    async fn setup() -> (TempDir, Store, Wiki, WorkspaceId, ProjectId) {
        let tmp = TempDir::new().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let ws = store
            .writer
            .get_or_create_workspace("default")
            .await
            .unwrap();
        let proj = store
            .writer
            .get_or_create_project(ws, "scratch", None)
            .await
            .unwrap();
        let wiki = Wiki::new(tmp.path(), store.writer.clone()).unwrap();
        (tmp, store, wiki, ws, proj)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn picks_up_externally_created_file() {
        let (tmp, store, wiki, ws, proj) = setup().await;
        let handle = WatcherHandle::start(wiki.clone(), ws, proj).unwrap();

        // Drop a file *bypassing* the wiki write API (simulating an
        // external editor).
        let target = tmp.path().join("wiki/external.md");
        std::fs::write(&target, "Hello from outside the wiki API.\n").unwrap();

        // Poll for the row to land. Watcher debounces at 300ms.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut hits = Vec::new();
        while std::time::Instant::now() < deadline {
            hits = store
                .reader
                .search_pages("outside".into(), 5)
                .await
                .unwrap();
            if !hits.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!hits.is_empty(), "watcher did not pick up external write");
        assert_eq!(hits[0].path.as_str(), "external.md");
        handle.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_picks_up_file_added_while_watcher_offline() {
        let (tmp, store, wiki, ws, proj) = setup().await;
        // Write a file BEFORE starting the watcher.
        let target = tmp.path().join("wiki/preexisting.md");
        std::fs::write(&target, "I existed first.\n").unwrap();

        let handle = WatcherHandle::start(wiki.clone(), ws, proj).unwrap();
        // Hit reconcile manually instead of waiting 30s.
        reconcile(&wiki, ws, proj).await.unwrap();

        let hits = store
            .reader
            .search_pages("existed".into(), 5)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path.as_str(), "preexisting.md");
        handle.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ignores_own_atomic_tempfiles() {
        // Quick unit test: tempfile prefix detection.
        let p = Path::new("/some/dir/.ai-memory-tmp.abc.md");
        assert!(is_tempfile(p));
        let q = Path::new("/some/dir/normal.md");
        assert!(!is_tempfile(q));
    }

    /// Defence: an attacker who can write to wiki/ shouldn't be able
    /// to make the watcher index arbitrary files via symlinks.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn walk_markdown_skips_symlinks() {
        let tmp = TempDir::new().unwrap();
        let wiki_root = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_root).unwrap();

        // A real file (should be picked up).
        std::fs::write(wiki_root.join("real.md"), "real content\n").unwrap();

        // A "secret" file outside the wiki root.
        let secret = tmp.path().join("secret.md");
        std::fs::write(&secret, "this is sensitive\n").unwrap();

        // Plant a symlink inside wiki/ pointing at the outside file.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, wiki_root.join("symlinked.md")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&secret, wiki_root.join("symlinked.md")).unwrap();

        let found = walk_markdown(&wiki_root).unwrap();
        let names: Vec<_> = found.iter().map(|p| p.as_str().to_string()).collect();
        assert!(names.contains(&"real.md".to_string()), "real file present");
        assert!(
            !names.contains(&"symlinked.md".to_string()),
            "symlink to outside file must be skipped; got: {names:?}"
        );
    }
}
