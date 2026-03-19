use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;
use serde::Serialize;

use super::AUTORESEARCH_CHECKS_FILE;
use super::AUTORESEARCH_DOC_FILE;
use super::AUTORESEARCH_IDEAS_FILE;
use super::AUTORESEARCH_JOURNAL_FILE;
use super::AUTORESEARCH_SCRIPT_FILE;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoresearchWorkspaceMode {
    Git,
    Filesystem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoresearchWorkspace {
    pub mode: AutoresearchWorkspaceMode,
    pub workdir: PathBuf,
    pub git_root: Option<PathBuf>,
    pub git_branch: Option<String>,
    pub accepted_revision: Option<String>,
    pub snapshot_root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedAutoresearchWorkspace {
    pub workspace: AutoresearchWorkspace,
    pub summary: String,
}

impl AutoresearchWorkspace {
    pub fn prepare(
        codex_home: &Path,
        thread_id: &str,
        workdir: &Path,
    ) -> Result<PreparedAutoresearchWorkspace, String> {
        if let Some(git_root) = detect_git_root(workdir)? {
            if !git_dirty_paths_excluding_protected(workdir)?.is_empty() {
                return Err("Autoresearch requires a clean git worktree before start.".to_string());
            }
            let accepted_revision = command_output(workdir, "git", ["rev-parse", "HEAD"])?;
            let git_branch = command_output(workdir, "git", ["branch", "--show-current"])
                .ok()
                .and_then(|branch| (!branch.is_empty()).then_some(branch));
            return Ok(PreparedAutoresearchWorkspace {
                summary: format!(
                    "git repo {} at {}",
                    git_branch.as_deref().unwrap_or("(detached)"),
                    git_root.display()
                ),
                workspace: AutoresearchWorkspace {
                    mode: AutoresearchWorkspaceMode::Git,
                    workdir: workdir.to_path_buf(),
                    git_root: Some(git_root),
                    git_branch,
                    accepted_revision: Some(accepted_revision),
                    snapshot_root: None,
                },
            });
        }

        let snapshot_root = codex_home
            .join(".autoresearch-snapshots")
            .join(thread_id)
            .join("accepted");
        refresh_snapshot(workdir, &snapshot_root)?;
        Ok(PreparedAutoresearchWorkspace {
            summary: format!("filesystem snapshot at {}", snapshot_root.display()),
            workspace: AutoresearchWorkspace {
                mode: AutoresearchWorkspaceMode::Filesystem,
                workdir: workdir.to_path_buf(),
                git_root: None,
                git_branch: None,
                accepted_revision: None,
                snapshot_root: Some(snapshot_root),
            },
        })
    }

    pub fn commit_keep(
        &mut self,
        description: &str,
        result_json: &str,
    ) -> Result<Option<String>, String> {
        match self.mode {
            AutoresearchWorkspaceMode::Git => self.commit_keep_git(description, result_json),
            AutoresearchWorkspaceMode::Filesystem => {
                let snapshot_root = self
                    .snapshot_root
                    .as_ref()
                    .ok_or_else(|| "missing filesystem snapshot root".to_string())?;
                refresh_snapshot(&self.workdir, snapshot_root)?;
                Ok(None)
            }
        }
    }

    pub fn restore_discard(&self) -> Result<String, String> {
        match self.mode {
            AutoresearchWorkspaceMode::Git => self.restore_discard_git(),
            AutoresearchWorkspaceMode::Filesystem => {
                let snapshot_root = self
                    .snapshot_root
                    .as_ref()
                    .ok_or_else(|| "missing filesystem snapshot root".to_string())?;
                restore_snapshot(&self.workdir, snapshot_root)?;
                Ok("restored filesystem snapshot".to_string())
            }
        }
    }

    pub fn clear_snapshot(&self) -> std::io::Result<()> {
        if let Some(snapshot_root) = self.snapshot_root.as_ref() {
            match fs::remove_dir_all(snapshot_root) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    fn commit_keep_git(
        &mut self,
        description: &str,
        result_json: &str,
    ) -> Result<Option<String>, String> {
        run_command(&self.workdir, "git", ["add", "-A", "--", "."])?;
        let quiet = Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .current_dir(&self.workdir)
            .status()
            .map_err(|err| format!("failed to check staged diff: {err}"))?;
        if quiet.success() {
            return Ok(None);
        }

        let message = format!("{description}\n\nAutoresearch-Result: {result_json}");
        run_command(&self.workdir, "git", ["commit", "-m", &message])?;
        let revision = command_output(&self.workdir, "git", ["rev-parse", "HEAD"])?;
        self.accepted_revision = Some(revision.clone());
        Ok(Some(revision))
    }

    fn restore_discard_git(&self) -> Result<String, String> {
        let accepted_revision = self
            .accepted_revision
            .as_deref()
            .ok_or_else(|| "missing accepted git revision".to_string())?;

        let mut tracked_paths = git_changed_paths(&self.workdir, true)?;
        tracked_paths.extend(git_changed_paths(&self.workdir, false)?);
        tracked_paths.retain(|path| !is_protected_rel_path(path));
        tracked_paths.sort();
        tracked_paths.dedup();
        if !tracked_paths.is_empty() {
            let mut args = vec![
                "restore".to_string(),
                format!("--source={accepted_revision}"),
                "--staged".to_string(),
                "--worktree".to_string(),
                "--".to_string(),
            ];
            args.extend(tracked_paths);
            run_command_owned(&self.workdir, "git", args)?;
        }

        for path in git_untracked_paths(&self.workdir)? {
            if is_protected_rel_path(&path) {
                continue;
            }
            remove_path_force(&self.workdir.join(path))?;
        }

        Ok("restored git workspace".to_string())
    }
}

fn detect_git_root(workdir: &Path) -> Result<Option<PathBuf>, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(workdir)
        .output()
        .map_err(|err| format!("failed to detect git root: {err}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(stdout)))
}

fn command_output<const N: usize>(
    cwd: &Path,
    program: &str,
    args: [&str; N],
) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| format!("failed to run {program}: {err}"))?;
    if !output.status.success() {
        return Err(command_error(program, &output.stderr, output.status.code()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_command<const N: usize>(cwd: &Path, program: &str, args: [&str; N]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| format!("failed to run {program}: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(command_error(program, &output.stderr, output.status.code()))
}

fn run_command_owned(cwd: &Path, program: &str, args: Vec<String>) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| format!("failed to run {program}: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(command_error(program, &output.stderr, output.status.code()))
}

fn command_error(program: &str, stderr: &[u8], code: Option<i32>) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if stderr.is_empty() {
        format!(
            "{program} failed with exit code {}",
            code.unwrap_or_default()
        )
    } else {
        format!("{program} failed: {stderr}")
    }
}

fn git_changed_paths(workdir: &Path, staged: bool) -> Result<Vec<String>, String> {
    let args = if staged {
        vec!["diff", "--name-only", "--cached", "--", "."]
    } else {
        vec!["diff", "--name-only", "--", "."]
    };
    let output = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .map_err(|err| format!("failed to run git: {err}"))?;
    if !output.status.success() {
        return Err(command_error("git", &output.stderr, output.status.code()));
    }
    let output = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn git_untracked_paths(workdir: &Path) -> Result<Vec<String>, String> {
    let output = command_output(
        workdir,
        "git",
        ["ls-files", "--others", "--exclude-standard"],
    )?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn git_dirty_paths_excluding_protected(workdir: &Path) -> Result<Vec<String>, String> {
    let mut paths = git_changed_paths(workdir, true)?;
    paths.extend(git_changed_paths(workdir, false)?);
    paths.extend(git_untracked_paths(workdir)?);
    paths.retain(|path| !is_protected_rel_path(path));
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn refresh_snapshot(workdir: &Path, snapshot_root: &Path) -> Result<(), String> {
    match fs::remove_dir_all(snapshot_root) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("failed to clear snapshot: {err}")),
    }
    fs::create_dir_all(snapshot_root).map_err(|err| format!("failed to create snapshot: {err}"))?;
    copy_dir_contents(workdir, snapshot_root)?;
    Ok(())
}

fn restore_snapshot(workdir: &Path, snapshot_root: &Path) -> Result<(), String> {
    clear_workdir_except_protected(workdir)?;
    copy_dir_contents(snapshot_root, workdir)?;
    Ok(())
}

fn clear_workdir_except_protected(workdir: &Path) -> Result<(), String> {
    let entries = fs::read_dir(workdir).map_err(|err| format!("failed to read workdir: {err}"))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to read directory entry: {err}"))?;
        let path = entry.path();
        let name = entry.file_name();
        if is_protected_name(name.as_os_str()) {
            continue;
        }
        remove_path_force(&path)?;
    }
    Ok(())
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in
        fs::read_dir(src).map_err(|err| format!("failed to read {}: {err}", src.display()))?
    {
        let entry = entry.map_err(|err| format!("failed to read directory entry: {err}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if is_protected_name(entry.file_name().as_os_str()) {
            continue;
        }
        copy_path(&src_path, &dst_path)?;
    }
    Ok(())
}

fn copy_path(src: &Path, dst: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(src)
        .map_err(|err| format!("failed to inspect {}: {err}", src.display()))?;
    if metadata.is_dir() {
        fs::create_dir_all(dst)
            .map_err(|err| format!("failed to create {}: {err}", dst.display()))?;
        for entry in
            fs::read_dir(src).map_err(|err| format!("failed to read {}: {err}", src.display()))?
        {
            let entry = entry.map_err(|err| format!("failed to read directory entry: {err}"))?;
            copy_path(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(src)
            .map_err(|err| format!("failed to read symlink {}: {err}", src.display()))?;
        create_symlink(&target, dst)?;
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    fs::copy(src, dst).map_err(|err| {
        format!(
            "failed to copy {} to {}: {err}",
            src.display(),
            dst.display()
        )
    })?;
    Ok(())
}

fn remove_path_force(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                fs::remove_dir_all(path)
                    .map_err(|err| format!("failed to remove {}: {err}", path.display()))
            } else {
                fs::remove_file(path)
                    .map_err(|err| format!("failed to remove {}: {err}", path.display()))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("failed to inspect {}: {err}", path.display())),
    }
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> Result<(), String> {
    use std::os::unix::fs::symlink;
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    match fs::remove_file(link) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("failed to replace {}: {err}", link.display())),
    }
    symlink(target, link)
        .map_err(|err| format!("failed to create symlink {}: {err}", link.display()))
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path) -> Result<(), String> {
    use std::os::windows::fs::symlink_dir;
    use std::os::windows::fs::symlink_file;
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    match fs::remove_file(link) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("failed to replace {}: {err}", link.display())),
    }
    if target.is_dir() {
        symlink_dir(target, link).map_err(|err| {
            format!(
                "failed to create directory symlink {}: {err}",
                link.display()
            )
        })
    } else {
        symlink_file(target, link)
            .map_err(|err| format!("failed to create file symlink {}: {err}", link.display()))
    }
}

fn is_protected_rel_path(path: &str) -> bool {
    Path::new(path).components().count() == 1 && is_protected_name(Path::new(path).as_os_str())
}

fn is_protected_name(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(AUTORESEARCH_JOURNAL_FILE)
            | Some(AUTORESEARCH_DOC_FILE)
            | Some(AUTORESEARCH_SCRIPT_FILE)
            | Some(AUTORESEARCH_CHECKS_FILE)
            | Some(AUTORESEARCH_IDEAS_FILE)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn filesystem_snapshot_round_trip_preserves_session_files() {
        let codex_home = tempdir().expect("codex home");
        let workdir = tempdir().expect("workdir");
        fs::write(workdir.path().join("code.txt"), "baseline").expect("write");
        let prepared =
            AutoresearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
                .expect("prepare");
        let workspace = prepared.workspace;
        fs::write(workdir.path().join("code.txt"), "changed").expect("write");
        fs::write(workdir.path().join(AUTORESEARCH_DOC_FILE), "session docs").expect("write");
        workspace.restore_discard().expect("restore");
        assert_eq!(
            fs::read_to_string(workdir.path().join("code.txt")).expect("read"),
            "baseline"
        );
        assert_eq!(
            fs::read_to_string(workdir.path().join(AUTORESEARCH_DOC_FILE)).expect("read"),
            "session docs"
        );
    }

    #[test]
    fn git_discard_restores_tracked_files_but_keeps_session_files() {
        let codex_home = tempdir().expect("codex home");
        let workdir = tempdir().expect("workdir");

        run_command(workdir.path(), "git", ["init"]).expect("git init");
        run_command(workdir.path(), "git", ["config", "user.name", "Test User"])
            .expect("git config user.name");
        run_command(
            workdir.path(),
            "git",
            ["config", "user.email", "test@example.com"],
        )
        .expect("git config user.email");

        fs::write(workdir.path().join("code.txt"), "baseline").expect("write");
        run_command(workdir.path(), "git", ["add", "code.txt"]).expect("git add");
        run_command(workdir.path(), "git", ["commit", "-m", "baseline"]).expect("git commit");

        let prepared =
            AutoresearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
                .expect("prepare");
        let workspace = prepared.workspace;

        fs::write(workdir.path().join("code.txt"), "changed").expect("write");
        fs::write(workdir.path().join(AUTORESEARCH_DOC_FILE), "session docs").expect("write");

        workspace.restore_discard().expect("restore");

        assert_eq!(
            fs::read_to_string(workdir.path().join("code.txt")).expect("read"),
            "baseline"
        );
        assert_eq!(
            fs::read_to_string(workdir.path().join(AUTORESEARCH_DOC_FILE)).expect("read"),
            "session docs"
        );
    }

    #[test]
    fn git_prepare_allows_existing_session_files() {
        let codex_home = tempdir().expect("codex home");
        let workdir = tempdir().expect("workdir");

        run_command(workdir.path(), "git", ["init"]).expect("git init");
        run_command(workdir.path(), "git", ["config", "user.name", "Test User"])
            .expect("git config user.name");
        run_command(
            workdir.path(),
            "git",
            ["config", "user.email", "test@example.com"],
        )
        .expect("git config user.email");

        fs::write(workdir.path().join("code.txt"), "baseline").expect("write");
        run_command(workdir.path(), "git", ["add", "code.txt"]).expect("git add");
        run_command(workdir.path(), "git", ["commit", "-m", "baseline"]).expect("git commit");

        fs::write(workdir.path().join(AUTORESEARCH_DOC_FILE), "session docs").expect("write");
        fs::write(workdir.path().join(AUTORESEARCH_JOURNAL_FILE), "{}\n").expect("write");

        let prepared =
            AutoresearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
                .expect("prepare");

        assert_eq!(prepared.workspace.mode, AutoresearchWorkspaceMode::Git);
    }
}
