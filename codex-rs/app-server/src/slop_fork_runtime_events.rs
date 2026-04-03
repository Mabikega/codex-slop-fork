use crate::slop_fork_automation::SlopForkAutomationManager;
use crate::slop_fork_autoresearch::SlopForkAutoresearchManager;
use crate::slop_fork_pilot::SlopForkPilotManager;
use codex_app_server_protocol::AutomationUpdatedNotification;
use codex_app_server_protocol::AutoresearchUpdatedNotification;
use codex_app_server_protocol::PilotUpdatedNotification;
use codex_app_server_protocol::TurnError;
use codex_core::CodexThread;
use codex_core::slop_fork::SlopForkConfig;
use codex_protocol::ThreadId;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::protocol::EventMsg;
use std::path::Path;
use std::path::PathBuf;
use tracing::warn;

#[derive(Default)]
pub(crate) struct SlopForkRuntimeEventEffects {
    pub(crate) automation_notifications: Vec<AutomationUpdatedNotification>,
    pub(crate) autoresearch_notification: Option<AutoresearchUpdatedNotification>,
    pub(crate) pilot_notification: Option<PilotUpdatedNotification>,
}

pub(crate) struct SlopForkRuntimeEventHandler<'a> {
    pub(crate) automation_manager: &'a SlopForkAutomationManager,
    pub(crate) autoresearch_manager: &'a SlopForkAutoresearchManager,
    pub(crate) pilot_manager: &'a SlopForkPilotManager,
    pub(crate) codex_home: &'a Path,
    pub(crate) thread: &'a CodexThread,
    pub(crate) thread_id: &'a ThreadId,
    pub(crate) fork_config: Option<&'a SlopForkConfig>,
    pub(crate) codex_linux_sandbox_exe: Option<PathBuf>,
    pub(crate) windows_sandbox_level: WindowsSandboxLevel,
    pub(crate) windows_sandbox_private_desktop: bool,
}

