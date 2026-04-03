use super::*;
use codex_protocol::protocol::GranularApprovalConfig;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
#[cfg(not(target_os = "windows"))]
use tempfile::NamedTempFile;
#[cfg(not(target_os = "windows"))]
use tempfile::tempdir;

#[test]
fn wants_no_sandbox_approval_granular_respects_sandbox_flag() {
    let runtime = ApplyPatchRuntime::new();
    assert!(runtime.wants_no_sandbox_approval(AskForApproval::OnRequest));
    assert!(
        !runtime.wants_no_sandbox_approval(AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: false,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }))
    );
    assert!(
        runtime.wants_no_sandbox_approval(AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }))
    );
}

#[test]
fn guardian_review_request_includes_patch_context() {
    let path = std::env::temp_dir().join("guardian-apply-patch-test.txt");
    let action = ApplyPatchAction::new_add_for_test(&path, "hello".to_string());
    let expected_cwd = action.cwd.clone();
    let expected_patch = action.patch.clone();
    let request = ApplyPatchRequest {
        action,
        file_paths: vec![
            AbsolutePathBuf::from_absolute_path(&path).expect("temp path should be absolute"),
        ],
        changes: HashMap::from([(
            path,
            FileChange::Add {
                content: "hello".to_string(),
            },
        )]),
        exec_approval_requirement: ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
        timeout_ms: None,
    };

    let guardian_request = ApplyPatchRuntime::build_guardian_review_request(&request, "call-1");

    assert_eq!(
        guardian_request,
        GuardianApprovalRequest::ApplyPatch {
            id: "call-1".to_string(),
            cwd: expected_cwd,
            files: request.file_paths,
            change_count: 1usize,
            patch: expected_patch,
        }
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn build_sandbox_command_prefers_apply_patch_helper_for_apply_patch() {
    let path = std::env::temp_dir().join("apply-patch-current-exe-test.txt");
    let action = ApplyPatchAction::new_add_for_test(&path, "hello".to_string());
    let request = ApplyPatchRequest {
        action,
        file_paths: vec![
            AbsolutePathBuf::from_absolute_path(&path).expect("temp path should be absolute"),
        ],
        changes: HashMap::from([(
            path,
            FileChange::Add {
                content: "hello".to_string(),
            },
        )]),
        exec_approval_requirement: ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
        timeout_ms: None,
    };
    let helper_dir = tempdir().expect("helper dir");
    let codex_linux_sandbox_exe_path = helper_dir.path().join("codex-linux-sandbox");
    std::fs::write(&codex_linux_sandbox_exe_path, "").expect("write linux sandbox helper");
    let apply_patch_helper_path = helper_dir.path().join("apply_patch");
    std::fs::write(&apply_patch_helper_path, "").expect("write apply_patch helper");
    let codex_self_exe = NamedTempFile::new().expect("self exe temp file");
    let codex_self_exe_path = codex_self_exe.path().to_path_buf();

    let command = ApplyPatchRuntime::build_sandbox_command(
        &request,
        Some(&codex_linux_sandbox_exe_path),
        Some(&codex_self_exe_path),
    )
    .expect("build sandbox command");

    assert_eq!(command.program, apply_patch_helper_path.into_os_string());
    assert_eq!(command.args, vec![request.action.patch.clone()]);
}

#[cfg(not(target_os = "windows"))]
#[test]
fn build_sandbox_command_prefers_configured_codex_self_exe_when_no_linux_helper() {
    let path = std::env::temp_dir().join("apply-patch-current-exe-test.txt");
    let action = ApplyPatchAction::new_add_for_test(&path, "hello".to_string());
    let request = ApplyPatchRequest {
        action,
        file_paths: vec![
            AbsolutePathBuf::from_absolute_path(&path).expect("temp path should be absolute"),
        ],
        changes: HashMap::from([(
            path,
            FileChange::Add {
                content: "hello".to_string(),
            },
        )]),
        exec_approval_requirement: ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
        timeout_ms: None,
    };
    let codex_self_exe = NamedTempFile::new().expect("self exe temp file");

    let command = ApplyPatchRuntime::build_sandbox_command(
        &request,
        /*codex_linux_sandbox_exe*/ None,
        Some(&codex_self_exe.path().to_path_buf()),
    )
    .expect("build sandbox command");

    assert_eq!(command.program, codex_self_exe.path().as_os_str());
    assert_eq!(
        command.args,
        vec![
            CODEX_CORE_APPLY_PATCH_ARG1.to_string(),
            request.action.patch.clone(),
        ]
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn build_sandbox_command_falls_back_to_current_exe_for_apply_patch() {
    let path = std::env::temp_dir().join("apply-patch-current-exe-test.txt");
    let action = ApplyPatchAction::new_add_for_test(&path, "hello".to_string());
    let request = ApplyPatchRequest {
        action,
        file_paths: vec![
            AbsolutePathBuf::from_absolute_path(&path).expect("temp path should be absolute"),
        ],
        changes: HashMap::from([(
            path,
            FileChange::Add {
                content: "hello".to_string(),
            },
        )]),
        exec_approval_requirement: ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
        timeout_ms: None,
    };

    let command = ApplyPatchRuntime::build_sandbox_command(
        &request, /*codex_linux_sandbox_exe*/ None, /*codex_self_exe*/ None,
    )
    .expect("build sandbox command");

    assert_eq!(
        command.program,
        std::env::current_exe()
            .expect("current exe")
            .into_os_string()
    );
    assert_eq!(
        command.args,
        vec![
            CODEX_CORE_APPLY_PATCH_ARG1.to_string(),
            request.action.patch.clone(),
        ]
    );
}
