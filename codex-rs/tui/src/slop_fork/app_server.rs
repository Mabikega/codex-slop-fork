use crate::app_command::AppCommand;
use crate::app_command::AppCommandView;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::app_server_session::AppServerSession;
use crate::chatwidget::ChatWidget;
use crate::slop_fork::SlopForkEvent;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::Automation;
use codex_app_server_protocol::AutomationDeleteParams;
use codex_app_server_protocol::AutomationDeleteResponse;
use codex_app_server_protocol::AutomationListParams;
use codex_app_server_protocol::AutomationListResponse;
use codex_app_server_protocol::AutomationSetEnabledParams;
use codex_app_server_protocol::AutomationSetEnabledResponse;
use codex_app_server_protocol::AutomationUpsertParams;
use codex_app_server_protocol::AutomationUpsertResponse;
use codex_app_server_protocol::AutoresearchControlParams;
use codex_app_server_protocol::AutoresearchControlResponse;
use codex_app_server_protocol::AutoresearchReadParams;
use codex_app_server_protocol::AutoresearchReadResponse;
use codex_app_server_protocol::AutoresearchRun;
use codex_app_server_protocol::AutoresearchStartParams;
use codex_app_server_protocol::AutoresearchStartResponse;
use codex_app_server_protocol::AutoresearchStatus;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::PilotControlParams;
use codex_app_server_protocol::PilotControlResponse;
use codex_app_server_protocol::PilotReadParams;
use codex_app_server_protocol::PilotReadResponse;
use codex_app_server_protocol::PilotRun;
use codex_app_server_protocol::PilotStartParams;
use codex_app_server_protocol::PilotStartResponse;
use codex_app_server_protocol::PilotStatus;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SlopForkAssistantTurnKind;
use codex_protocol::ThreadId;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use uuid::Uuid;

#[derive(Debug, Default)]
pub(crate) struct SlopForkAppServerState {
    next_remote_bootstrap_request_nonce: u64,
    pending_remote_automation_bootstrap: Option<(String, u64)>,
    pending_remote_autoresearch_bootstrap: Option<(String, u64)>,
    pending_remote_pilot_bootstrap: Option<(String, u64)>,
}

#[derive(Clone, Copy)]
enum RemotePilotMutationKind {
    Start,
    Pause,
    Resume,
    WrapUp,
    Stop,
}

#[derive(Clone, Copy)]
enum RemoteAutoresearchMutationKind {
    Start,
    Pause,
    Resume,
    WrapUp,
    Stop,
    Clear,
    Discover,
}

struct RemotePilotMutationResult {
    updated: bool,
    authoritative: bool,
    run: Option<PilotRun>,
}

struct RemoteAutoresearchMutationResult {
    updated: bool,
    authoritative: bool,
    run: Option<AutoresearchRun>,
}

