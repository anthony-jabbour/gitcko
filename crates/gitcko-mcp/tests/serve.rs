// SPDX-License-Identifier: AGPL-3.0-or-later

//! The stdio server advertises tools and nothing else. Framing is rmcp's.

use rmcp::ServiceExt;

#[tokio::test]
async fn initialize_advertises_tools_and_no_other_capability() {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server = gitcko_mcp::test_server();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("the client finishes initialize");
    let info = client
        .peer()
        .peer_info()
        .expect("server info after initialize");
    assert!(info.capabilities.tools.is_some(), "{info:?}");
    assert!(
        info.capabilities
            .tools
            .as_ref()
            .unwrap()
            .list_changed
            .is_none()
    );
    assert!(info.capabilities.resources.is_none());
    assert!(info.capabilities.prompts.is_none());
    assert!(info.capabilities.logging.is_none());
    let tools = client
        .peer()
        .list_tools(Default::default())
        .await
        .expect("tools/list");
    let again = client
        .peer()
        .list_tools(Default::default())
        .await
        .expect("tools/list");
    let names: Vec<_> = tools
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert_eq!(
        names,
        [
            "gitcko_review",
            "gitcko_review_status",
            "gitcko_get_review",
            "gitcko_list_findings",
        ]
    );
    for tool in &tools.tools {
        assert!(
            !matches!(
                tool.name.as_ref(),
                "gitcko_query_repo"
                    | "gitcko_find_symbol"
                    | "gitcko_find_callers"
                    | "gitcko_find_references"
                    | "gitcko_index"
                    | "gitcko_verify"
            ),
            "{}",
            tool.name
        );
        let schema = serde_json::to_value(&*tool.input_schema).unwrap();
        assert_eq!(schema["additionalProperties"], false, "{}", tool.name);
        assert!(tool.output_schema.is_some(), "{}", tool.name);
    }
    let review = &tools.tools[0];
    let hints = review.annotations.as_ref().unwrap();
    assert_eq!(hints.read_only_hint, Some(false));
    assert_eq!(hints.destructive_hint, Some(false));
    assert_eq!(hints.idempotent_hint, Some(false));
    assert_eq!(hints.open_world_hint, Some(false));
    assert_eq!(
        tools.tools[1].annotations.as_ref().unwrap().read_only_hint,
        Some(true)
    );
    let first = serde_json::to_vec(&tools).unwrap();
    let second = serde_json::to_vec(&again).unwrap();
    assert_eq!(first, second, "tools/list is stable across calls");
    drop(client);
    server_task.abort();
}
