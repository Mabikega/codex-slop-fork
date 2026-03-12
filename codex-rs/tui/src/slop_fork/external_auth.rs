use std::sync::Arc;
use std::time::Duration;

use codex_core::AuthManager;
use codex_core::slop_fork::sync_external_auth_if_enabled;
use codex_core::slop_fork::take_external_auth_switch_notice;
use tokio::task::JoinHandle;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

const EXTERNAL_AUTH_SYNC_POLL_INTERVAL: Duration = Duration::from_millis(750);

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
            let Some(label) = take_external_auth_switch_notice(auth_manager.as_ref()) else {
                continue;
            };
            app_event_tx.send(AppEvent::AuthStateChanged {
                message: format!(
                    "Another Codex instance changed the shared active account. This session followed it and is now using {label}."
                ),
                is_error: false,
                is_warning: true,
            });
        }
    })
}