pub(crate) async fn try_submit_app_server_op(
    chat_widget: &mut ChatWidget,
    app_server: &mut AppServerSession,
    thread_id: ThreadId,
    op: &AppCommand,
) -> Result<bool> {
    match op.view() {
        AppCommandView::SlopForkPilotTurn { prompt } => {
            if let Err(err) = app_server
                .slop_fork_assistant_turn_start(
                    thread_id,
                    prompt.to_string(),
                    SlopForkAssistantTurnKind::Pilot,
                )
                .await
            {
                chat_widget.on_pilot_turn_submission_rpc_failed();
                return Err(err);
            }
            Ok(true)
        }
        AppCommandView::SlopForkAutoresearchTurn { prompt } => {
            if let Err(err) = app_server
                .slop_fork_assistant_turn_start(
                    thread_id,
                    prompt.to_string(),
                    SlopForkAssistantTurnKind::Autoresearch,
                )
                .await
            {
                chat_widget.on_autoresearch_turn_submission_rpc_failed();
                return Err(err);
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

impl SlopForkAppServerState {
    pub(crate) fn handle_app_event(
        &mut self,
        chat_widget: &mut ChatWidget,
        app_server: Option<&AppServerSession>,
        app_event_tx: &AppEventSender,
        event: SlopForkEvent,
    ) -> Option<SlopForkEvent> {
        match event {
            SlopForkEvent::FetchRemoteAutomationState { thread_id } => {
                let Some(app_server) = app_server else {
                    return self.missing_app_server(
                        chat_widget,
                        "remote automation bootstrap requires app-server session",
                    );
                };
                self.fetch_remote_automation_state(app_server, app_event_tx, thread_id);
                None
            }
            SlopForkEvent::RemoteAutomationStateLoaded {
                thread_id,
                request_nonce,
                result,
            } => {
                if self.pending_remote_automation_bootstrap.as_ref()
                    == Some(&(thread_id.clone(), request_nonce))
                {
                    self.pending_remote_automation_bootstrap = None;
                    if let Some(active_thread_id) = chat_widget.thread_id()
                        && active_thread_id.to_string() == thread_id
                    {
                        match result {
                            Ok(automations) => {
                                let _ = chat_widget.on_remote_automation_state_loaded(automations);
                            }
                            Err(err) => chat_widget.add_error_message(err),
                        }
                    }
                }
                None
            }
            SlopForkEvent::FetchRemoteAutoresearchState { thread_id } => {
                let Some(app_server) = app_server else {
                    return self.missing_app_server(
                        chat_widget,
                        "remote autoresearch bootstrap requires app-server session",
                    );
                };
                self.fetch_remote_autoresearch_state(app_server, app_event_tx, thread_id);
                None
            }
            SlopForkEvent::RemoteAutoresearchStateLoaded {
                thread_id,
                request_nonce,
                source,
                report_error,
                result,
            } => {
                if self.pending_remote_autoresearch_bootstrap.as_ref()
                    == Some(&(thread_id.clone(), request_nonce))
                {
                    self.pending_remote_autoresearch_bootstrap = None;
                    if let Some(active_thread_id) = chat_widget.thread_id()
                        && active_thread_id.to_string() == thread_id
                    {
                        match result {
                            Ok(run) => {
                                let _ = match source {
                                    crate::slop_fork::event::RemoteStateLoadSource::Bootstrap => {
                                        chat_widget.on_remote_autoresearch_state_loaded(run)
                                    }
                                    crate::slop_fork::event::RemoteStateLoadSource::ActionResponse => {
                                        chat_widget.on_remote_autoresearch_action_state_loaded(run)
                                    }
                                };
                            }
                            Err(err) => {
                                let _ = chat_widget.on_remote_autoresearch_state_load_failed();
                                if report_error {
                                    chat_widget.add_error_message(err);
                                }
                            }
                        }
                    }
                }
                None
            }
            SlopForkEvent::FetchRemotePilotState { thread_id } => {
                let Some(app_server) = app_server else {
                    return self.missing_app_server(
                        chat_widget,
                        "remote pilot bootstrap requires app-server session",
                    );
                };
                self.fetch_remote_pilot_state(app_server, app_event_tx, thread_id);
                None
            }
            SlopForkEvent::RemotePilotStateLoaded {
                thread_id,
                request_nonce,
                source,
                report_error,
                result,
            } => {
                if self.pending_remote_pilot_bootstrap.as_ref()
                    == Some(&(thread_id.clone(), request_nonce))
                {
                    self.pending_remote_pilot_bootstrap = None;
                    if let Some(active_thread_id) = chat_widget.thread_id()
                        && active_thread_id.to_string() == thread_id
                    {
                        match result {
                            Ok(run) => {
                                let _ = match source {
                                    crate::slop_fork::event::RemoteStateLoadSource::Bootstrap => {
                                        chat_widget.on_remote_pilot_state_loaded(run)
                                    }
                                    crate::slop_fork::event::RemoteStateLoadSource::ActionResponse => {
                                        chat_widget.on_remote_pilot_action_state_loaded(run)
                                    }
                                };
                            }
                            Err(err) => {
                                let _ = chat_widget.on_remote_pilot_state_load_failed();
                                if report_error {
                                    chat_widget.add_error_message(err);
                                }
                            }
                        }
                    }
                }
                None
            }
            SlopForkEvent::StartRemotePilot {
                thread_id,
                goal,
                deadline_at,
            } => {
                let Some(app_server) = app_server else {
                    return self.missing_app_server(
                        chat_widget,
                        "remote pilot start requires app-server session",
                    );
                };
                self.refresh_remote_pilot_after_action(
                    app_server,
                    app_event_tx,
                    thread_id.clone(),
                    RemotePilotMutationKind::Start,
                    move |request_handle| async move {
                        start_remote_pilot(request_handle, thread_id, goal, deadline_at).await
                    },
                );
                chat_widget.arm_remote_pilot_state_reload();
                None
            }
            SlopForkEvent::ControlRemotePilot { thread_id, action } => {
                let Some(app_server) = app_server else {
                    return self.missing_app_server(
                        chat_widget,
                        "remote pilot control requires app-server session",
                    );
                };
                let mutation_kind = match action {
                    codex_app_server_protocol::PilotControlAction::Pause => {
                        RemotePilotMutationKind::Pause
                    }
                    codex_app_server_protocol::PilotControlAction::Resume => {
                        RemotePilotMutationKind::Resume
                    }
                    codex_app_server_protocol::PilotControlAction::WrapUp => {
                        RemotePilotMutationKind::WrapUp
                    }
                    codex_app_server_protocol::PilotControlAction::Stop => {
                        RemotePilotMutationKind::Stop
                    }
                };
                self.refresh_remote_pilot_after_action(
                    app_server,
                    app_event_tx,
                    thread_id.clone(),
                    mutation_kind,
                    move |request_handle| async move {
                        control_remote_pilot(request_handle, thread_id, action).await
                    },
                );
                chat_widget.arm_remote_pilot_state_reload();
                None
            }
            SlopForkEvent::StartRemoteAutoresearch {
                thread_id,
                goal,
                max_runs,
                mode,
            } => {
                let Some(app_server) = app_server else {
                    return self.missing_app_server(
                        chat_widget,
                        "remote autoresearch start requires app-server session",
                    );
                };
                self.refresh_remote_autoresearch_after_action(
                    app_server,
                    app_event_tx,
                    thread_id.clone(),
                    RemoteAutoresearchMutationKind::Start,
                    move |request_handle| async move {
                        start_remote_autoresearch(request_handle, thread_id, goal, max_runs, mode)
                            .await
                    },
                );
                chat_widget.arm_remote_autoresearch_state_reload();
                None
            }
            SlopForkEvent::ControlRemoteAutoresearch {
                thread_id,
                action,
                focus,
            } => {
                let Some(app_server) = app_server else {
                    return self.missing_app_server(
                        chat_widget,
                        "remote autoresearch control requires app-server session",
                    );
                };
                self.refresh_remote_autoresearch_after_action(
                    app_server,
                    app_event_tx,
                    thread_id.clone(),
                    match action {
                        codex_app_server_protocol::AutoresearchControlAction::Pause => {
                            RemoteAutoresearchMutationKind::Pause
                        }
                        codex_app_server_protocol::AutoresearchControlAction::Resume => {
                            RemoteAutoresearchMutationKind::Resume
                        }
                        codex_app_server_protocol::AutoresearchControlAction::WrapUp => {
                            RemoteAutoresearchMutationKind::WrapUp
                        }
                        codex_app_server_protocol::AutoresearchControlAction::Stop => {
                            RemoteAutoresearchMutationKind::Stop
                        }
                        codex_app_server_protocol::AutoresearchControlAction::Clear => {
                            RemoteAutoresearchMutationKind::Clear
                        }
                        codex_app_server_protocol::AutoresearchControlAction::Discover => {
                            RemoteAutoresearchMutationKind::Discover
                        }
                    },
                    move |request_handle| async move {
                        control_remote_autoresearch(request_handle, thread_id, action, focus).await
                    },
                );
                chat_widget.arm_remote_autoresearch_state_reload();
                None
            }
            SlopForkEvent::UpsertRemoteAutomation {
                thread_id,
                scope,
                automation,
            } => {
                let Some(app_server) = app_server else {
                    return self.missing_app_server(
                        chat_widget,
                        "remote automation upsert requires app-server session",
                    );
                };
                let request_handle = app_server.request_handle();
                let app_event_tx = app_event_tx.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        upsert_remote_automation(request_handle, thread_id, scope, automation).await
                    {
                        send_remote_action_failed(
                            &app_event_tx,
                            format!("automation/upsert failed in TUI: {err}"),
                        );
                    }
                });
                None
            }
            SlopForkEvent::SetRemoteAutomationEnabled {
                thread_id,
                runtime_id,
                enabled,
            } => {
                let Some(app_server) = app_server else {
                    return self.missing_app_server(
                        chat_widget,
                        "remote automation setEnabled requires app-server session",
                    );
                };
                let request_handle = app_server.request_handle();
                let app_event_tx = app_event_tx.clone();
                tokio::spawn(async move {
                    if let Err(err) = set_remote_automation_enabled(
                        request_handle,
                        thread_id,
                        runtime_id,
                        enabled,
                    )
                    .await
                    {
                        send_remote_action_failed(
                            &app_event_tx,
                            format!("automation/setEnabled failed in TUI: {err}"),
                        );
                    }
                });
                None
            }
            SlopForkEvent::DeleteRemoteAutomation {
                thread_id,
                runtime_id,
            } => {
                let Some(app_server) = app_server else {
                    return self.missing_app_server(
                        chat_widget,
                        "remote automation delete requires app-server session",
                    );
                };
                let request_handle = app_server.request_handle();
                let app_event_tx = app_event_tx.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        delete_remote_automation(request_handle, thread_id, runtime_id).await
                    {
                        send_remote_action_failed(
                            &app_event_tx,
                            format!("automation/delete failed in TUI: {err}"),
                        );
                    }
                });
                None
            }
            SlopForkEvent::RemoteActionFailed { message } => {
                chat_widget.add_error_message(message);
                None
            }
            other => Some(other),
        }
    }

    fn fetch_remote_automation_state(
        &mut self,
        app_server: &AppServerSession,
        app_event_tx: &AppEventSender,
        thread_id: String,
    ) {
        let request_nonce = self.next_request_nonce();
        self.pending_remote_automation_bootstrap = Some((thread_id.clone(), request_nonce));
        let request_handle = app_server.request_handle();
        let app_event_tx = app_event_tx.clone();
        tokio::spawn(async move {
            let result = fetch_remote_automation_state(request_handle, thread_id.clone())
                .await
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::SlopFork(
                SlopForkEvent::RemoteAutomationStateLoaded {
                    thread_id,
                    request_nonce,
                    result,
                },
            ));
        });
    }

    fn fetch_remote_autoresearch_state(
        &mut self,
        app_server: &AppServerSession,
        app_event_tx: &AppEventSender,
        thread_id: String,
    ) {
        let request_nonce = self.next_request_nonce();
        self.pending_remote_autoresearch_bootstrap = Some((thread_id.clone(), request_nonce));
        let request_handle = app_server.request_handle();
        let app_event_tx = app_event_tx.clone();
        tokio::spawn(async move {
            let result = fetch_remote_autoresearch_state(request_handle, thread_id.clone())
                .await
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::SlopFork(
                SlopForkEvent::RemoteAutoresearchStateLoaded {
                    thread_id,
                    request_nonce,
                    source: crate::slop_fork::event::RemoteStateLoadSource::Bootstrap,
                    report_error: true,
                    result,
                },
            ));
        });
    }

    fn fetch_remote_pilot_state(
        &mut self,
        app_server: &AppServerSession,
        app_event_tx: &AppEventSender,
        thread_id: String,
    ) {
        let request_nonce = self.next_request_nonce();
        self.pending_remote_pilot_bootstrap = Some((thread_id.clone(), request_nonce));
        let request_handle = app_server.request_handle();
        let app_event_tx = app_event_tx.clone();
        tokio::spawn(async move {
            let result = fetch_remote_pilot_state(request_handle, thread_id.clone())
                .await
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::SlopFork(SlopForkEvent::RemotePilotStateLoaded {
                thread_id,
                request_nonce,
                source: crate::slop_fork::event::RemoteStateLoadSource::Bootstrap,
                report_error: true,
                result,
            }));
        });
    }

    fn refresh_remote_pilot_after_action<F, Fut>(
        &mut self,
        app_server: &AppServerSession,
        app_event_tx: &AppEventSender,
        thread_id: String,
        mutation_kind: RemotePilotMutationKind,
        action: F,
    ) where
        F: FnOnce(AppServerRequestHandle) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<RemotePilotMutationResult>> + Send + 'static,
    {
        let request_nonce = self.next_request_nonce();
        self.pending_remote_pilot_bootstrap = Some((thread_id.clone(), request_nonce));
        let request_handle = app_server.request_handle();
        let app_event_tx = app_event_tx.clone();
        tokio::spawn(async move {
            let mutation_result = match action(request_handle.clone()).await {
                Ok(result) => result,
                Err(err) => {
                    let error = format!("pilot request failed in TUI: {err}");
                    send_remote_action_failed(&app_event_tx, error.clone());
                    app_event_tx.send(AppEvent::SlopFork(SlopForkEvent::RemotePilotStateLoaded {
                        thread_id,
                        request_nonce,
                        source: crate::slop_fork::event::RemoteStateLoadSource::ActionResponse,
                        report_error: false,
                        result: Err(error),
                    }));
                    return;
                }
            };
            let updated = mutation_result.updated;
            let result =
                remote_pilot_state_after_action(mutation_result, request_handle, &thread_id).await;
            if !updated && let Ok(run) = result.as_ref() {
                send_remote_action_failed(
                    &app_event_tx,
                    remote_pilot_noop_message(mutation_kind, run.as_ref()),
                );
            }
            app_event_tx.send(AppEvent::SlopFork(SlopForkEvent::RemotePilotStateLoaded {
                thread_id,
                request_nonce,
                source: crate::slop_fork::event::RemoteStateLoadSource::ActionResponse,
                report_error: true,
                result,
            }));
        });
    }

    fn refresh_remote_autoresearch_after_action<F, Fut>(
        &mut self,
        app_server: &AppServerSession,
        app_event_tx: &AppEventSender,
        thread_id: String,
        mutation_kind: RemoteAutoresearchMutationKind,
        action: F,
    ) where
        F: FnOnce(AppServerRequestHandle) -> Fut + Send + 'static,
        Fut:
            std::future::Future<Output = Result<RemoteAutoresearchMutationResult>> + Send + 'static,
    {
        let request_nonce = self.next_request_nonce();
        self.pending_remote_autoresearch_bootstrap = Some((thread_id.clone(), request_nonce));
        let request_handle = app_server.request_handle();
        let app_event_tx = app_event_tx.clone();
        tokio::spawn(async move {
            let mutation_result = match action(request_handle.clone()).await {
                Ok(result) => result,
                Err(err) => {
                    let error = format!("autoresearch request failed in TUI: {err}");
                    send_remote_action_failed(&app_event_tx, error.clone());
                    app_event_tx.send(AppEvent::SlopFork(
                        SlopForkEvent::RemoteAutoresearchStateLoaded {
                            thread_id,
                            request_nonce,
                            source: crate::slop_fork::event::RemoteStateLoadSource::ActionResponse,
                            report_error: false,
                            result: Err(error),
                        },
                    ));
                    return;
                }
            };
            let updated = mutation_result.updated;
            let result =
                remote_autoresearch_state_after_action(mutation_result, request_handle, &thread_id)
                    .await;
            if !updated && let Ok(run) = result.as_ref() {
                send_remote_action_failed(
                    &app_event_tx,
                    remote_autoresearch_noop_message(mutation_kind, run.as_ref()),
                );
            }
            app_event_tx.send(AppEvent::SlopFork(
                SlopForkEvent::RemoteAutoresearchStateLoaded {
                    thread_id,
                    request_nonce,
                    source: crate::slop_fork::event::RemoteStateLoadSource::ActionResponse,
                    report_error: true,
                    result,
                },
            ));
        });
    }

    fn next_request_nonce(&mut self) -> u64 {
        let request_nonce = self.next_remote_bootstrap_request_nonce;
        self.next_remote_bootstrap_request_nonce += 1;
        request_nonce
    }

    fn missing_app_server(
        &self,
        chat_widget: &mut ChatWidget,
        message: &str,
    ) -> Option<SlopForkEvent> {
        debug_assert!(false, "{message}");
        chat_widget.add_error_message(message.to_string());
        None
    }

    #[cfg(test)]
    pub(crate) fn seed_pending_remote_automation_bootstrap(
        &mut self,
        thread_id: String,
        request_nonce: u64,
    ) {
        self.pending_remote_automation_bootstrap = Some((thread_id, request_nonce));
    }

    #[cfg(test)]
    pub(crate) fn seed_pending_remote_autoresearch_bootstrap(
        &mut self,
        thread_id: String,
        request_nonce: u64,
    ) {
        self.pending_remote_autoresearch_bootstrap = Some((thread_id, request_nonce));
    }

    #[cfg(test)]
    pub(crate) fn seed_pending_remote_pilot_bootstrap(
        &mut self,
        thread_id: String,
        request_nonce: u64,
    ) {
        self.pending_remote_pilot_bootstrap = Some((thread_id, request_nonce));
    }
}

