use std::fs;
use std::path::Path;
use std::path::PathBuf;

use super::workspace::refresh_snapshot;
use super::workspace::restore_snapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoresearchResearchWorkspace {
    root: PathBuf,
    baseline_root: PathBuf,
    approaches_root: PathBuf,
}

impl AutoresearchResearchWorkspace {
    pub fn prepare(codex_home: &Path, thread_id: &str, workdir: &Path) -> Result<Self, String> {
        let workspace = Self::new(codex_home, thread_id);
        refresh_snapshot(workdir, &workspace.baseline_root)?;
        fs::create_dir_all(&workspace.approaches_root)
            .map_err(|err| format!("failed to create research snapshots: {err}"))?;
        Ok(workspace)
    }

    pub fn new(codex_home: &Path, thread_id: &str) -> Self {
        let root = codex_home
            .join(".autoresearch-snapshots")
            .join(thread_id)
            .join("research");
        let baseline_root = root.join("baseline");
        let approaches_root = root.join("approaches");
        Self {
            root,
            baseline_root,
            approaches_root,
        }
    }

    pub fn restore_for_approach(
        &self,
        workdir: &Path,
        approach_id: Option<&str>,
    ) -> Result<String, String> {
        let snapshot_root = approach_id
            .map(|approach_id| self.approach_snapshot_root(approach_id))
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| self.baseline_root.clone());
        restore_snapshot(workdir, &snapshot_root)?;
        Ok(format!("restored {}", snapshot_root.display()))
    }

    pub fn keep_approach_snapshot(&self, workdir: &Path, approach_id: &str) -> Result<(), String> {
        let snapshot_root = self.approach_snapshot_root(approach_id);
        refresh_snapshot(workdir, &snapshot_root)
    }

    pub fn has_approach_snapshot(&self, approach_id: &str) -> bool {
        self.approach_snapshot_root(approach_id).is_dir()
    }

    pub fn clear(&self) -> std::io::Result<()> {
        match fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn approach_snapshot_root(&self, approach_id: &str) -> PathBuf {
        self.approaches_root.join(approach_id).join("accepted")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slop_fork::autoresearch::AUTORESEARCH_DOC_FILE;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn restore_for_missing_approach_uses_baseline_snapshot() {
        let codex_home = tempdir().expect("codex home");
        let workdir = tempdir().expect("workdir");
        fs::write(workdir.path().join("code.txt"), "baseline").expect("write");
        let workspace =
            AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
                .expect("prepare");

        fs::write(workdir.path().join("code.txt"), "changed").expect("write");
        fs::write(workdir.path().join(AUTORESEARCH_DOC_FILE), "docs").expect("write");

        workspace
            .restore_for_approach(workdir.path(), Some("missing"))
            .expect("restore");

        assert_eq!(
            fs::read_to_string(workdir.path().join("code.txt")).expect("read"),
            "baseline"
        );
        assert_eq!(
            fs::read_to_string(workdir.path().join(AUTORESEARCH_DOC_FILE)).expect("read"),
            "docs"
        );
    }
}
