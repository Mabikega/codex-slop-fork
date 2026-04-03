use anyhow::Context;
use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::to_response;
use chrono::Local;
use codex_app_server_protocol::AutoresearchControlAction;
use codex_app_server_protocol::AutoresearchControlParams;
use codex_app_server_protocol::AutoresearchControlResponse;
use codex_app_server_protocol::AutoresearchMode;
use codex_app_server_protocol::AutoresearchReadParams;
use codex_app_server_protocol::AutoresearchReadResponse;
use codex_app_server_protocol::AutoresearchStartParams;
use codex_app_server_protocol::AutoresearchStartResponse;
use codex_app_server_protocol::AutoresearchStatus;
use codex_app_server_protocol::AutoresearchUpdateType;
use codex_app_server_protocol::AutoresearchUpdatedNotification;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::PilotControlAction;
use codex_app_server_protocol::PilotControlParams;
use codex_app_server_protocol::PilotControlResponse;
use codex_app_server_protocol::PilotReadParams;
use codex_app_server_protocol::PilotReadResponse;
use codex_app_server_protocol::PilotStartParams;
use codex_app_server_protocol::PilotStartResponse;
use codex_app_server_protocol::PilotStatus;
use codex_app_server_protocol::PilotUpdateType;
use codex_app_server_protocol::PilotUpdatedNotification;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadUnsubscribeParams;
use codex_app_server_protocol::ThreadUnsubscribeResponse;
use codex_app_server_protocol::ThreadUnsubscribeStatus;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_core::slop_fork::autoresearch::AutoresearchMode as CoreAutoresearchMode;
use codex_core::slop_fork::autoresearch::AutoresearchResearchWorkspace;
use codex_core::slop_fork::autoresearch::AutoresearchRuntime;
use codex_core::slop_fork::autoresearch::AutoresearchWorkspace;
use codex_core::slop_fork::autoresearch::AutoresearchWorkspaceMode;
use codex_core::slop_fork::pilot::PilotRuntime;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

use core_test_support::responses;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn pilot_control_succeeds_after_thread_is_unloaded() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_cwd = create_thread_cwd(codex_home.path())?;
    let thread_id = start_thread(&mut mcp, &thread_cwd).await?;
    let mut runtime = PilotRuntime::load(codex_home.path(), thread_id.as_str())?;
    assert!(runtime.start(
        "keep shipping".to_string(),
        /*deadline_at*/ None,
        Local::now(),
    )?);

    unload_thread(&mut mcp, &thread_id).await?;

    let control_id = mcp
        .send_pilot_control_request(PilotControlParams {
            thread_id: thread_id.clone(),
            action: PilotControlAction::Pause,
        })
        .await?;
    let control_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(control_id)),
    )
    .await??;
    let PilotControlResponse { updated, run } = to_response::<PilotControlResponse>(control_resp)?;

    assert!(updated);
    assert_eq!(
        run.as_ref().map(|run| run.status),
        Some(PilotStatus::Paused)
    );
    Ok(())
}

#[tokio::test]
async fn autoresearch_control_succeeds_after_thread_is_unloaded() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    let workdir = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_cwd = create_thread_cwd(codex_home.path())?;
    let thread_id = start_thread(&mut mcp, &thread_cwd).await?;
    let mut runtime = AutoresearchRuntime::load(codex_home.path(), thread_id.as_str())?;
    AutoresearchResearchWorkspace::prepare(codex_home.path(), &thread_id, workdir.path())
        .map_err(anyhow::Error::msg)?;
    assert!(runtime.start(
        "map hypotheses".to_string(),
        CoreAutoresearchMode::Scientist,
        workdir.path().to_path_buf(),
        AutoresearchWorkspace {
            mode: AutoresearchWorkspaceMode::Filesystem,
            workdir: workdir.path().to_path_buf(),
            git_root: None,
            git_branch: None,
            accepted_revision: None,
            snapshot_root: Some(workdir.path().join("snapshot")),
        },
        /*max_runs*/ None,
        Local::now(),
    )?);

    unload_thread(&mut mcp, &thread_id).await?;

    let control_id = mcp
        .send_autoresearch_control_request(AutoresearchControlParams {
            thread_id: thread_id.clone(),
            action: AutoresearchControlAction::Pause,
            focus: None,
        })
        .await?;
    let control_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(control_id)),
    )
    .await??;
    let AutoresearchControlResponse { updated, run } =
        to_response::<AutoresearchControlResponse>(control_resp)?;

    assert!(updated);
    assert_eq!(
        run.as_ref().map(|run| run.status),
        Some(AutoresearchStatus::Paused)
    );
    Ok(())
}