async fn fetch_remote_automation_state(
    request_handle: AppServerRequestHandle,
    thread_id: String,
) -> Result<Vec<Automation>> {
    let request_id = RequestId::String(format!("automation-list-{}", Uuid::new_v4()));
    let response: AutomationListResponse = request_handle
        .request_typed(ClientRequest::AutomationList {
            request_id,
            params: AutomationListParams { thread_id },
        })
        .await
        .wrap_err("automation/list failed in TUI")?;
    Ok(response.data)
}

async fn fetch_remote_autoresearch_state(
    request_handle: AppServerRequestHandle,
    thread_id: String,
) -> Result<Option<AutoresearchRun>> {
    let request_id = RequestId::String(format!("autoresearch-read-{}", Uuid::new_v4()));
    let response: AutoresearchReadResponse = request_handle
        .request_typed(ClientRequest::AutoresearchRead {
            request_id,
            params: AutoresearchReadParams { thread_id },
        })
        .await
        .wrap_err("autoresearch/read failed in TUI")?;
    Ok(response.run)
}

async fn fetch_remote_pilot_state(
    request_handle: AppServerRequestHandle,
    thread_id: String,
) -> Result<Option<PilotRun>> {
    let request_id = RequestId::String(format!("pilot-read-{}", Uuid::new_v4()));
    let response: PilotReadResponse = request_handle
        .request_typed(ClientRequest::PilotRead {
            request_id,
            params: PilotReadParams { thread_id },
        })
        .await
        .wrap_err("pilot/read failed in TUI")?;
    Ok(response.run)
}

