use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher as _};
use std::{
    path::{Component, Path},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Editors and agents write files in bursts, so a reload waits for this much quiet.
const QUIET: Duration = Duration::from_millis(300);

pub struct Watcher {
    _watcher: notify::RecommendedWatcher,
    changed: Arc<Mutex<Option<Instant>>>,
}

impl Watcher {
    pub fn start(repository: &Path) -> Result<Self> {
        let repository = repository.to_path_buf();
        let changed = Arc::new(Mutex::new(None));
        let latest = changed.clone();
        let root = repository.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                if !matches!(event.kind, EventKind::Access(_))
                    && event.paths.iter().any(|path| relevant(&root, path))
                {
                    *latest.lock().unwrap() = Some(Instant::now());
                }
            })?;
        watcher
            .watch(&repository, RecursiveMode::Recursive)
            .with_context(|| format!("Cannot watch {}", repository.display()))?;
        Ok(Self {
            _watcher: watcher,
            changed,
        })
    }

    /// Returns true once per burst of changes, after the burst has gone quiet.
    pub fn settled(&self) -> bool {
        let mut changed = self.changed.lock().unwrap();
        if changed.is_some_and(|at| at.elapsed() >= QUIET) {
            *changed = None;
            return true;
        }
        false
    }
}

/// Working files and Jujutsu operations change the diff; other VCS internals only churn.
fn relevant(root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut components = relative.components();
    match components.next() {
        Some(Component::Normal(name)) if name == ".git" => false,
        Some(Component::Normal(name)) if name == ".jj" => {
            components.any(|component| component.as_os_str() == "op_heads")
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_files_and_operations_count_but_vcs_internals_do_not() {
        let root = Path::new("/repo");
        assert!(relevant(root, Path::new("/repo/src/main.rs")));
        assert!(relevant(
            root,
            Path::new("/repo/.jj/repo/op_heads/heads/abc")
        ));
        assert!(!relevant(
            root,
            Path::new("/repo/.jj/working_copy/checkout")
        ));
        assert!(!relevant(root, Path::new("/repo/.git/index")));
        assert!(relevant(root, Path::new("/repo/docs/.github/ci.yml")));
    }

    #[test]
    fn writes_are_reported_after_they_settle() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let watcher = Watcher::start(&root).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        for index in 0..3 {
            std::fs::write(root.join("a.rs"), format!("{index}")).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while !watcher.settled() {
            assert!(Instant::now() < deadline, "no change was reported");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