#[tokio::test]
async fn pilot_start_succeeds_after_thread_is_unloaded() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_cwd = create_thread_cwd(codex_home.path())?;
    let thread_id = start_thread(&mut mcp, &thread_cwd).await?;
    unload_thread(&mut mcp, &thread_id).await?;

    let start_id = mcp
        .send_pilot_start_request(PilotStartParams {
            thread_id: thread_id.clone(),
            goal: "keep shipping".to_string(),
            deadline_at: None,
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let PilotStartResponse { run } = to_response::<PilotStartResponse>(start_resp)?;

    assert_eq!(run.status, PilotStatus::Running);
    assert_eq!(run.goal, "keep shipping");
    Ok(())
}

#[tokio::test]
async fn pilot_start_rejects_pending_thread_unload() -> Result<()> {
    let server = responses::start_mock_server().await;
    let delayed_body = responses::sse(vec![responses::ev_response_created("resp-pending-unload")]);
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(
            responses::sse_response(delayed_body).set_delay(std::time::Duration::from_secs(1)),
        )
        .mount(&server)
        .await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_cwd = create_thread_cwd(codex_home.path())?;
    let thread_id = start_thread(&mut mcp, &thread_cwd).await?;
    let _in_flight_turn_id = start_turn_without_waiting(&mut mcp, &thread_id).await?;
    begin_unload_thread(&mut mcp, &thread_id).await?;

    let start_id = mcp
        .send_pilot_start_request(PilotStartParams {
            thread_id: thread_id.clone(),
            goal: "keep shipping".to_string(),
            deadline_at: None,
        })
        .await?;
    let start_err = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(start_id)),
    )
    .await??;
    assert_eq!(start_err.error.code, -32600);
    assert!(
        start_err
            .error
            .message
            .contains("retry pilot/start after the thread is closed")
    );

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("thread/closed"),
    )
    .await??;
    Ok(())
}

