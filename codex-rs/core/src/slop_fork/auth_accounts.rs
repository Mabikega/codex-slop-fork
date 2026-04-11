use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AuthMode;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use crate::auth::AuthCredentialsStoreMode;
use crate::auth::AuthDotJson;
use crate::auth::CodexAuth;
use crate::auth::load_auth_dot_json;
use crate::path_utils::write_atomically;
use codex_login::token_data::parse_chatgpt_jwt_claims;
use codex_login::token_data::parse_chatgpt_subscription_active_until;

use super::account_rate_limits;
use super::config::SlopForkConfig;
use super::config::maybe_load_slop_fork_config;
use super::save_auth_with_account_sync;

const ACCOUNTS_DIR: &str = ".accounts";
const ACCOUNT_SWITCH_LOCK_FILE: &str = ".auth-switch.lock";

#[derive(Debug, Clone, PartialEq)]
pub struct StoredAccount {
    pub id: String,
    pub path: PathBuf,
    pub auth: AuthDotJson,
    pub modified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountRenameSuggestion {
    pub path: PathBuf,
    pub suggested_id: String,
    pub suggested_path: PathBuf,
    pub auth: AuthDotJson,
    pub modified_at: Option<DateTime<Utc>>,
    pub target_exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameAllAccountsResult {
    pub renamed_count: usize,
    pub skipped_existing_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountDisplayLabels {
    chatgpt_labels_by_lookup_key: HashMap<String, String>,
}

pub fn accounts_dir(codex_home: &Path) -> PathBuf {
    codex_home.join(ACCOUNTS_DIR)
}

pub fn account_switch_lock_path(codex_home: &Path) -> PathBuf {
    accounts_dir(codex_home).join(ACCOUNT_SWITCH_LOCK_FILE)
}

pub fn account_label(account: &StoredAccount) -> String {
    auth_label(&account.auth)
}

pub fn load_account_display_labels(codex_home: &Path) -> AccountDisplayLabels {
    let config = maybe_load_slop_fork_config(codex_home).unwrap_or_default();
    let accounts = list_accounts(codex_home).unwrap_or_default();
    AccountDisplayLabels::from_config(&config, &accounts)
}

pub fn auth_label(auth: &AuthDotJson) -> String {
    match auth.resolved_mode() {
        AuthMode::ApiKey => {
            let suffix = auth
                .openai_api_key
                .as_deref()
                .map(api_key_suffix)
                .unwrap_or_else(|| "unknown".to_string());
            format!("API key ({suffix})")
        }
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => {
            let base_label = auth
                .tokens
                .as_ref()
                .and_then(|tokens| {
                    tokens
                        .id_token
                        .email
                        .clone()
                        .or_else(|| tokens.account_id.clone())
                })
                .unwrap_or_else(|| "ChatGPT account".to_string());
            chatgpt_label_with_base(auth, base_label)
        }
    }
}

pub fn codex_auth_label(auth: &CodexAuth) -> String {
    if let Some(auth_dot_json) = auth.auth_dot_json() {
        return auth_label(&auth_dot_json);
    }

    if let Some(api_key) = auth.api_key() {
        return format!("API key ({})", api_key_suffix(api_key));
    }

    auth.get_account_email()
        .or_else(|| auth.get_account_id())
        .unwrap_or_else(|| "ChatGPT account".to_string())
}

impl AccountDisplayLabels {
    pub fn from_config(config: &SlopForkConfig, accounts: &[StoredAccount]) -> Self {
        if !config.show_account_numbers_instead_of_emails {
            return Self::default();
        }

        let mut ordered_accounts = accounts
            .iter()
            .filter(|account| account.auth.is_chatgpt_mode())
            .map(chatgpt_account_label_keys)
            .collect::<Vec<_>>();
        ordered_accounts.sort_by(|lhs, rhs| lhs.sort_key.cmp(&rhs.sort_key));

        let mut chatgpt_labels_by_lookup_key = HashMap::new();
        let mut seen_lookup_keys = HashSet::new();
        let mut next_account_number = 1;
        for keys in ordered_accounts {
            if !seen_lookup_keys.insert(keys.lookup_key.clone()) {
                continue;
            }
            chatgpt_labels_by_lookup_key
                .insert(keys.lookup_key, format!("Account {next_account_number}"));
            next_account_number += 1;
        }

        Self {
            chatgpt_labels_by_lookup_key,
        }
    }

    pub fn label_for_account(&self, account: &StoredAccount) -> String {
        self.label_for_auth(&account.auth)
    }

    pub fn label_for_auth(&self, auth: &AuthDotJson) -> String {
        self.label_for_auth_with_lookup_key(auth, chatgpt_account_lookup_key(auth))
    }

    pub fn label_for_codex_auth(&self, auth: &CodexAuth) -> String {
        if let Some(auth_dot_json) = auth.auth_dot_json() {
            return self.label_for_auth(&auth_dot_json);
        }

        if let Some(lookup_key) = auth
            .get_account_id()
            .or_else(|| auth.get_chatgpt_user_id())
            .or_else(|| stored_account_id_for_auth(auth))
            && let Some(label) = self.chatgpt_labels_by_lookup_key.get(&lookup_key)
        {
            return label.clone();
        }

        codex_auth_label(auth)
    }

    fn label_for_auth_with_lookup_key(
        &self,
        auth: &AuthDotJson,
        lookup_key: Option<String>,
    ) -> String {
        if let Some(lookup_key) = lookup_key
            && let Some(numbered_label) = self.chatgpt_labels_by_lookup_key.get(&lookup_key)
        {
            return chatgpt_label_with_base(auth, numbered_label.clone());
        }

        auth_label(auth)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChatgptAccountLabelKeys {
    lookup_key: String,
    sort_key: (String, String, String),
}

fn chatgpt_account_label_keys(account: &StoredAccount) -> ChatgptAccountLabelKeys {
    let fallback = account.id.clone();
    let lookup_key = chatgpt_account_lookup_key(&account.auth).unwrap_or_else(|| fallback.clone());
    let user_id = account
        .auth
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.id_token.chatgpt_user_id.clone())
        .unwrap_or_else(|| lookup_key.clone());
    ChatgptAccountLabelKeys {
        lookup_key: lookup_key.clone(),
        sort_key: (user_id, lookup_key, fallback),
    }
}

fn chatgpt_account_lookup_key(auth: &AuthDotJson) -> Option<String> {
    let tokens = auth.tokens.as_ref()?;
    tokens
        .id_token
        .chatgpt_user_id
        .clone()
        .or_else(|| tokens.account_id.clone())
        .or_else(|| tokens.id_token.chatgpt_account_id.clone())
}

pub fn list_accounts(codex_home: &Path) -> std::io::Result<Vec<StoredAccount>> {
    let mut accounts_by_id = std::collections::HashMap::new();
    for file in scan_account_files(codex_home)? {
        let Some(expected_id) = stored_account_id(&file.auth) else {
            continue;
        };
        let account = StoredAccount {
            id: expected_id.clone(),
            path: file.path,
            auth: file.auth,
            modified_at: file.modified_at,
        };
        match accounts_by_id.get_mut(&expected_id) {
            Some(existing) => {
                if should_prefer_account_file(codex_home, existing, &account) {
                    *existing = account;
                }
            }
            None => {
                accounts_by_id.insert(expected_id, account);
            }
        }
    }
    let mut accounts = accounts_by_id.into_values().collect::<Vec<_>>();

    accounts.sort_by(|lhs, rhs| {
        rhs.modified_at
            .cmp(&lhs.modified_at)
            .then_with(|| lhs.id.cmp(&rhs.id))
    });
    Ok(accounts)
}

pub fn list_account_rename_suggestions(
    codex_home: &Path,
) -> std::io::Result<Vec<AccountRenameSuggestion>> {
    let mut suggestions = scan_account_files(codex_home)?
        .into_iter()
        .filter_map(|file| {
            let suggested_id = stored_account_id(&file.auth)?;
            (file.file_id != suggested_id).then_some(AccountRenameSuggestion {
                target_exists: account_path(codex_home, &suggested_id).exists(),
                suggested_path: account_path(codex_home, &suggested_id),
                suggested_id,
                path: file.path,
                auth: file.auth,
                modified_at: file.modified_at,
            })
        })
        .collect::<Vec<_>>();

    suggestions.sort_by(|lhs, rhs| lhs.path.cmp(&rhs.path));
    Ok(suggestions)
}

pub fn find_account(codex_home: &Path, account_id: &str) -> std::io::Result<Option<StoredAccount>> {
    Ok(list_accounts(codex_home)?
        .into_iter()
        .find(|account| account.id == account_id))
}

pub fn rate_limit_snapshot_lookup_ids(account: &StoredAccount) -> Vec<String> {
    let mut ids = vec![account.id.clone()];
    if let Some(legacy_id) = account.auth.tokens.as_ref().and_then(|tokens| {
        tokens
            .account_id
            .clone()
            .or_else(|| tokens.id_token.chatgpt_account_id.clone())
    }) && legacy_id != account.id
    {
        ids.push(legacy_id);
    }
    if let Some(legacy_saved_id) = legacy_chatgpt_saved_account_id(&account.auth)
        && !ids.contains(&legacy_saved_id)
    {
        ids.push(legacy_saved_id);
    }
    ids
}

pub fn remove_account(codex_home: &Path, account_id: &str) -> std::io::Result<bool> {
    let Some(account) = find_account(codex_home, account_id)? else {
        return Ok(false);
    };
    match std::fs::remove_file(account.path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

pub fn rename_account_file(codex_home: &Path, current_path: &Path) -> std::io::Result<bool> {
    let Some(suggestion) = list_account_rename_suggestions(codex_home)?
        .into_iter()
        .find(|suggestion| suggestion.path == current_path)
    else {
        return Ok(false);
    };
    if suggestion.target_exists {
        return Ok(false);
    }
    std::fs::rename(&suggestion.path, &suggestion.suggested_path)?;
    Ok(true)
}

pub fn rename_all_account_files(codex_home: &Path) -> std::io::Result<RenameAllAccountsResult> {
    let mut result = RenameAllAccountsResult {
        renamed_count: 0,
        skipped_existing_count: 0,
    };

    for suggestion in list_account_rename_suggestions(codex_home)? {
        if suggestion.target_exists {
            result.skipped_existing_count += 1;
            continue;
        }
        std::fs::rename(&suggestion.path, &suggestion.suggested_path)?;
        result.renamed_count += 1;
    }

    Ok(result)
}

pub fn activate_account(
    codex_home: &Path,
    account_id: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<Option<StoredAccount>> {
    let Some(account) = find_account(codex_home, account_id)? else {
        return Ok(None);
    };
    save_auth_with_account_sync(codex_home, &account.auth, auth_credentials_store_mode)?;
    Ok(Some(account))
}

pub fn current_active_account_id(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<Option<String>> {
    let Some(auth) = load_current_active_auth(codex_home, auth_credentials_store_mode)? else {
        return Ok(None);
    };
    Ok(stored_account_id(&auth))
}

pub fn saved_account_subscription_ran_out(
    account: &StoredAccount,
    snapshot: Option<&account_rate_limits::StoredRateLimitSnapshot>,
) -> bool {
    if !account.auth.is_chatgpt_mode() {
        return false;
    }

    if snapshot.is_some_and(|snapshot| snapshot.workspace_deactivated) {
        return true;
    }
    if auth_chatgpt_subscription_active_until_indicates_expired(&account.auth, snapshot, Utc::now())
    {
        return true;
    }

    let Some(snapshot_plan) = snapshot
        .and_then(|snapshot| snapshot.plan.as_deref())
        .map(normalized_chatgpt_plan)
    else {
        return false;
    };
    if snapshot_plan != "free" {
        return false;
    }

    auth_chatgpt_plan_type_raw(&account.auth).is_some_and(|saved_plan| saved_plan != "free")
}

pub fn ensure_current_active_account_saved(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<Option<String>> {
    let Some(auth) = load_current_active_auth(codex_home, auth_credentials_store_mode)? else {
        return Ok(None);
    };
    let Some(account_id) = stored_account_id(&auth) else {
        return Ok(None);
    };
    let path = account_path(codex_home, &account_id);
    if path.exists() {
        return Ok(Some(account_id));
    }
    let dir = accounts_dir(codex_home);
    std::fs::create_dir_all(&dir)?;
    write_auth_file(&path, &auth)?;
    Ok(Some(account_id))
}

pub fn upsert_account(codex_home: &Path, auth: &AuthDotJson) -> std::io::Result<Option<String>> {
    let Some(account_id) = stored_account_id(auth) else {
        return Ok(None);
    };

    let dir = accounts_dir(codex_home);
    std::fs::create_dir_all(&dir)?;
    let path = account_path(codex_home, &account_id);
    let merged = match std::fs::read_to_string(&path) {
        Ok(existing) => {
            let existing: AuthDotJson = serde_json::from_str(&existing)?;
            merge_existing_auth(existing, auth.clone())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => auth.clone(),
        Err(err) => return Err(err),
    };
    write_auth_file(&path, &merged)?;
    Ok(Some(account_id))
}

pub fn stored_account_id(auth: &AuthDotJson) -> Option<String> {
    let identity = account_identity(auth)?;
    let prefix = match auth.resolved_mode() {
        AuthMode::ApiKey => "api-key",
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => "chatgpt",
    };
    let digest = Sha256::digest(identity.as_bytes());
    let hex = format!("{digest:x}");
    Some(format!("{prefix}-{}", &hex[..16]))
}

pub fn stored_account_id_for_auth(auth: &CodexAuth) -> Option<String> {
    if let Some(auth_dot_json) = auth.auth_dot_json() {
        return stored_account_id(&auth_dot_json);
    }

    auth.api_key().and_then(|api_key| {
        stored_account_id(&AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some(api_key.to_string()),
            tokens: None,
            last_refresh: None,
        })
    })
}

fn load_current_active_auth(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<Option<AuthDotJson>> {
    if let Some(auth) = load_auth_dot_json(codex_home, AuthCredentialsStoreMode::Ephemeral)? {
        return Ok(Some(auth));
    }
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return Ok(None);
    }
    load_auth_dot_json(codex_home, auth_credentials_store_mode)
}

fn auth_chatgpt_plan_type_raw(auth: &AuthDotJson) -> Option<String> {
    auth.tokens
        .as_ref()
        .and_then(|tokens| {
            tokens.id_token.get_chatgpt_plan_type_raw().or_else(|| {
                parse_chatgpt_jwt_claims(&tokens.id_token.raw_jwt)
                    .ok()
                    .and_then(|claims| claims.get_chatgpt_plan_type_raw())
            })
        })
        .map(normalized_chatgpt_plan)
}

fn auth_chatgpt_subscription_active_until(auth: &AuthDotJson) -> Option<DateTime<Utc>> {
    auth.tokens.as_ref().and_then(|tokens| {
        parse_chatgpt_subscription_active_until(&tokens.id_token.raw_jwt)
            .ok()
            .flatten()
    })
}

fn auth_chatgpt_subscription_active_until_indicates_expired(
    auth: &AuthDotJson,
    snapshot: Option<&account_rate_limits::StoredRateLimitSnapshot>,
    now: DateTime<Utc>,
) -> bool {
    let Some(active_until) = auth_chatgpt_subscription_active_until(auth) else {
        return false;
    };
    if now <= active_until {
        return false;
    }

    !snapshot.is_some_and(|snapshot| {
        snapshot.snapshot.is_some()
            && snapshot
                .observed_at
                .is_some_and(|observed_at| observed_at > active_until)
    })
}

fn normalized_chatgpt_plan(plan: impl AsRef<str>) -> String {
    plan.as_ref().trim().to_ascii_lowercase()
}

#[derive(Debug)]
struct ParsedAccountFile {
    file_id: String,
    path: PathBuf,
    auth: AuthDotJson,
    modified_at: Option<DateTime<Utc>>,
}

fn scan_account_files(codex_home: &Path) -> std::io::Result<Vec<ParsedAccountFile>> {
    let dir = accounts_dir(codex_home);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json")
            || account_rate_limits::is_rate_limits_sidecar_file(&path)
        {
            continue;
        }
        let Some(file_id) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) => {
                tracing::warn!("ignoring unreadable account file {}: {err}", path.display());
                continue;
            }
        };
        let auth = match serde_json::from_str::<AuthDotJson>(&contents) {
            Ok(auth) => auth,
            Err(err) => {
                tracing::warn!("ignoring invalid account file {}: {err}", path.display());
                continue;
            }
        };
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(err) => {
                tracing::warn!(
                    "ignoring account file with unreadable metadata {}: {err}",
                    path.display()
                );
                continue;
            }
        };
        files.push(ParsedAccountFile {
            file_id,
            path,
            auth,
            modified_at: metadata.modified().ok().map(DateTime::<Utc>::from),
        });
    }

    Ok(files)
}

fn should_prefer_account_file(
    codex_home: &Path,
    existing: &StoredAccount,
    candidate: &StoredAccount,
) -> bool {
    let canonical_path = account_path(codex_home, &existing.id);
    match (
        existing.path == canonical_path,
        candidate.path == canonical_path,
    ) {
        (false, true) => true,
        (true, false) => false,
        _ => candidate
            .modified_at
            .cmp(&existing.modified_at)
            .then_with(|| existing.path.cmp(&candidate.path))
            .is_gt(),
    }
}

fn chatgpt_label_with_base(auth: &AuthDotJson, base_label: String) -> String {
    match auth
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.id_token.get_chatgpt_plan_type())
    {
        Some(plan) => format!("{base_label} ({plan})"),
        None => base_label,
    }
}

fn account_identity(auth: &AuthDotJson) -> Option<String> {
    match auth.resolved_mode() {
        AuthMode::ApiKey => auth
            .openai_api_key
            .as_ref()
            .map(|api_key| format!("api-key:{api_key}")),
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => chatgpt_account_lookup_key(auth)
            .or_else(|| {
                auth.tokens
                    .as_ref()
                    .and_then(|tokens| tokens.id_token.email.as_deref())
                    .map(str::trim)
                    .filter(|email| !email.is_empty())
                    .map(str::to_ascii_lowercase)
            })
            .map(|identity| format!("chatgpt:{identity}")),
    }
}

fn legacy_chatgpt_saved_account_id(auth: &AuthDotJson) -> Option<String> {
    if !auth.is_chatgpt_mode() {
        return None;
    }
    let tokens = auth.tokens.as_ref()?;
    let account_id = tokens
        .account_id
        .as_deref()
        .or(tokens.id_token.chatgpt_account_id.as_deref())
        .unwrap_or("");
    let email = tokens
        .id_token
        .email
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let digest = Sha256::digest(format!("chatgpt:{account_id}:{email}").as_bytes());
    let hex = format!("{digest:x}");
    Some(format!("chatgpt-{}", &hex[..16]))
}

fn merge_existing_auth(existing: AuthDotJson, incoming: AuthDotJson) -> AuthDotJson {
    if !existing.is_chatgpt_mode() || !incoming.is_chatgpt_mode() {
        return incoming;
    }

    let Some(existing_tokens) = existing.tokens.as_ref() else {
        return incoming;
    };
    let Some(incoming_tokens) = incoming.tokens.as_ref() else {
        return incoming;
    };

    if existing_tokens.refresh_token.is_empty() || !incoming_tokens.refresh_token.is_empty() {
        return incoming;
    }

    let mut merged = incoming.clone();
    if let Some(tokens) = merged.tokens.as_mut() {
        tokens.refresh_token = existing_tokens.refresh_token.clone();
    }
    merged.auth_mode = Some(AuthMode::Chatgpt);
    merged
}

fn write_auth_file(path: &Path, auth: &AuthDotJson) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(auth)?;
    write_atomically(path, &json)
}

pub(crate) fn saved_account_path(codex_home: &Path, account_id: &str) -> PathBuf {
    account_path(codex_home, account_id)
}

fn account_path(codex_home: &Path, account_id: &str) -> PathBuf {
    accounts_dir(codex_home).join(format!("{account_id}.json"))
}

fn api_key_suffix(api_key: &str) -> String {
    let suffix_len = api_key.chars().count().min(4);
    let suffix: String = api_key
        .chars()
        .rev()
        .take(suffix_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("...{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use codex_login::token_data::IdTokenInfo;
    use codex_login::token_data::KnownPlan;
    use codex_login::token_data::PlanType;
    use codex_login::token_data::TokenData;
    use pretty_assertions::assert_eq;
    use serde::Serialize;
    use tempfile::tempdir;

    fn fake_jwt(email: &str, plan: &str, account_id: &str) -> String {
        #[derive(Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }

        let header = Header {
            alg: "none",
            typ: "JWT",
        };
        let payload = serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": plan,
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

    fn fake_jwt_with_active_until(
        email: &str,
        plan: &str,
        account_id: &str,
        active_until: &str,
    ) -> String {
        #[derive(Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }

        let header = Header {
            alg: "none",
            typ: "JWT",
        };
        let payload = serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": plan,
                "chatgpt_account_id": account_id,
                "chatgpt_subscription_active_until": active_until,
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

    fn chatgpt_auth(account_id: &str, email: &str) -> AuthDotJson {
        AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some(email.to_string()),
                    chatgpt_plan_type: Some(PlanType::Known(KnownPlan::Pro)),
                    chatgpt_user_id: None,
                    chatgpt_account_id: Some(account_id.to_string()),
                    raw_jwt: fake_jwt(email, "pro", account_id),
                },
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: Some(account_id.to_string()),
            }),
            last_refresh: Some(Utc::now()),
        }
    }

    #[test]
    fn upsert_and_list_accounts_round_trip() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let auth = chatgpt_auth("acct-1", "person@example.com");
        let account_id = upsert_account(dir.path(), &auth)?.expect("saved account id");
        let accounts = list_accounts(dir.path())?;
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, account_id);
        assert_eq!(accounts[0].auth, auth);
        Ok(())
    }

    #[test]
    fn account_switch_lock_path_lives_in_accounts_dir() -> anyhow::Result<()> {
        let dir = tempdir()?;
        assert_eq!(
            account_switch_lock_path(dir.path()),
            dir.path().join(".accounts").join(".auth-switch.lock")
        );
        Ok(())
    }

    #[test]
    fn external_auth_does_not_drop_refresh_token_when_existing_account_is_managed()
    -> anyhow::Result<()> {
        let dir = tempdir()?;
        let existing = chatgpt_auth("acct-1", "person@example.com");
        upsert_account(dir.path(), &existing)?;

        let mut external = existing;
        external.auth_mode = Some(AuthMode::ChatgptAuthTokens);
        external
            .tokens
            .as_mut()
            .expect("tokens")
            .refresh_token
            .clear();
        external.tokens.as_mut().expect("tokens").access_token = "new-access".to_string();

        let account_id = upsert_account(dir.path(), &external)?.expect("saved account id");
        let saved = find_account(dir.path(), &account_id)?.expect("saved account");
        assert_eq!(
            saved.auth.tokens.expect("saved tokens").refresh_token,
            "refresh".to_string()
        );
        Ok(())
    }

    #[test]
    fn current_active_auth_is_saved_before_overwrite() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let auth = chatgpt_auth("acct-1", "person@example.com");
        write_auth_file(&dir.path().join("auth.json"), &auth)?;

        let account_id =
            ensure_current_active_account_saved(dir.path(), AuthCredentialsStoreMode::File)?
                .expect("saved account id");
        let saved = find_account(dir.path(), &account_id)?.expect("saved account");

        assert_eq!(saved.auth, auth);
        Ok(())
    }

