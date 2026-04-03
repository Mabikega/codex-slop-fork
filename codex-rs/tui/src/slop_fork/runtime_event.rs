use codex_app_server_protocol::Automation;
use codex_app_server_protocol::AutomationUpdateType;
use codex_app_server_protocol::AutoresearchRun;
use codex_app_server_protocol::AutoresearchUpdateType;
use codex_app_server_protocol::PilotRun;
use codex_app_server_protocol::PilotUpdateType;
#[cfg(test)]
use codex_protocol::protocol::TurnAbortReason;

use super::ui::SlopForkRuntimeEvent;
use super::ui::SlopForkTurnAbortCause;

pub(crate) fn automation_updated<'a>(
    update_type: AutomationUpdateType,
    runtime_id: &'a str,
    automation: Option<Automation>,
    message: Option<String>,
    from_replay: bool,
) -> SlopForkRuntimeEvent<'a> {
    SlopForkRuntimeEvent::AutomationUpdated {
        update_type,
        runtime_id,
        automation: automation.map(Box::new),
        message,
        from_replay,
    }
}

pub(crate) fn pilot_updated(
    update_type: PilotUpdateType,
    run: Option<PilotRun>,
    message: Option<String>,
    from_replay: bool,
) -> SlopForkRuntimeEvent<'static> {
    SlopForkRuntimeEvent::PilotUpdated {
        update_type,
        run: run.map(Box::new),
        message,
        from_replay,
    }
}

pub(crate) fn autoresearch_updated(
    update_type: AutoresearchUpdateType,
    run: Option<AutoresearchRun>,
    message: Option<String>,
    from_replay: bool,
) -> SlopForkRuntimeEvent<'static> {
    SlopForkRuntimeEvent::AutoresearchUpdated {
        update_type,
        run: run.map(Box::new),
        message,
        from_replay,
    }
}

pub(crate) fn controller_turn_started(
    turn_id: &str,
    from_replay: bool,
) -> SlopForkRuntimeEvent<'_> {
    SlopForkRuntimeEvent::ControllerTurnStarted {
        turn_id,
        from_replay,
    }
}

pub(crate) fn interrupted_controller_turn(
    turn_id: Option<&str>,
    from_replay: bool,
) -> SlopForkRuntimeEvent<'_> {
    controller_turn_aborted(turn_id, SlopForkTurnAbortCause::Interrupted, from_replay)
}

pub(crate) fn failed_controller_turn(
    turn_id: Option<&str>,
    from_replay: bool,
) -> SlopForkRuntimeEvent<'_> {
    controller_turn_aborted(turn_id, SlopForkTurnAbortCause::Failed, from_replay)
}

#[cfg(test)]
pub(crate) fn from_turn_abort_reason(
    turn_id: Option<&str>,
    reason: TurnAbortReason,
    from_replay: bool,
) -> SlopForkRuntimeEvent<'_> {
    let cause = match reason {
        TurnAbortReason::Interrupted => SlopForkTurnAbortCause::Interrupted,
        TurnAbortReason::Replaced => SlopForkTurnAbortCause::Replaced,
        TurnAbortReason::ReviewEnded => SlopForkTurnAbortCause::ReviewEnded,
    };
    controller_turn_aborted(turn_id, cause, from_replay)
}

pub(crate) fn fallback_pilot_status_message(
    update_type: PilotUpdateType,
    run: Option<&PilotRun>,
) -> Option<String> {
    run.and_then(|run| run.status_message.clone()).or_else(|| {
        let message = match update_type {
            PilotUpdateType::Started => "Pilot started.",
            PilotUpdateType::Queued => "Pilot queued the next autonomous cycle.",
            PilotUpdateType::CycleStarted => "Pilot started a cycle.",
            PilotUpdateType::CycleCompleted => "Pilot completed a cycle.",
            PilotUpdateType::Paused => "Pilot paused.",
            PilotUpdateType::Resumed => "Pilot resumed.",
            PilotUpdateType::WrapUpRequested => "Pilot wrap-up requested.",
            PilotUpdateType::Stopped => "Pilot stopped.",
            PilotUpdateType::Completed => "Pilot completed its wrap-up cycle.",
            PilotUpdateType::Failed => "Pilot failed.",
            PilotUpdateType::Updated => return None,
        };
        Some(message.to_string())
    })
}

pub(crate) fn fallback_autoresearch_status_message(
    update_type: AutoresearchUpdateType,
    run: Option<&AutoresearchRun>,
) -> Option<String> {
    run.and_then(|run| run.status_message.clone()).or_else(|| {
        let message = match update_type {
            AutoresearchUpdateType::Started => "Autoresearch started.",
            AutoresearchUpdateType::Queued => "Autoresearch queued the next autonomous cycle.",
            AutoresearchUpdateType::CycleStarted => "Autoresearch started a cycle.",
            AutoresearchUpdateType::CycleCompleted => "Autoresearch completed a cycle.",
            AutoresearchUpdateType::Paused => "Autoresearch paused.",
            AutoresearchUpdateType::Resumed => "Autoresearch resumed.",
            AutoresearchUpdateType::WrapUpRequested => "Autoresearch wrap-up requested.",
            AutoresearchUpdateType::Stopped => "Autoresearch stopped.",
            AutoresearchUpdateType::Completed => "Autoresearch completed.",
            AutoresearchUpdateType::Failed => "Autoresearch failed.",
            AutoresearchUpdateType::Cleared => "Autoresearch cleared.",
            AutoresearchUpdateType::DiscoveryQueued => {
                "Autoresearch queued a bounded discovery pass."
            }
            AutoresearchUpdateType::Updated => return None,
        };
        Some(message.to_string())
    })
}

fn controller_turn_aborted(
    turn_id: Option<&str>,
    cause: SlopForkTurnAbortCause,
    from_replay: bool,
) -> SlopForkRuntimeEvent<'_> {
    SlopForkRuntimeEvent::ControllerTurnAborted {
        turn_id,
        cause,
        from_replay,
    }
}
