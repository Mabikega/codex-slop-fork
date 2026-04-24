#![expect(clippy::expect_used)]
#![expect(clippy::unwrap_used)]

use anyhow::Result;
use codex_core::CodexAuth;
use codex_core::auth::AuthCredentialsStoreMode;
use codex_core::auth::AuthDotJson;
use codex_core::auth::load_auth_dot_json;
use codex_core::slop_fork::auth_accounts;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_message_item_added;
use core_test_support::responses::ev_output_text_delta;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::header_regex;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[expect(clippy::unwrap_used)]
fn write_chatgpt_auth_json(
    codex_home: &TempDir,
    email: &str,
    account_id: &str,
    access_token: &str,
) -> AuthDotJson {
    use base64::Engine as _;

    let header = json!({ "alg": "none", "typ": "JWT" });
    let payload = json!({
        "email": email,
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_account_id": account_id,
        }
    });

    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let header_b64 = b64(&serde_json::to_vec(&header).unwrap());
    let payload_b64 = b64(&serde_json::to_vec(&payload).unwrap());
    let signature_b64 = b64(b"sig");
    let fake_jwt = format!("{header_b64}.{payload_b64}.{signature_b64}");

    let auth_json = json!({
        "tokens": {
            "id_token": fake_jwt,
            "access_token": access_token,
            "refresh_token": "refresh-test",
            "account_id": account_id,
        },
        "last_refresh": chrono::Utc::now(),
    });

    std::fs::write(
        codex_home.path().join("auth.json"),
        serde_json::to_string_pretty(&auth_json).unwrap(),
    )
    .unwrap();

    load_auth_dot_json(codex_home.path(), AuthCredentialsStoreMode::File)
        .unwrap()
        .expect("auth.json should deserialize")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quota_exceeded_emits_single_error_event() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            json!({
                "type": "response.failed",
                "response": {
                    "id": "resp-1",
                    "error": {
                        "code": "insufficient_quota",
                        "message": "You exceeded your current quota, please check your plan and billing details."
                    }
                }
            }),
        ]),
    )
    .await;

    let test = builder.build(&server).await?;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "quota?".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await
        .unwrap();

    let mut error_events = 0;

    loop {
        let event = wait_for_event(&test.codex, |_| true).await;

        match event {
            EventMsg::Error(err) => {
                error_events += 1;
                assert_eq!(
                    err.message,
                    "Quota exceeded. Check your plan and billing details."
                );
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(error_events, 1, "expected exactly one Codex:Error event");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_limit_switches_to_another_saved_account_and_retries_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let home = std::sync::Arc::new(TempDir::new()?);
    let active_auth_json =
        write_chatgpt_auth_json(&home, "first@example.com", "acct-first", "access-first");
    let active_auth = CodexAuth::from_auth_storage(home.path(), AuthCredentialsStoreMode::File)?
        .expect("active auth");

    let saved_active_account_id =
        auth_accounts::upsert_account(home.path(), &active_auth_json)?.expect("active account id");

    let saved_auth_json =
        write_chatgpt_auth_json(&home, "second@example.com", "acct-second", "access-second");
    let saved_account_id =
        auth_accounts::upsert_account(home.path(), &saved_auth_json)?.expect("saved account id");
    let saved_account_path = home
        .path()
        .join(".accounts")
        .join(format!("{saved_account_id}.json"));
    let legacy_saved_account_path = home.path().join(".accounts").join("second.json");
    std::fs::rename(&saved_account_path, &legacy_saved_account_path)?;
    std::fs::write(
        home.path().join("auth.json"),
        serde_json::to_string_pretty(&active_auth_json)?,
    )?;

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header_regex("authorization", "Bearer access-first"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "limit reached",
                "resets_at": 1704067242,
                "plan_type": "team"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header_regex("authorization", "Bearer access-second"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(
                    sse(vec![
                        ev_response_created("resp-2"),
                        ev_message_item_added("msg-2", ""),
                        ev_output_text_delta("switched ok"),
                        ev_completed("resp-2"),
                    ]),
                    "text/event-stream",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut builder = test_codex()
        .with_home(home.clone())
        .with_auth(active_auth)
        .with_config(|config| {
            config.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;
        });
    let test = builder.build(&server).await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            environments: None,
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await
        .expect("submission should succeed after switching accounts");

    let warning_event =
        wait_for_event(&test.codex, |msg| matches!(msg, EventMsg::Warning(_))).await;
    let EventMsg::Warning(warning_event) = warning_event else {
        unreachable!();
    };
    assert!(
        warning_event.message.contains("Switched to saved account"),
        "unexpected warning message: {}",
        warning_event.message
    );

    let turn_complete =
        wait_for_event(&test.codex, |msg| matches!(msg, EventMsg::TurnComplete(_))).await;
    assert!(matches!(turn_complete, EventMsg::TurnComplete(_)));

    let active_auth_after = load_auth_dot_json(home.path(), AuthCredentialsStoreMode::File)?
        .expect("switched auth should be saved");
    let active_account_after =
        auth_accounts::current_active_account_id(home.path(), AuthCredentialsStoreMode::File)?;
    assert_eq!(active_account_after, Some(saved_account_id.clone()));
    assert_eq!(
        auth_accounts::find_account(home.path(), &saved_account_id)?
            .expect("saved account should exist")
            .auth,
        active_auth_after
    );
    assert_ne!(saved_active_account_id, saved_account_id);

    Ok(())
}
