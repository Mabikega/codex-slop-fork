use std::sync::Arc;

use codex_agent_identity::AgentIdentityKey;
use codex_agent_identity::AgentTaskAuthorizationTarget;
use codex_agent_identity::authorization_header_for_agent_task;
use codex_agent_identity::normalize_chatgpt_base_url;
use codex_agent_identity::register_agent_task;
use codex_protocol::account::PlanType as AccountPlanType;
use tokio::sync::OnceCell;

use crate::default_client::build_reqwest_client;

use super::storage::AgentIdentityAuthRecord;

const DEFAULT_CHATGPT_BACKEND_BASE_URL: &str = "https://chatgpt.com/backend-api";

#[derive(Debug)]
pub struct AgentIdentityAuth {
    record: AgentIdentityAuthRecord,
    process_task_id: Arc<OnceCell<String>>,
}

impl Clone for AgentIdentityAuth {
    fn clone(&self) -> Self {
        Self {
            record: self.record.clone(),
            process_task_id: Arc::clone(&self.process_task_id),
        }
    }
}

impl AgentIdentityAuth {
    pub fn new(record: AgentIdentityAuthRecord) -> Self {
        Self {
            record,
            process_task_id: Arc::new(OnceCell::new()),
        }
    }

    pub fn record(&self) -> &AgentIdentityAuthRecord {
        &self.record
    }

    pub async fn ensure_runtime(&self, chatgpt_base_url: Option<String>) -> std::io::Result<()> {
        self.process_task_id
            .get_or_try_init(|| async {
                let base_url = normalize_chatgpt_base_url(
                    chatgpt_base_url
                        .as_deref()
                        .unwrap_or(DEFAULT_CHATGPT_BACKEND_BASE_URL),
                );
                register_agent_task(&build_reqwest_client(), &base_url, self.key())
                    .await
                    .map_err(std::io::Error::other)
            })
            .await
            .map(|_| ())
    }

    pub fn authorization_header_value(&self) -> std::io::Result<String> {
        let task_id = self
            .process_task_id
            .get()
            .ok_or_else(|| std::io::Error::other("agent identity runtime is not initialized"))?;
        authorization_header_for_agent_task(
            self.key(),
            AgentTaskAuthorizationTarget {
                agent_runtime_id: &self.record.agent_runtime_id,
                task_id,
            },
        )
        .map_err(std::io::Error::other)
    }

    pub fn account_id(&self) -> &str {
        &self.record.account_id
    }

    pub fn chatgpt_user_id(&self) -> &str {
        &self.record.chatgpt_user_id
    }

    pub fn email(&self) -> &str {
        &self.record.email
    }

    pub fn plan_type(&self) -> AccountPlanType {
        self.record.plan_type
    }

    pub fn is_fedramp_account(&self) -> bool {
        self.record.chatgpt_account_is_fedramp
    }

    fn key(&self) -> AgentIdentityKey<'_> {
        AgentIdentityKey {
            agent_runtime_id: &self.record.agent_runtime_id,
            private_key_pkcs8_base64: &self.record.agent_private_key,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn sample_auth() -> AgentIdentityAuth {
        AgentIdentityAuth::new(AgentIdentityAuthRecord {
            agent_runtime_id: "runtime-123".to_string(),
            agent_private_key: "MC4CAQAwBQYDK2VwBCIEIF0YfwNgTOuld+mqaN7OfdKVvNKnUgb2N0ONXqXY92a2"
                .to_string(),
            account_id: "acct-123".to_string(),
            chatgpt_user_id: "user-123".to_string(),
            email: "agent@example.com".to_string(),
            plan_type: AccountPlanType::Pro,
            chatgpt_account_is_fedramp: false,
        })
    }

    #[test]
    fn authorization_header_value_uses_initialized_runtime_task() {
        let auth = sample_auth();
        auth.process_task_id
            .set("task-123".to_string())
            .expect("seed task id");

        let header = auth
            .authorization_header_value()
            .expect("authorization header");

        assert!(header.starts_with("AgentAssertion "));
    }

    #[test]
    fn authorization_header_value_requires_initialized_runtime() {
        let err = sample_auth()
            .authorization_header_value()
            .expect_err("runtime init should be required");

        assert_eq!(err.to_string(), "agent identity runtime is not initialized");
    }
}
