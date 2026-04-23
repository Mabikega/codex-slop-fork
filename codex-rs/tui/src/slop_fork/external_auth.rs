use std::sync::Arc;
use std::time::Duration;

use codex_core::AuthManager;
use codex_core::auth::ExternalAuthSwitchNoticeForFork;
use codex_core::slop_fork::sync_external_auth_if_enabled;
use codex_core::slop_fork::take_external_auth_switch_notice;
use tokio::task::JoinHandle;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

const EXTERNAL_AUTH_SYNC_POLL_INTERVAL: Duration = Duration::from_millis(750);

fn auth_switch_notice_event(notice: ExternalAuthSwitchNoticeForFork) -> AppEvent {
    let (message, is_warning) = match notice {
        ExternalAuthSwitchNoticeForFork::External { label } => (
            format!(
                "Another Codex instance changed the shared active account. This session followed it and is now using {label}."
            ),
            true,
        ),
        ExternalAuthSwitchNoticeForFork::Local { label } => (
            format!("This session changed the shared active account and is now using {label}."),
            false,
        ),
    };
    AppEvent::AuthStateChanged {
        message,
        is_error: false,
        is_warning,
    }
}

pub(crate) fn spawn_external_auth_sync_poller(
    auth_manager: Arc<AuthManager>,
    app_event_tx: AppEventSender,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(EXTERNAL_AUTH_SYNC_POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            interval.tick().await;
            let _ = sync_external_auth_if_enabled(auth_manager.as_ref());
            let Some(notice) = take_external_auth_switch_notice(auth_manager.as_ref()) else {
                continue;
            };
            app_event_tx.send(auth_switch_notice_event(notice));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::auth_switch_notice_event;
    use codex_core::auth::ExternalAuthSwitchNoticeForFork;
    use pretty_assertions::assert_eq;

    use crate::app_event::AppEvent;

    #[test]
    fn external_notice_stays_a_warning() {
        let event = auth_switch_notice_event(ExternalAuthSwitchNoticeForFork::External {
            label: "next@example.com (Pro)".to_string(),
        });

        let AppEvent::AuthStateChanged {
            message,
            is_error,
            is_warning,
        } = event
        else {
            panic!("expected auth state changed event");
        };
        assert_eq!(
            message,
            "Another Codex instance changed the shared active account. This session followed it and is now using next@example.com (Pro)."
        );
        assert_eq!(is_error, false);
        assert_eq!(is_warning, true);
    }

    #[test]
    fn local_notice_is_informational() {
        let event = auth_switch_notice_event(ExternalAuthSwitchNoticeForFork::Local {
            label: "next@example.com (Pro)".to_string(),
        });

        let AppEvent::AuthStateChanged {
            message,
            is_error,
            is_warning,
        } = event
        else {
            panic!("expected auth state changed event");
        };
        assert_eq!(
            message,
            "This session changed the shared active account and is now using next@example.com (Pro)."
        );
        assert_eq!(is_error, false);
        assert_eq!(is_warning, false);
    }
}