async fn remote_pilot_state_after_action(
    mutation_result: RemotePilotMutationResult,
    request_handle: AppServerRequestHandle,
    thread_id: &str,
) -> std::result::Result<Option<PilotRun>, String> {
    if mutation_result.authoritative {
        return Ok(mutation_result.run);
    }
    let readback_result = fetch_remote_pilot_state(request_handle, thread_id.to_string())
        .await
        .map_err(|err| err.to_string());
    resolve_remote_pilot_state_after_action(mutation_result, readback_result)
}

fn resolve_remote_pilot_state_after_action(
    mutation_result: RemotePilotMutationResult,
    readback_result: std::result::Result<Option<PilotRun>, String>,
) -> std::result::Result<Option<PilotRun>, String> {
    if mutation_result.authoritative {
        Ok(mutation_result.run)
    } else {
        readback_result
    }
}

async fn remote_autoresearch_state_after_action(
    mutation_result: RemoteAutoresearchMutationResult,
    request_handle: AppServerRequestHandle,
    thread_id: &str,
) -> std::result::Result<Option<AutoresearchRun>, String> {
    if mutation_result.authoritative {
        return Ok(mutation_result.run);
    }
    let readback_result = fetch_remote_autoresearch_state(request_handle, thread_id.to_string())
        .await
        .map_err(|err| err.to_string());
    resolve_remote_autoresearch_state_after_action(mutation_result, readback_result)
}