    #[test]
    fn saved_account_subscription_ran_out_uses_fresh_snapshot_plan() {
        let auth = chatgpt_auth("acct-1", "person@example.com");
        let account = StoredAccount {
            id: stored_account_id(&auth).expect("saved account id"),
            path: PathBuf::from("acct-1.json"),
            auth,
            modified_at: None,
        };
        let snapshot = account_rate_limits::StoredRateLimitSnapshot {
            account_id: account.id.clone(),
            plan: Some("free".to_string()),
            workspace_deactivated: false,
            snapshot: None,
            five_hour_window: account_rate_limits::StoredQuotaWindow::default(),
            weekly_window: account_rate_limits::StoredQuotaWindow::default(),
            observed_at: None,
            primary_next_reset_at: None,
            secondary_next_reset_at: None,
            last_refresh_attempt_at: None,
            last_usage_limit_hit_at: None,
        };

        assert_eq!(
            saved_account_subscription_ran_out(&account, Some(&snapshot)),
            true
        );
    }

    #[test]
    fn saved_account_subscription_ran_out_uses_expired_subscription_until_without_snapshot() {
        let auth = AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some("person@example.com".to_string()),
                    chatgpt_plan_type: Some(PlanType::Known(KnownPlan::Team)),
                    chatgpt_user_id: None,
                    chatgpt_account_id: Some("acct-1".to_string()),
                    raw_jwt: fake_jwt_with_active_until(
                        "person@example.com",
                        "team",
                        "acct-1",
                        "2026-04-05T23:08:23+00:00",
                    ),
                },
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: Some("acct-1".to_string()),
            }),
            last_refresh: None,
        };
        let account = StoredAccount {
            id: stored_account_id(&auth).expect("saved account id"),
            path: PathBuf::from("acct-1.json"),
            auth,
            modified_at: None,
        };

        assert!(saved_account_subscription_ran_out(
            &account, /*snapshot*/ None
        ));
    }

    #[test]
    fn numbered_display_labels_sort_chatgpt_accounts_by_uid() -> anyhow::Result<()> {
        let config = SlopForkConfig {
            show_account_numbers_instead_of_emails: true,
            ..SlopForkConfig::default()
        };
        let auth_b = chatgpt_auth("acct-b", "beta@example.com");
        let auth_a = chatgpt_auth("acct-a", "alpha@example.com");
        let account_b = StoredAccount {
            id: stored_account_id(&auth_b).expect("saved account id"),
            path: PathBuf::from("acct-b.json"),
            auth: auth_b.clone(),
            modified_at: None,
        };
        let account_a = StoredAccount {
            id: stored_account_id(&auth_a).expect("saved account id"),
            path: PathBuf::from("acct-a.json"),
            auth: auth_a.clone(),
            modified_at: None,
        };
        let labels =
            AccountDisplayLabels::from_config(&config, &[account_b.clone(), account_a.clone()]);

        assert_eq!(
            labels.label_for_account(&account_a),
            "Account 1 (Pro)".to_string()
        );
        assert_eq!(
            labels.label_for_account(&account_b),
            "Account 2 (Pro)".to_string()
        );
        assert_eq!(
            labels.label_for_auth(&auth_a),
            "Account 1 (Pro)".to_string()
        );
        assert_eq!(
            labels.label_for_auth(&auth_b),
            "Account 2 (Pro)".to_string()
        );
        Ok(())
    }

    #[test]
    fn numbered_display_labels_ignore_email_differences_for_same_account() -> anyhow::Result<()> {
        let config = SlopForkConfig {
            show_account_numbers_instead_of_emails: true,
            ..SlopForkConfig::default()
        };
        let saved_auth = chatgpt_auth("acct-a", "alpha@example.com");
        let mut current_auth = saved_auth.clone();
        current_auth.tokens.as_mut().expect("tokens").id_token.email =
            Some("renamed@example.com".to_string());
        let account = StoredAccount {
            id: stored_account_id(&saved_auth).expect("saved account id"),
            path: PathBuf::from("acct-a.json"),
            auth: saved_auth,
            modified_at: None,
        };
        let labels = AccountDisplayLabels::from_config(&config, &[account]);

        assert_eq!(
            labels.label_for_auth(&current_auth),
            "Account 1 (Pro)".to_string()
        );
        Ok(())
    }

    #[test]
    fn numbered_display_labels_use_contiguous_numbers_for_duplicate_lookup_keys()
    -> anyhow::Result<()> {
        let config = SlopForkConfig {
            show_account_numbers_instead_of_emails: true,
            ..SlopForkConfig::default()
        };
        let auth_duplicate_a = chatgpt_auth("acct-a", "alpha@example.com");
        let auth_duplicate_b = chatgpt_auth("acct-a", "renamed@example.com");
        let auth_unique = chatgpt_auth("acct-b", "beta@example.com");
        let duplicate_account_a = StoredAccount {
            id: "legacy-alpha".to_string(),
            path: PathBuf::from("legacy-alpha.json"),
            auth: auth_duplicate_a.clone(),
            modified_at: None,
        };
        let duplicate_account_b = StoredAccount {
            id: "legacy-renamed".to_string(),
            path: PathBuf::from("legacy-renamed.json"),
            auth: auth_duplicate_b.clone(),
            modified_at: None,
        };
        let unique_account = StoredAccount {
            id: "acct-b".to_string(),
            path: PathBuf::from("acct-b.json"),
            auth: auth_unique.clone(),
            modified_at: None,
        };

        let labels = AccountDisplayLabels::from_config(
            &config,
            &[duplicate_account_a, duplicate_account_b, unique_account],
        );

        assert_eq!(
            labels.label_for_auth(&auth_duplicate_a),
            "Account 1 (Pro)".to_string()
        );
        assert_eq!(
            labels.label_for_auth(&auth_duplicate_b),
            "Account 1 (Pro)".to_string()
        );
        assert_eq!(
            labels.label_for_auth(&auth_unique),
            "Account 2 (Pro)".to_string()
        );
        Ok(())
    }

    #[test]
    fn upsert_account_keeps_same_saved_id_when_chatgpt_email_changes() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let original = chatgpt_auth("acct-1", "person@example.com");
        let renamed = chatgpt_auth("acct-1", "renamed@example.com");

        let first_id = upsert_account(dir.path(), &original)?.expect("saved account id");
        let second_id = upsert_account(dir.path(), &renamed)?.expect("saved account id");
        let accounts = list_accounts(dir.path())?;

        assert_eq!(second_id, first_id);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, first_id);
        assert_eq!(
            accounts[0]
                .auth
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.id_token.email.as_deref()),
            Some("renamed@example.com")
        );
        Ok(())
    }

    #[test]
    fn rate_limit_snapshot_lookup_ids_include_legacy_email_derived_saved_id() -> anyhow::Result<()>
    {
        let auth = chatgpt_auth("acct-1", "person@example.com");
        let account = StoredAccount {
            id: stored_account_id(&auth).expect("saved account id"),
            path: PathBuf::from("acct-1.json"),
            auth: auth.clone(),
            modified_at: None,
        };

        let lookup_ids = rate_limit_snapshot_lookup_ids(&account);

        assert!(lookup_ids.contains(&account.id));
        assert!(lookup_ids.contains(&"acct-1".to_string()));
        assert!(
            lookup_ids.contains(
                &legacy_chatgpt_saved_account_id(&auth).expect("legacy saved account id")
            )
        );
        Ok(())
    }

    #[test]
    fn misnamed_account_files_are_reported_as_rename_suggestions() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let auth = chatgpt_auth("acct-1", "person@example.com");
        let wrong_path = accounts_dir(dir.path()).join("person.json");
        std::fs::create_dir_all(accounts_dir(dir.path()))?;
        write_auth_file(&wrong_path, &auth)?;

        let account_id = stored_account_id(&auth).expect("expected account id");
        let accounts = list_accounts(dir.path())?;
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, account_id);
        assert_eq!(accounts[0].path, wrong_path);

        let suggestions = list_account_rename_suggestions(dir.path())?;
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].path, wrong_path);
        assert_eq!(
            suggestions[0].suggested_id,
            stored_account_id(&auth).expect("expected account id")
        );
        assert_eq!(suggestions[0].target_exists, false);
        Ok(())
    }

    #[test]
    fn renaming_misnamed_account_file_makes_it_usable() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let auth = chatgpt_auth("acct-1", "person@example.com");
        let wrong_path = accounts_dir(dir.path()).join("person.json");
        std::fs::create_dir_all(accounts_dir(dir.path()))?;
        write_auth_file(&wrong_path, &auth)?;

        assert!(rename_account_file(dir.path(), &wrong_path)?);

        let account_id = stored_account_id(&auth).expect("expected account id");
        let accounts = list_accounts(dir.path())?;
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, account_id);
        assert_eq!(accounts[0].path, account_path(dir.path(), &account_id));
        assert_eq!(list_account_rename_suggestions(dir.path())?, Vec::new());
        Ok(())
    }

    #[test]
    fn removing_account_deletes_misnamed_file_when_no_canonical_file_exists() -> anyhow::Result<()>
    {
        let dir = tempdir()?;
        let auth = chatgpt_auth("acct-1", "person@example.com");
        let wrong_path = accounts_dir(dir.path()).join("person.json");
        std::fs::create_dir_all(accounts_dir(dir.path()))?;
        write_auth_file(&wrong_path, &auth)?;

        let account_id = stored_account_id(&auth).expect("expected account id");
        assert!(remove_account(dir.path(), &account_id)?);
        assert!(!wrong_path.exists());
        assert_eq!(list_accounts(dir.path())?, Vec::new());
        Ok(())
    }

    #[test]
    fn rename_all_account_files_renames_only_usable_entries() -> anyhow::Result<()> {
        let dir = tempdir()?;
        std::fs::create_dir_all(accounts_dir(dir.path()))?;

        let renameable_auth = chatgpt_auth("acct-rename", "renameable@example.com");
        let renameable_path = accounts_dir(dir.path()).join("renameable.json");
        write_auth_file(&renameable_path, &renameable_auth)?;

        let duplicate_auth = chatgpt_auth("acct-duplicate", "duplicate@example.com");
        let duplicate_wrong_path = accounts_dir(dir.path()).join("duplicate.json");
        write_auth_file(&duplicate_wrong_path, &duplicate_auth)?;
        let duplicate_correct_path = account_path(
            dir.path(),
            &stored_account_id(&duplicate_auth).expect("expected account id"),
        );
        write_auth_file(&duplicate_correct_path, &duplicate_auth)?;

        let result = rename_all_account_files(dir.path())?;
        assert_eq!(
            result,
            RenameAllAccountsResult {
                renamed_count: 1,
                skipped_existing_count: 1,
            }
        );

        let accounts = list_accounts(dir.path())?;
        assert_eq!(accounts.len(), 2);
        let suggestions = list_account_rename_suggestions(dir.path())?;
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].path, duplicate_wrong_path);
        Ok(())
    }

    #[test]
    fn rate_limits_sidecar_is_ignored_when_listing_accounts() -> anyhow::Result<()> {
        let dir = tempdir()?;
        std::fs::create_dir_all(accounts_dir(dir.path()))?;
        std::fs::write(
            accounts_dir(dir.path()).join(".rate-limits.json"),
            r#"{"version":1,"snapshots":{}}"#,
        )?;

        assert_eq!(list_accounts(dir.path())?, Vec::new());
        assert_eq!(list_account_rename_suggestions(dir.path())?, Vec::new());
        Ok(())
    }
}