#[tokio::test]
async fn autoresearch_start_succeeds_after_thread_is_unloaded() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_cwd = create_thread_cwd(codex_home.path())?;
    let thread_id = start_thread(&mut mcp, &thread_cwd).await?;
    unload_thread(&mut mcp, &thread_id).await?;

    let start_id = mcp
        .send_autoresearch_start_request(AutoresearchStartParams {
            thread_id: thread_id.clone(),
            goal: "map hypotheses".to_string(),
            max_runs: None,
            mode: AutoresearchMode::Scientist,
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let AutoresearchStartResponse { updated, run } =
        to_response::<AutoresearchStartResponse>(start_resp)?;

    assert!(updated);
    let run = run.expect("autoresearch run after start");
    assert_eq!(run.status, AutoresearchStatus::Running);
    assert_eq!(run.goal, "map hypotheses");
    Ok(())
}

#[tokio::test]
async fn pilot_read_clears_stale_running_state_after_thread_is_unloaded() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_cwd = create_thread_cwd(codex_home.path())?;
    let thread_id = start_thread(&mut mcp, &thread_cwd).await?;
    let mut runtime = PilotRuntime::load(codex_home.path(), thread_id.as_str())?;
    assert!(runtime.start(
        "keep shipping".to_string(),
        /*deadline_at*/ None,
        Local::now(),
    )?);
    let _plan = runtime
        .prepare_cycle_submission(Local::now())?
        .expect("pilot cycle should be prepared");
    assert!(runtime.note_turn_submitted("turn-pilot")?);
    assert!(runtime.activate_pending_cycle("turn-pilot".to_string())?);

    unload_thread(&mut mcp, &thread_id).await?;

    let read_id = mcp
        .send_pilot_read_request(PilotReadParams {
            thread_id: thread_id.clone(),
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let PilotReadResponse { run } = to_response::<PilotReadResponse>(read_resp)?;

    let run = run.expect("pilot run after stale recovery");
    assert_eq!(run.status, PilotStatus::Running);
    assert_eq!(run.active_turn_id, None);
    assert_eq!(run.last_submitted_turn_id, None);
    Ok(())
}

#[tokio::test]
async fn autoresearch_read_clears_stale_running_state_after_thread_is_unloaded() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    let workdir = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_cwd = create_thread_cwd(codex_home.path())?;
    let thread_id = start_thread(&mut mcp, &thread_cwd).await?;
    let mut runtime = AutoresearchRuntime::load(codex_home.path(), thread_id.as_str())?;
    AutoresearchResearchWorkspace::prepare(codex_home.path(), &thread_id, workdir.path())
        .map_err(anyhow::Error::msg)?;
    assert!(runtime.start(
        "map hypotheses".to_string(),
        CoreAutoresearchMode::Scientist,
        workdir.path().to_path_buf(),
        AutoresearchWorkspace {
            mode: AutoresearchWorkspaceMode::Filesystem,
            workdir: workdir.path().to_path_buf(),
            git_root: None,
            git_branch: None,
            accepted_revision: None,
            snapshot_root: Some(workdir.path().join("snapshot")),
        },
        /*max_runs*/ None,
        Local::now(),
    )?);
    let _plan = runtime
        .prepare_cycle_submission(Local::now())?
        .expect("autoresearch cycle should be prepared");
    assert!(runtime.note_turn_submitted("turn-autoresearch")?);
    assert!(runtime.activate_pending_cycle("turn-autoresearch".to_string())?);

    unload_thread(&mut mcp, &thread_id).await?;

    let read_id = mcp
        .send_autoresearch_read_request(AutoresearchReadParams {
            thread_id: thread_id.clone(),
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let AutoresearchReadResponse { run } = to_response::<AutoresearchReadResponse>(read_resp)?;

    let run = run.expect("autoresearch run after stale recovery");
    assert_eq!(run.status, AutoresearchStatus::Running);
    assert_eq!(run.active_turn_id, None);
    assert_eq!(run.last_submitted_turn_id, None);
    Ok(())
}

#[tokio::test]
async fn pilot_read_notifies_loaded_thread_after_stale_recovery() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_cwd = create_thread_cwd(codex_home.path())?;
    let thread_id = start_thread(&mut mcp, &thread_cwd).await?;
    let mut runtime = PilotRuntime::load(codex_home.path(), thread_id.as_str())?;
    assert!(runtime.start(
        "keep shipping".to_string(),
        /*deadline_at*/ None,
        Local::now(),
    )?);
    let _plan = runtime
        .prepare_cycle_submission(Local::now())?
        .expect("pilot cycle should be prepared");
    assert!(runtime.note_turn_submitted("turn-pilot")?);
    assert!(runtime.activate_pending_cycle("turn-pilot".to_string())?);
    mcp.clear_message_buffer();

    let read_id = mcp
        .send_pilot_read_request(PilotReadParams {
            thread_id: thread_id.clone(),
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let PilotReadResponse { run } = to_response::<PilotReadResponse>(read_resp)?;

    let run = run.expect("pilot run after stale recovery");
    assert_eq!(run.status, PilotStatus::Running);
    assert_eq!(run.active_turn_id, None);
    assert_eq!(run.last_submitted_turn_id, None);

    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("pilot/updated"),
    )
    .await??;
    let notification = parse_notification::<PilotUpdatedNotification>(notification)?;
    assert_eq!(notification.thread_id, thread_id);
    assert_eq!(notification.update_type, PilotUpdateType::Updated);
    let run = notification.run.expect("pilot notification run");
    assert_eq!(run.status, PilotStatus::Running);
    assert_eq!(run.active_turn_id, None);
    assert_eq!(run.last_submitted_turn_id, None);
    Ok(())
}

#[tokio::test]
async fn autoresearch_read_notifies_loaded_thread_after_stale_recovery() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    let workdir = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_cwd = create_thread_cwd(codex_home.path())?;
    let thread_id = start_thread(&mut mcp, &thread_cwd).await?;
    let mut runtime = AutoresearchRuntime::load(codex_home.path(), thread_id.as_str())?;
    AutoresearchResearchWorkspace::prepare(codex_home.path(), &thread_id, workdir.path())
        .map_err(anyhow::Error::msg)?;
    assert!(runtime.start(
        "map hypotheses".to_string(),
        CoreAutoresearchMode::Scientist,
        workdir.path().to_path_buf(),
        AutoresearchWorkspace {
            mode: AutoresearchWorkspaceMode::Filesystem,
            workdir: workdir.path().to_path_buf(),
            git_root: None,
            git_branch: None,
            accepted_revision: None,
            snapshot_root: Some(workdir.path().join("snapshot")),
        },
        /*max_runs*/ None,
        Local::now(),
    )?);
    let _plan = runtime
        .prepare_cycle_submission(Local::now())?
        .expect("autoresearch cycle should be prepared");
    assert!(runtime.note_turn_submitted("turn-autoresearch")?);
    assert!(runtime.activate_pending_cycle("turn-autoresearch".to_string())?);
    mcp.clear_message_buffer();

    let read_id = mcp
        .send_autoresearch_read_request(AutoresearchReadParams {
            thread_id: thread_id.clone(),
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let AutoresearchReadResponse { run } = to_response::<AutoresearchReadResponse>(read_resp)?;

    let run = run.expect("autoresearch run after stale recovery");
    assert_eq!(run.status, AutoresearchStatus::Running);
    assert_eq!(run.active_turn_id, None);
    assert_eq!(run.last_submitted_turn_id, None);

    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("autoresearch/updated"),
    )
    .await??;
    let notification = parse_notification::<AutoresearchUpdatedNotification>(notification)?;
    assert_eq!(notification.thread_id, thread_id);
    assert_eq!(notification.update_type, AutoresearchUpdateType::Updated);
    let run = notification.run.expect("autoresearch notification run");
    assert_eq!(run.status, AutoresearchStatus::Running);
    assert_eq!(run.active_turn_id, None);
    assert_eq!(run.last_submitted_turn_id, None);
    Ok(())
}

#[tokio::test]
async fn autoresearch_stop_clears_stale_running_state_after_thread_is_unloaded() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    let workdir = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_cwd = create_thread_cwd(codex_home.path())?;
    let thread_id = start_thread(&mut mcp, &thread_cwd).await?;
    let mut runtime = AutoresearchRuntime::load(codex_home.path(), thread_id.as_str())?;
    AutoresearchResearchWorkspace::prepare(codex_home.path(), &thread_id, workdir.path())
        .map_err(anyhow::Error::msg)?;
    assert!(runtime.start(
        "map hypotheses".to_string(),
        CoreAutoresearchMode::Scientist,
        workdir.path().to_path_buf(),
        AutoresearchWorkspace {
            mode: AutoresearchWorkspaceMode::Filesystem,
            workdir: workdir.path().to_path_buf(),
            git_root: None,
            git_branch: None,
            accepted_revision: None,
            snapshot_root: Some(workdir.path().join("snapshot")),
        },
        /*max_runs*/ None,
        Local::now(),
    )?);
    let _plan = runtime
        .prepare_cycle_submission(Local::now())?
        .expect("autoresearch cycle should be prepared");
    assert!(runtime.note_turn_submitted("turn-autoresearch")?);
    assert!(runtime.activate_pending_cycle("turn-autoresearch".to_string())?);

    unload_thread(&mut mcp, &thread_id).await?;

    let control_id = mcp
        .send_autoresearch_control_request(AutoresearchControlParams {
            thread_id: thread_id.clone(),
            action: AutoresearchControlAction::Stop,
            focus: None,
        })
        .await?;
    let control_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(control_id)),
    )
    .await??;
    let AutoresearchControlResponse { updated, run } =
        to_response::<AutoresearchControlResponse>(control_resp)?;

    assert!(updated);
    let run = run.expect("autoresearch run after stop");
    assert_eq!(run.status, AutoresearchStatus::Stopped);
    assert_eq!(run.active_turn_id, None);
    assert_eq!(run.last_submitted_turn_id, None);
    Ok(())
}