fn resolve_remote_autoresearch_state_after_action(
    mutation_result: RemoteAutoresearchMutationResult,
    readback_result: std::result::Result<Option<AutoresearchRun>, String>,
) -> std::result::Result<Option<AutoresearchRun>, String> {
    if mutation_result.authoritative {
        Ok(mutation_result.run)
    } else {
        readback_result
    }
}

async fn start_remote_pilot(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    goal: String,
    deadline_at: Option<i64>,
) -> Result<RemotePilotMutationResult> {
    let request_id = RequestId::String(format!("pilot-start-{}", Uuid::new_v4()));
    let response: PilotStartResponse = request_handle
        .request_typed(ClientRequest::PilotStart {
            request_id,
            params: PilotStartParams {
                thread_id,
                goal,
                deadline_at,
            },
        })
        .await
        .wrap_err("pilot/start failed in TUI")?;
    Ok(RemotePilotMutationResult {
        updated: true,
        authoritative: true,
        run: Some(response.run),
    })
}

async fn control_remote_pilot(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    action: codex_app_server_protocol::PilotControlAction,
) -> Result<RemotePilotMutationResult> {
    let request_id = RequestId::String(format!("pilot-control-{}", Uuid::new_v4()));
    let response: PilotControlResponse = request_handle
        .request_typed(ClientRequest::PilotControl {
            request_id,
            params: PilotControlParams { thread_id, action },
        })
        .await
        .wrap_err("pilot/control failed in TUI")?;
    Ok(RemotePilotMutationResult {
        updated: response.updated,
        authoritative: true,
        run: response.run,
    })
}