impl SlopForkRuntimeEventHandler<'_> {
    pub(crate) async fn handle_event(
        &self,
        event: &EventMsg,
        turn_complete_error: Option<&TurnError>,
    ) -> SlopForkRuntimeEventEffects {
        let mut effects = SlopForkRuntimeEventEffects::default();
        match event {
            EventMsg::TurnStarted(turn_started) => {
                match self
                    .pilot_manager
                    .handle_turn_started(self.codex_home, self.thread_id, &turn_started.turn_id)
                    .await
                {
                    Ok(notification) => {
                        effects.pilot_notification = notification;
                    }
                    Err(err) => {
                        warn!("failed to update pilot state for {}: {err}", self.thread_id);
                    }
                }
                match self
                    .autoresearch_manager
                    .handle_turn_started(self.codex_home, self.thread_id, &turn_started.turn_id)
                    .await
                {
                    Ok(notification) => {
                        effects.autoresearch_notification = notification;
                    }
                    Err(err) => {
                        warn!(
                            "failed to update autoresearch state for {}: {err}",
                            self.thread_id
                        );
                    }
                }
            }
            EventMsg::TurnComplete(turn_complete) => match turn_complete_error {
                Some(error) => {
                    let pilot_reason = format!(
                        "Pilot paused because the active turn failed: {}.",
                        error.message
                    );
                    match self
                        .pilot_manager
                        .handle_turn_aborted(
                            self.codex_home,
                            self.thread_id,
                            Some(turn_complete.turn_id.as_str()),
                            &pilot_reason,
                        )
                        .await
                    {
                        Ok(notification) => {
                            effects.pilot_notification = notification;
                        }
                        Err(err) => {
                            warn!("failed to update pilot state for {}: {err}", self.thread_id);
                        }
                    }
                    let autoresearch_reason = format!(
                        "Autoresearch paused because the active turn failed: {}.",
                        error.message
                    );
                    match self
                        .autoresearch_manager
                        .handle_turn_aborted(
                            self.codex_home,
                            self.thread_id,
                            Some(turn_complete.turn_id.as_str()),
                            &autoresearch_reason,
                        )
                        .await
                    {
                        Ok(notification) => {
                            effects.autoresearch_notification = notification;
                        }
                        Err(err) => {
                            warn!(
                                "failed to update autoresearch state for {}: {err}",
                                self.thread_id
                            );
                        }
                    }
                }
                None => {
                    let mut controller_turn_owned = false;
                    match self
                        .pilot_manager
                        .handle_turn_completed(
                            self.codex_home,
                            self.thread_id,
                            &turn_complete.turn_id,
                            turn_complete
                                .last_agent_message
                                .as_deref()
                                .unwrap_or_default(),
                        )
                        .await
                    {
                        Ok(notification) => {
                            controller_turn_owned = notification.is_some();
                            effects.pilot_notification = notification;
                        }
                        Err(err) => {
                            warn!("failed to update pilot state for {}: {err}", self.thread_id);
                        }
                    }
                    match self
                        .autoresearch_manager
                        .handle_turn_completed(
                            self.codex_home,
                            self.thread_id,
                            &turn_complete.turn_id,
                            turn_complete
                                .last_agent_message
                                .as_deref()
                                .unwrap_or_default(),
                        )
                        .await
                    {
                        Ok(notification) => {
                            controller_turn_owned |= notification.is_some();
                            effects.autoresearch_notification = notification;
                        }
                        Err(err) => {
                            warn!(
                                "failed to update autoresearch state for {}: {err}",
                                self.thread_id
                            );
                        }
                    }

                    if !controller_turn_owned {
                        match self
                            .pilot_manager
                            .owns_turn(self.codex_home, self.thread_id, &turn_complete.turn_id)
                            .await
                        {
                            Ok(owned) => {
                                controller_turn_owned = owned;
                            }
                            Err(err) => {
                                controller_turn_owned = true;
                                warn!(
                                    "failed to confirm pilot turn ownership for {}: {err}",
                                    self.thread_id
                                );
                            }
                        }
                    }
                    if !controller_turn_owned {
                        match self
                            .autoresearch_manager
                            .owns_turn(self.codex_home, self.thread_id, &turn_complete.turn_id)
                            .await
                        {
                            Ok(owned) => {
                                controller_turn_owned = owned;
                            }
                            Err(err) => {
                                controller_turn_owned = true;
                                warn!(
                                    "failed to confirm autoresearch turn ownership for {}: {err}",
                                    self.thread_id
                                );
                            }
                        }
                    }

                    if !controller_turn_owned
                        && let Some(fork_config) =
                            self.fork_config.filter(|config| config.automation_enabled)
                    {
                        match self
                            .automation_manager
                            .evaluate_turn_completed(
                                self.codex_home,
                                self.thread,
                                self.thread_id,
                                &turn_complete.turn_id,
                                turn_complete
                                    .last_agent_message
                                    .as_deref()
                                    .unwrap_or_default(),
                                fork_config.automation_shell_timeout_ms,
                                self.codex_linux_sandbox_exe.clone(),
                                self.windows_sandbox_level,
                                self.windows_sandbox_private_desktop,
                            )
                            .await
                        {
                            Ok(notifications) => {
                                effects.automation_notifications = notifications;
                            }
                            Err(err) => {
                                warn!(
                                    "failed to evaluate turn-complete automations for {}: {err}",
                                    self.thread_id
                                );
                            }
                        }
                    }
                }
            },
            EventMsg::TurnAborted(turn_aborted) => {
                let pilot_reason = format!(
                    "Pilot paused because the active turn was aborted: {:?}.",
                    turn_aborted.reason
                );
                match self
                    .pilot_manager
                    .handle_turn_aborted(
                        self.codex_home,
                        self.thread_id,
                        turn_aborted.turn_id.as_deref(),
                        &pilot_reason,
                    )
                    .await
                {
                    Ok(notification) => {
                        effects.pilot_notification = notification;
                    }
                    Err(err) => {
                        warn!("failed to update pilot state for {}: {err}", self.thread_id);
                    }
                }
                let autoresearch_reason = format!(
                    "Autoresearch paused because the active turn was aborted: {:?}.",
                    turn_aborted.reason
                );
                match self
                    .autoresearch_manager
                    .handle_turn_aborted(
                        self.codex_home,
                        self.thread_id,
                        turn_aborted.turn_id.as_deref(),
                        &autoresearch_reason,
                    )
                    .await
                {
                    Ok(notification) => {
                        effects.autoresearch_notification = notification;
                    }
                    Err(err) => {
                        warn!(
                            "failed to update autoresearch state for {}: {err}",
                            self.thread_id
                        );
                    }
                }
            }
            _ => {}
        }
        effects
    }
}
