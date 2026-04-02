use std::path::Path;

use crate::auth::AuthCredentialsStoreMode;
use crate::auth::AuthDotJson;
use crate::auth::CodexAuth;
use crate::auth::RefreshTokenError;
use crate::auth::auth_for_auth_file;
use crate::auth::refresh_chatgpt_auth_from_authority_for_auth;

use super::auth_accounts;

pub fn auth_for_saved_account_file(
    codex_home: &Path,
    account_id: &str,
    auth: AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<CodexAuth> {
    auth_for_auth_file(
        codex_home,
        auth_accounts::saved_account_path(codex_home, account_id),
        auth,
        auth_credentials_store_mode,
    )
}

pub async fn refresh_saved_account_auth_from_authority(
    auth: &CodexAuth,
) -> Result<(), RefreshTokenError> {
    refresh_chatgpt_auth_from_authority_for_auth(auth).await
}

#[cfg(test)]
mod tests {
    use super::auth_accounts;
    use super::auth_for_saved_account_file;
    use crate::auth::AuthCredentialsStoreMode;
    use crate::auth::AuthDotJson;
    use crate::auth::CodexAuth;
    use crate::auth::load_auth_dot_json;
    use crate::auth::login_with_api_key;
    use base64::Engine;
    use codex_login::token_data::IdTokenInfo;
    use codex_login::token_data::KnownPlan as InternalKnownPlan;
    use codex_login::token_data::PlanType as InternalPlanType;
    use codex_login::token_data::TokenData;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    fn fake_jwt(email: &str, account_id: &str) -> String {
        let header = serde_json::json!({ "alg": "HS256", "typ": "JWT" });
        let payload = serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "pro",
                "chatgpt_user_id": "saved-user",
                "chatgpt_account_id": account_id,
            }
        });

        fn b64url_no_pad(bytes: &[u8]) -> String {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        }

        let header_b64 = b64url_no_pad(&serde_json::to_vec(&header).expect("header"));
        let payload_b64 = b64url_no_pad(&serde_json::to_vec(&payload).expect("payload"));
        let signature_b64 = b64url_no_pad(b"sig");
        format!("{header_b64}.{payload_b64}.{signature_b64}")
    }

    #[test]
    fn saved_account_file_auth_persists_tokens_to_the_saved_account_file() {
        let dir = tempdir().unwrap();
        let raw_jwt = fake_jwt("saved@example.com", "workspace-saved");
        let saved_auth = AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some("saved@example.com".to_string()),
                    chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Pro)),
                    chatgpt_user_id: Some("saved-user".to_string()),
                    chatgpt_account_id: Some("workspace-saved".to_string()),
                    raw_jwt: raw_jwt.clone(),
                },
                access_token: "saved-access-token".to_string(),
                refresh_token: "saved-refresh-token".to_string(),
                account_id: None,
            }),
            last_refresh: None,
        };
        let account_id = auth_accounts::upsert_account(dir.path(), &saved_auth)
            .expect("saved account upsert should succeed")
            .expect("saved account id should exist");
        login_with_api_key(dir.path(), "sk-root", AuthCredentialsStoreMode::File)
            .expect("root auth should be replaced");

        let auth = auth_for_saved_account_file(
            dir.path(),
            &account_id,
            saved_auth,
            AuthCredentialsStoreMode::File,
        )
        .expect("saved-account auth should build");
        let CodexAuth::Chatgpt(chatgpt_auth) = auth else {
            panic!("saved account should use managed ChatGPT auth");
        };

        let updated = AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some("saved@example.com".to_string()),
                    chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Pro)),
                    chatgpt_user_id: Some("saved-user".to_string()),
                    chatgpt_account_id: Some("workspace-saved".to_string()),
                    raw_jwt,
                },
                access_token: "saved-access-token-updated".to_string(),
                refresh_token: "saved-refresh-token-updated".to_string(),
                account_id: None,
            }),
            last_refresh: None,
        };
        chatgpt_auth
            .save_auth_for_test(&updated)
            .expect("saved-account token persistence should succeed");

        let root_auth = load_auth_dot_json(dir.path(), AuthCredentialsStoreMode::File)
            .expect("root auth should load")
            .expect("root auth should exist");
        assert_eq!(root_auth.openai_api_key.as_deref(), Some("sk-root"));
        assert!(
            root_auth.tokens.is_none(),
            "root auth should stay untouched"
        );

        let account_path = auth_accounts::saved_account_path(dir.path(), &account_id);
        let saved_account_json = std::fs::read_to_string(account_path).expect("saved account file");
        let saved_account: AuthDotJson =
            serde_json::from_str(&saved_account_json).expect("saved account json should parse");
        assert_eq!(saved_account, updated);
    }
}
