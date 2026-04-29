use anyhow::Result;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse_completed;
use core_test_support::responses::sse_failed;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;

fn server_overloaded_sse(id: &str) -> String {
    sse_failed(
        id,
        "server_is_overloaded",
        "Selected model is at capacity. Please try a different model.",
    )
}

async fn submit_user_input(test: &TestCodex) -> Result<()> {
    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    Ok(())
}

async fn collect_retry_events(test: &TestCodex) -> (Vec<String>, Vec<String>) {
    let mut stream_errors = Vec::new();
    let mut errors = Vec::new();

    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::StreamError(event) => stream_errors.push(event.message),
            EventMsg::Error(event) => errors.push(event.message),
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    (stream_errors, errors)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_overloaded_retries_by_default() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response_mock = mount_sse_sequence(
        &server,
        vec![server_overloaded_sse("resp-1"), sse_completed("resp-2")],
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.model_provider.stream_max_retries = Some(1);
        config.model_provider.request_max_retries = Some(0);
    });
    let test = builder.build(&server).await?;

    submit_user_input(&test).await?;
    let (stream_errors, errors) = collect_retry_events(&test).await;

    assert_eq!(
        stream_errors,
        vec!["Model at capacity; retrying... 1/1".to_string()]
    );
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(response_mock.requests().len(), 2);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_overloaded_retry_can_be_disabled_in_fork_config() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response_mock = mount_sse_sequence(&server, vec![server_overloaded_sse("resp-1")]).await;

    let mut builder = test_codex()
        .with_pre_build_hook(|codex_home| {
            std::fs::write(
                codex_home.join("config-slop-fork.toml"),
                "retry_model_at_capacity = false\n",
            )
            .expect("write fork config");
        })
        .with_config(|config| {
            config.model_provider.stream_max_retries = Some(1);
            config.model_provider.request_max_retries = Some(0);
        });
    let test = builder.build(&server).await?;

    submit_user_input(&test).await?;
    let (stream_errors, errors) = collect_retry_events(&test).await;

    assert_eq!(stream_errors, Vec::<String>::new());
    assert_eq!(
        errors,
        vec!["Selected model is at capacity. Please try a different model.".to_string()]
    );
    assert_eq!(response_mock.requests().len(), 1);

    Ok(())
}
