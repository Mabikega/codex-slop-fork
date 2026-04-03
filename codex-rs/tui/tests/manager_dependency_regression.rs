use std::fs;
use std::path::Path;
use std::path::PathBuf;

fn rust_sources_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("failed to read dir entry: {err}"));
        let path = entry.path();
        if path.is_dir() {
            files.extend(rust_sources_under(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files.sort();
    files
}

#[test]
fn tui_runtime_source_does_not_depend_on_manager_escape_hatches() {
    let src_dir = codex_utils_cargo_bin::find_resource!("src")
        .unwrap_or_else(|err| panic!("failed to resolve src runfile: {err}"));
    let sources = rust_sources_under(&src_dir);
    let forbidden = [
        "AuthManager",
        "ThreadManager",
        "auth_manager(",
        "thread_manager(",
    ];
    let allowed_hits = [
        // The fork overlay still carries the auth manager explicitly in a few seams. Keep the
        // allowlist line-shaped so new usages in these files still fail the test.
        (
            "app.rs",
            "AuthManager",
            "auth_manager: codex_core::AuthManager::shared(",
        ),
        (
            "chatwidget.rs",
            "AuthManager",
            "use codex_core::AuthManager;",
        ),
        (
            "chatwidget.rs",
            "AuthManager",
            "pub(crate) auth_manager: Arc<AuthManager>,",
        ),
        (
            "chatwidget.rs",
            "AuthManager",
            "auth_manager: Arc<AuthManager>,",
        ),
        (
            "chatwidget/tests.rs",
            "AuthManager",
            "auth_manager: codex_core::AuthManager::shared(",
        ),
        (
            "slop_fork/external_auth.rs",
            "AuthManager",
            "use codex_core::AuthManager;",
        ),
        (
            "slop_fork/external_auth.rs",
            "AuthManager",
            "auth_manager: Arc<AuthManager>,",
        ),
        (
            "slop_fork/rate_limit_poller.rs",
            "AuthManager",
            "use codex_core::AuthManager;",
        ),
        (
            "slop_fork/rate_limit_poller.rs",
            "AuthManager",
            "auth_manager: &AuthManager,",
        ),
        (
            "slop_fork/rate_limit_poller.rs",
            "AuthManager",
            "auth_manager: Arc<AuthManager>,",
        ),
        (
            "slop_fork/ui.rs",
            "AuthManager",
            "use codex_core::AuthManager;",
        ),
        (
            "slop_fork/ui.rs",
            "AuthManager",
            "pub(crate) auth_manager: Arc<AuthManager>,",
        ),
    ];

    let mut violations = Vec::new();
    for path in &sources {
        let contents = fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let rel_path = path
            .strip_prefix(&src_dir)
            .unwrap_or(path)
            .iter()
            .filter_map(|part| part.to_str())
            .collect::<Vec<_>>()
            .join("/");
        for (line_idx, line) in contents.lines().enumerate() {
            for needle in &forbidden {
                if !line.contains(needle) {
                    continue;
                }
                let allowed =
                    allowed_hits
                        .iter()
                        .any(|(allowed_path, allowed_needle, allowed_line)| {
                            *allowed_path == rel_path
                                && *allowed_needle == *needle
                                && line.contains(allowed_line)
                        });
                if !allowed {
                    violations.push(format!(
                        "{rel_path}:{} contains `{needle}` on line `{line}`",
                        line_idx + 1
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "unexpected manager dependency regression(s):\n{}",
        violations.join("\n")
    );
}
