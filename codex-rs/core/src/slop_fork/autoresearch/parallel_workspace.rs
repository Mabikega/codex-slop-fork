use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;
use serde::Serialize;

use super::AutoresearchResearchWorkspace;
use super::AutoresearchWorkspace;
use super::AutoresearchWorkspaceMode;
use super::workspace::restore_snapshot;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoresearchParallelWorkspaceLease {
    pub root: PathBuf,
    pub workdir: PathBuf,
    pub approach_id: String,
    pub git_worktree: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoresearchParallelWorkspaceManager {
    root: PathBuf,
}

impl AutoresearchParallelWorkspaceManager {
    pub fn new(codex_home: &Path, thread_id: &str) -> Self {
        Self {
            root: codex_home
                .join(".autoresearch-snapshots")
                .join(thread_id)
                .join("parallel"),
        }
    }

    pub fn prepare_candidate_workspace(
        &self,
        workspace: &AutoresearchWorkspace,
        research_workspace: &AutoresearchResearchWorkspace,
        approach_id: &str,
        token: &str,
    ) -> Result<AutoresearchParallelWorkspaceLease, String> {
        let candidate_root = self.root.join(token);
        match fs::remove_dir_all(&candidate_root) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(format!(
                    "failed to clear parallel candidate workspace {}: {err}",
                    candidate_root.display()
                ));
            }
        }
        fs::create_dir_all(&self.root)
            .map_err(|err| format!("failed to create parallel workspace root: {err}"))?;

        match workspace.mode {
            AutoresearchWorkspaceMode::Git => {
                run_git_owned(
                    &workspace.workdir,
                    vec![
                        "worktree".to_string(),
                        "add".to_string(),
                        "--detach".to_string(),
                        "--".to_string(),
                        candidate_root.to_string_lossy().to_string(),
                    ],
                )?;
            }
            AutoresearchWorkspaceMode::Filesystem => {
                fs::create_dir_all(&candidate_root).map_err(|err| {
                    format!(
                        "failed to create parallel filesystem workspace {}: {err}",
                        candidate_root.display()
                    )
                })?;
            }
        }
        research_workspace.restore_for_approach(&candidate_root, Some(approach_id))?;

        Ok(AutoresearchParallelWorkspaceLease {
            root: candidate_root.clone(),
            workdir: candidate_root,
            approach_id: approach_id.to_string(),
            git_worktree: workspace.mode == AutoresearchWorkspaceMode::Git,
        })
    }

    pub fn promote_candidate(
        &self,
        target_workdir: &Path,
        lease: &AutoresearchParallelWorkspaceLease,
    ) -> Result<(), String> {
        restore_snapshot(target_workdir, &lease.workdir)
    }

    pub fn clear_candidate(
        &self,
        workspace: &AutoresearchWorkspace,
        lease: &AutoresearchParallelWorkspaceLease,
    ) -> Result<(), String> {
        if lease.git_worktree {
            run_git_owned(
                &workspace.workdir,
                vec![
                    "worktree".to_string(),
                    "remove".to_string(),
                    "--force".to_string(),
                    "--".to_string(),
                    lease.root.to_string_lossy().to_string(),
                ],
            )?;
            return Ok(());
        }
        remove_dir_if_exists(&lease.root)
    }

    pub fn clear_all(&self, workspace: Option<&AutoresearchWorkspace>) -> Result<(), String> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(format!(
                    "failed to read parallel workspace root {}: {err}",
                    self.root.display()
                ));
            }
        };
        for entry in entries {
            let entry =
                entry.map_err(|err| format!("failed to read parallel workspace entry: {err}"))?;
            let lease = AutoresearchParallelWorkspaceLease {
                root: entry.path(),
                workdir: entry.path(),
                approach_id: String::new(),
                git_worktree: workspace
                    .is_some_and(|workspace| workspace.mode == AutoresearchWorkspaceMode::Git),
            };
            if let Some(workspace) = workspace {
                self.clear_candidate(workspace, &lease)?;
            } else {
                remove_dir_if_exists(&lease.root)?;
            }
        }
        remove_dir_if_exists(&self.root)
    }
}

fn run_git_owned(cwd: &Path, args: Vec<String>) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| format!("failed to run git: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        Err(format!(
            "git failed with exit code {}",
            output.status.code().unwrap_or_default()
        ))
    } else {
        Err(format!("git failed: {stderr}"))
    }
}

fn remove_dir_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("failed to remove {}: {err}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slop_fork::autoresearch::AUTORESEARCH_DOC_FILE;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    fn run_command<const N: usize>(cwd: &Path, program: &str, args: [&str; N]) {
        let output = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run command");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn prepare_and_promote_parallel_git_candidate_round_trips() {
        let codex_home = tempdir().expect("codex home");
        let workdir = tempdir().expect("workdir");
        run_command(workdir.path(), "git", ["init"]);
        run_command(workdir.path(), "git", ["config", "user.name", "Test User"]);
        run_command(
            workdir.path(),
            "git",
            ["config", "user.email", "test@example.com"],
        );
        fs::write(workdir.path().join("code.txt"), "baseline").expect("write code");
        run_command(workdir.path(), "git", ["add", "code.txt"]);
        run_command(workdir.path(), "git", ["commit", "-m", "baseline"]);
        fs::write(workdir.path().join(AUTORESEARCH_DOC_FILE), "docs").expect("write doc");

        let workspace =
            AutoresearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
                .expect("prepare workspace")
                .workspace;
        let research_workspace =
            AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
                .expect("prepare research workspace");
        research_workspace
            .keep_approach_snapshot(workdir.path(), "approach-1")
            .expect("snapshot");

        let manager = AutoresearchParallelWorkspaceManager::new(codex_home.path(), "thread-1");
        let lease = manager
            .prepare_candidate_workspace(&workspace, &research_workspace, "approach-1", "token-1")
            .expect("prepare parallel workspace");
        fs::write(lease.workdir.join("code.txt"), "parallel candidate").expect("write parallel");

        manager
            .promote_candidate(workdir.path(), &lease)
            .expect("promote candidate");

        assert_eq!(
            fs::read_to_string(workdir.path().join("code.txt")).expect("read code"),
            "parallel candidate"
        );
        assert_eq!(
            fs::read_to_string(workdir.path().join(AUTORESEARCH_DOC_FILE)).expect("read doc"),
            "docs"
        );
        assert!(workdir.path().join(".git").exists());

        manager
            .clear_candidate(&workspace, &lease)
            .expect("clear candidate");
        assert!(!lease.root.exists());
    }
}