async fn start_remote_autoresearch(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    goal: String,
    max_runs: Option<u32>,
    mode: codex_app_server_protocol::AutoresearchMode,
) -> Result<RemoteAutoresearchMutationResult> {
    let request_id = RequestId::String(format!("autoresearch-start-{}", Uuid::new_v4()));
    let response: AutoresearchStartResponse = request_handle
        .request_typed(ClientRequest::AutoresearchStart {
            request_id,
            params: AutoresearchStartParams {
                thread_id,
                goal,
                mode,
                max_runs,
            },
        })
        .await
        .wrap_err("autoresearch/start failed in TUI")?;
    Ok(RemoteAutoresearchMutationResult {
        updated: response.updated,
        authoritative: true,
        run: response.run,
    })
}

async fn control_remote_autoresearch(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    action: codex_app_server_protocol::AutoresearchControlAction,
    focus: Option<String>,
) -> Result<RemoteAutoresearchMutationResult> {
    let request_id = RequestId::String(format!("autoresearch-control-{}", Uuid::new_v4()));
    let response: AutoresearchControlResponse = request_handle
        .request_typed(ClientRequest::AutoresearchControl {
            request_id,
            params: AutoresearchControlParams {
                thread_id,
                action,
                focus,
            },
        })
        .await
        .wrap_err("autoresearch/control failed in TUI")?;
    Ok(RemoteAutoresearchMutationResult {
        updated: response.updated,
        authoritative: true,
        run: response.run,
    })
}

async fn upsert_remote_automation(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    scope: codex_app_server_protocol::AutomationScope,
    automation: codex_app_server_protocol::AutomationDefinition,
) -> Result<()> {
    let request_id = RequestId::String(format!("automation-upsert-{}", Uuid::new_v4()));
    let _: AutomationUpsertResponse = request_handle
        .request_typed(ClientRequest::AutomationUpsert {
            request_id,
            params: AutomationUpsertParams {
                thread_id,
                scope,
                automation,
            },
        })
        .await
        .wrap_err("automation/upsert failed in TUI")?;
    Ok(())
}

async fn set_remote_automation_enabled(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    runtime_id: String,
    enabled: bool,
) -> Result<()> {
    let request_id = RequestId::String(format!("automation-set-enabled-{}", Uuid::new_v4()));
    let _: AutomationSetEnabledResponse = request_handle
        .request_typed(ClientRequest::AutomationSetEnabled {
            request_id,
            params: AutomationSetEnabledParams {
                thread_id,
                runtime_id,
                enabled,
            },
        })
        .await
        .wrap_err("automation/setEnabled failed in TUI")?;
    Ok(())
}