async fn unload_thread(mcp: &mut McpProcess, thread_id: &str) -> Result<()> {
    begin_unload_thread(mcp, thread_id).await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("thread/closed"),
    )
    .await??;
    Ok(())
}

async fn begin_unload_thread(mcp: &mut McpProcess, thread_id: &str) -> Result<()> {
    let unsubscribe_id = mcp
        .send_thread_unsubscribe_request(ThreadUnsubscribeParams {
            thread_id: thread_id.to_string(),
        })
        .await?;
    let unsubscribe_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(unsubscribe_id)),
    )
    .await??;
    let unsubscribe = to_response::<ThreadUnsubscribeResponse>(unsubscribe_resp)?;
    assert_eq!(unsubscribe.status, ThreadUnsubscribeStatus::Unsubscribed);
    Ok(())
}

fn create_config_toml(codex_home: &std::path::Path, server_uri: &str) -> std::io::Result<()> {
    let config_toml = codex_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "mock-model"
approval_policy = "never"
sandbox_mode = "danger-full-access"

model_provider = "mock_provider"

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#
        ),
    )
}

fn create_thread_cwd(codex_home: &std::path::Path) -> Result<std::path::PathBuf> {
    let cwd = codex_home.join("project");
    std::fs::create_dir_all(&cwd)?;
    Ok(cwd)
}

fn parse_notification<T>(notification: JSONRPCNotification) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let params = notification
        .params
        .context("notification missing params payload")?;
    Ok(serde_json::from_value(params)?)
}

async fn start_thread(mcp: &mut McpProcess, cwd: &std::path::Path) -> Result<String> {
    let req_id = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(cwd.display().to_string()),
            ..Default::default()
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(req_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(resp)?;
    let turn_start_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "materialize".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_start_id)),
    )
    .await??;
    let _: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp)?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    Ok(thread.id)
}

async fn start_turn_without_waiting(mcp: &mut McpProcess, thread_id: &str) -> Result<String> {
    let turn_start_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread_id.to_string(),
            input: vec![UserInput::Text {
                text: "keep running".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_start_id)),
    )
    .await??;
    let turn: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp)?;
    Ok(turn.turn.id)
}
