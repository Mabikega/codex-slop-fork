#![allow(clippy::expect_used, clippy::unwrap_used)]

use anyhow::Result;
use codex_config::McpServerConfig;
use codex_config::McpServerTransportConfig;
use codex_features::Feature;
use core_test_support::responses;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::stdio_server_bin;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use wiremock::MockServer;

fn custom_tool_output_items(req: &ResponsesRequest, call_id: &str) -> Vec<Value> {
    req.custom_tool_call_output(call_id)
        .get("output")
        .and_then(Value::as_array)
        .expect("custom tool output should be serialized as content items")
        .clone()
}

fn custom_tool_output_body_and_success(
    req: &ResponsesRequest,
    call_id: &str,
) -> (String, Option<bool>) {
    let (_, success) = req
        .custom_tool_call_output_content_and_success(call_id)
        .expect("custom tool output should be present");
    let items = custom_tool_output_items(req, call_id);
    let output = items
        .iter()
        .skip(1)
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect();
    (output, success)
}

async fn run_code_mode_turn_with_rmcp(
    server: &MockServer,
    prompt: &str,
    code: &str,
) -> Result<(TestCodex, ResponseMock)> {
    let rmcp_test_server_bin = stdio_server_bin()?;
    let mut builder = test_codex().with_config(move |config| {
        let _ = config.features.enable(Feature::CodeMode);

        let mut servers = config.mcp_servers.get().clone();
        servers.insert(
            "rmcp".to_string(),
            McpServerConfig {
                transport: McpServerTransportConfig::Stdio {
                    command: rmcp_test_server_bin,
                    args: Vec::new(),
                    env: Some(HashMap::from([(
                        "MCP_TEST_VALUE".to_string(),
                        "propagated-env".to_string(),
                    )])),
                    env_vars: Vec::new(),
                    cwd: None,
                },
                enabled: true,
                required: false,
                disabled_reason: None,
                startup_timeout_sec: Some(Duration::from_secs(10)),
                tool_timeout_sec: None,
                enabled_tools: None,
                disabled_tools: None,
                scopes: None,
                oauth_resource: None,
                tools: HashMap::new(),
            },
        );
        config
            .mcp_servers
            .set(servers)
            .expect("test mcp servers should accept any configuration");
    });
    let test = builder.build(server).await?;

    responses::mount_sse_once(
        server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_custom_tool_call("call-1", "exec", code),
            ev_completed("resp-1"),
        ]),
    )
    .await;

    let second_mock = responses::mount_sse_once(
        server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    test.submit_turn(prompt).await?;
    Ok((test, second_mock))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_mode_keeps_flat_tools_namespace_compat_for_namespaced_mcp_tools() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let code = r#"
const { structuredContent, isError } = await tools["mcp__rmcp__echo"]({
  message: "ping",
});
text(
  `echo=${structuredContent?.echo ?? "missing"}
` +
    `env=${structuredContent?.env ?? "missing"}
` +
    `isError=${String(isError)}`
);
"#;

    let (_test, second_mock) =
        run_code_mode_turn_with_rmcp(&server, "use exec to run the rmcp echo tool", code).await?;

    let req = second_mock.single_request();
    let (output, success) = custom_tool_output_body_and_success(&req, "call-1");
    assert_ne!(
        success,
        Some(false),
        "exec rmcp echo call via tools.js compatibility failed unexpectedly: {output}"
    );
    assert_eq!(
        output,
        "echo=ECHOING: ping
env=propagated-env
isError=false"
    );

    Ok(())
}