async fn delete_remote_automation(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    runtime_id: String,
) -> Result<()> {
    let request_id = RequestId::String(format!("automation-delete-{}", Uuid::new_v4()));
    let response: AutomationDeleteResponse = request_handle
        .request_typed(ClientRequest::AutomationDelete {
            request_id,
            params: AutomationDeleteParams {
                thread_id,
                runtime_id,
            },
        })
        .await
        .wrap_err("automation/delete failed in TUI")?;
    if !response.deleted {
        color_eyre::eyre::bail!("remote automation was not deleted");
    }
    Ok(())
}

fn remote_pilot_noop_message(
    mutation_kind: RemotePilotMutationKind,
    run: Option<&PilotRun>,
) -> String {
    match mutation_kind {
        RemotePilotMutationKind::Start => {
            "Pilot start request did not change server state.".to_string()
        }
        RemotePilotMutationKind::Pause => {
            if run.is_none_or(|run| {
                matches!(run.status, PilotStatus::Stopped | PilotStatus::Completed)
            }) {
                "Pilot is not active.".to_string()
            } else {
                "Pilot pause request did not change server state.".to_string()
            }
        }
        RemotePilotMutationKind::Resume => {
            if run.is_none_or(|run| {
                matches!(run.status, PilotStatus::Stopped | PilotStatus::Completed)
            }) {
                "Pilot is not active.".to_string()
            } else {
                "Pilot cannot be resumed in its current state.".to_string()
            }
        }
        RemotePilotMutationKind::WrapUp => {
            if run.is_none_or(|run| {
                matches!(run.status, PilotStatus::Stopped | PilotStatus::Completed)
            }) {
                "Pilot is not active.".to_string()
            } else {
                "Pilot cannot wrap up in its current state.".to_string()
            }
        }
        RemotePilotMutationKind::Stop => {
            if run.is_none_or(|run| {
                matches!(run.status, PilotStatus::Stopped | PilotStatus::Completed)
            }) {
                "Pilot is not active.".to_string()
            } else {
                "Pilot stop request did not change server state.".to_string()
            }
        }
    }
}

fn remote_autoresearch_noop_message(
    mutation_kind: RemoteAutoresearchMutationKind,
    run: Option<&AutoresearchRun>,
) -> String {
    match mutation_kind {
        RemoteAutoresearchMutationKind::Start => {
            if run.is_some() {
                "Autoresearch already has an active session.".to_string()
            } else {
                "Autoresearch start request did not change server state.".to_string()
            }
        }
        RemoteAutoresearchMutationKind::Pause => {
            if run.is_none_or(|run| {
                matches!(
                    run.status,
                    AutoresearchStatus::Stopped | AutoresearchStatus::Completed
                )
            }) {
                "Autoresearch is not active.".to_string()
            } else {
                "Autoresearch pause request did not change server state.".to_string()
            }
        }
        RemoteAutoresearchMutationKind::Resume => {
            if run.is_none_or(|run| {
                matches!(
                    run.status,
                    AutoresearchStatus::Stopped | AutoresearchStatus::Completed
                )
            }) {
                "Autoresearch is not active.".to_string()
            } else {
                "Autoresearch cannot be resumed in its current state.".to_string()
            }
        }
        RemoteAutoresearchMutationKind::WrapUp => {
            if run.is_none_or(|run| {
                matches!(
                    run.status,
                    AutoresearchStatus::Stopped | AutoresearchStatus::Completed
                )
            }) {
                "Autoresearch is not active.".to_string()
            } else {
                "Autoresearch cannot wrap up in its current state.".to_string()
            }
        }
        RemoteAutoresearchMutationKind::Stop => {
            if run.is_none_or(|run| {
                matches!(
                    run.status,
                    AutoresearchStatus::Stopped | AutoresearchStatus::Completed
                )
            }) {
                "Autoresearch is not active.".to_string()
            } else {
                "Autoresearch stop request did not change server state.".to_string()
            }
        }
        RemoteAutoresearchMutationKind::Clear => {
            if run.is_none() {
                "Autoresearch is already clear.".to_string()
            } else {
                "Autoresearch clear request did not change server state.".to_string()
            }
        }
        RemoteAutoresearchMutationKind::Discover => {
            "Autoresearch could not queue discovery in its current state.".to_string()
        }
    }
}

