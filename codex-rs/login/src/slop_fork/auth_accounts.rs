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
use crate::token_data::parse_chatgpt_jwt_claims;
use crate::token_data::parse_chatgpt_subscription_active_until;

use super::config::SlopForkConfig;
use super::config::maybe_load_slop_fork_config;
use super::save_auth_with_account_sync;

const ACCOUNTS_DIR: &str = ".accounts";

#[derive(Debug, Clone, PartialEq)]
pub struct StoredAccount {
    pub id: String,
    pub path: PathBuf,
    pub auth: AuthDotJson,
    pub modified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountDisplayLabels {
    chatgpt_labels_by_lookup_key: HashMap<String, String>,
}

#[derive(Debug)]
struct ParsedAccountFile {
    path: PathBuf,
    auth: AuthDotJson,
    modified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChatgptAccountLabelKeys {
    lookup_key: String,
    sort_key: (String, String, String),
}

pub fn accounts_dir(codex_home: &Path) -> PathBuf {
    codex_home.join(ACCOUNTS_DIR)
}

pub fn load_account_display_labels(codex_home: &Path) -> AccountDisplayLabels {
    let config = maybe_load_slop_fork_config(codex_home).unwrap_or_default();
    let accounts = list_accounts(codex_home).unwrap_or_default();
    AccountDisplayLabels::from_config(config, &accounts)
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
    pub fn from_config(config: SlopForkConfig, accounts: &[StoredAccount]) -> Self {
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

pub fn list_accounts(codex_home: &Path) -> std::io::Result<Vec<StoredAccount>> {
    let mut accounts_by_id = HashMap::new();
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

pub fn find_account(codex_home: &Path, account_id: &str) -> std::io::Result<Option<StoredAccount>> {
    Ok(list_accounts(codex_home)?
        .into_iter()
        .find(|account| account.id == account_id))
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

pub fn account_has_credentials(account: &StoredAccount) -> bool {
    match account.auth.resolved_mode() {
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => account.auth.tokens.is_some(),
        AuthMode::ApiKey => account.auth.openai_api_key.is_some(),
    }
}

pub fn saved_account_subscription_ran_out_from_plan(
    account: &StoredAccount,
    current_plan: Option<&str>,
    workspace_deactivated: bool,
    observed_at: Option<DateTime<Utc>>,
    has_live_snapshot: bool,
) -> bool {
    if !account.auth.is_chatgpt_mode() {
        return false;
    }

    if workspace_deactivated {
        return true;
    }
    if auth_chatgpt_subscription_active_until_indicates_expired(
        &account.auth,
        observed_at,
        has_live_snapshot,
        Utc::now(),
    ) {
        return true;
    }

    let Some(snapshot_plan) = current_plan.map(normalized_chatgpt_plan) else {
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
    std::fs::create_dir_all(accounts_dir(codex_home))?;
    write_auth_file(&path, &auth)?;
    Ok(Some(account_id))
}

pub fn upsert_account(codex_home: &Path, auth: &AuthDotJson) -> std::io::Result<Option<String>> {
    let Some(account_id) = stored_account_id(auth) else {
        return Ok(None);
    };

    std::fs::create_dir_all(accounts_dir(codex_home))?;
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
    observed_at: Option<DateTime<Utc>>,
    has_live_snapshot: bool,
    now: DateTime<Utc>,
) -> bool {
    let Some(active_until) = auth_chatgpt_subscription_active_until(auth) else {
        return false;
    };
    if now <= active_until {
        return false;
    }

    !(has_live_snapshot && observed_at.is_some_and(|observed_at| observed_at > active_until))
}

fn normalized_chatgpt_plan(plan: impl AsRef<str>) -> String {
    plan.as_ref().trim().to_ascii_lowercase()
}

fn scan_account_files(codex_home: &Path) -> std::io::Result<Vec<ParsedAccountFile>> {
    let entries = match std::fs::read_dir(accounts_dir(codex_home)) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

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