fn send_remote_action_failed(app_event_tx: &AppEventSender, message: String) {
    app_event_tx.send(AppEvent::SlopFork(SlopForkEvent::RemoteActionFailed {
        message,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn pilot_noop_message_reports_inactive_state() {
        assert_eq!(
            remote_pilot_noop_message(RemotePilotMutationKind::Pause, None),
            "Pilot is not active."
        );
    }

    #[test]
    fn autoresearch_noop_message_reports_resume_failure() {
        assert_eq!(
            remote_autoresearch_noop_message(
                RemoteAutoresearchMutationKind::Resume,
                Some(&AutoresearchRun {
                    goal: "resume me".to_string(),
                    mode: codex_app_server_protocol::AutoresearchMode::Scientist,
                    status: AutoresearchStatus::Running,
                    started_at: 10,
                    updated_at: 11,
                    max_runs: None,
                    iteration_count: 1,
                    discovery_count: 0,
                    pending_cycle_kind: None,
                    active_cycle_kind: None,
                    active_turn_id: None,
                    last_submitted_turn_id: None,
                    wrap_up_requested: false,
                    stop_requested_at: None,
                    last_error: None,
                    status_message: None,
                    last_progress_at: None,
                    last_cycle_completed_at: None,
                    last_discovery_completed_at: None,
                    last_cycle_summary: None,
                    last_agent_message: None,
                }),
            ),
            "Autoresearch cannot be resumed in its current state."
        );
    }

    #[test]
    fn pilot_action_prefers_authoritative_mutation_run_over_readback() {
        let result = resolve_remote_pilot_state_after_action(
            RemotePilotMutationResult {
                updated: true,
                authoritative: true,
                run: Some(PilotRun {
                    goal: "authoritative".to_string(),
                    status: PilotStatus::Running,
                    started_at: 10,
                    deadline_at: None,
                    updated_at: 11,
                    iteration_count: 1,
                    pending_cycle_kind: None,
                    active_cycle_kind: None,
                    active_turn_id: None,
                    last_submitted_turn_id: None,
                    wrap_up_requested: false,
                    wrap_up_requested_at: None,
                    stop_requested_at: None,
                    last_error: None,
                    status_message: None,
                    last_progress_at: None,
                    last_cycle_completed_at: None,
                    last_cycle_summary: None,
                    last_cycle_kind: None,
                    last_agent_message: None,
                }),
            },
            Err("pilot/read failed".to_string()),
        )
        .expect("authoritative run should be accepted");

        assert_eq!(
            result.as_ref().map(|run| run.goal.as_str()),
            Some("authoritative")
        );
    }

    #[test]
    fn autoresearch_action_prefers_authoritative_mutation_run_over_readback() {
        let result = resolve_remote_autoresearch_state_after_action(
            RemoteAutoresearchMutationResult {
                updated: true,
                authoritative: true,
                run: Some(AutoresearchRun {
                    goal: "authoritative".to_string(),
                    mode: codex_app_server_protocol::AutoresearchMode::Scientist,
                    status: AutoresearchStatus::Running,
                    started_at: 10,
                    updated_at: 11,
                    max_runs: None,
                    iteration_count: 1,
                    discovery_count: 0,
                    pending_cycle_kind: None,
                    active_cycle_kind: None,
                    active_turn_id: None,
                    last_submitted_turn_id: None,
                    wrap_up_requested: false,
                    stop_requested_at: None,
                    last_error: None,
                    status_message: None,
                    last_progress_at: None,
                    last_cycle_completed_at: None,
                    last_discovery_completed_at: None,
                    last_cycle_summary: None,
                    last_agent_message: None,
                }),
            },
            Err("autoresearch/read failed".to_string()),
        )
        .expect("authoritative run should be accepted");

        assert_eq!(
            result.as_ref().map(|run| run.goal.as_str()),
            Some("authoritative")
        );
    }

    #[test]
    fn autoresearch_clear_prefers_authoritative_empty_state_over_readback() {
        let result = resolve_remote_autoresearch_state_after_action(
            RemoteAutoresearchMutationResult {
                updated: true,
                authoritative: true,
                run: None,
            },
            Err("autoresearch/read failed".to_string()),
        )
        .expect("authoritative clear should be accepted");

        assert_eq!(result, None);
    }

    #[test]
    fn pilot_noop_control_prefers_authoritative_run_over_readback() {
        let result = resolve_remote_pilot_state_after_action(
            RemotePilotMutationResult {
                updated: false,
                authoritative: true,
                run: None,
            },
            Err("pilot/read failed".to_string()),
        )
        .expect("authoritative pilot control result should be accepted");

        assert_eq!(result, None);
    }

    #[test]
    fn autoresearch_noop_control_prefers_authoritative_run_over_readback() {
        let result = resolve_remote_autoresearch_state_after_action(
            RemoteAutoresearchMutationResult {
                updated: false,
                authoritative: true,
                run: None,
            },
            Err("autoresearch/read failed".to_string()),
        )
        .expect("authoritative autoresearch control result should be accepted");

        assert_eq!(result, None);
    }
}
