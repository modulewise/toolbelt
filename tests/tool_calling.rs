mod common;

use composable_runtime::{ComponentGraph, Runtime};
use rmcp::model::CallToolRequestParams;
use toolbelt::server::ComponentServer;

#[tokio::test]
async fn test_tool_invocation() {
    let component_wasm = common::add_two_component();
    let graph = ComponentGraph::builder()
        .load_file(component_wasm.to_path_buf())
        .build()
        .unwrap();
    let runtime = Runtime::builder(&graph).build().await.unwrap();
    let server_handler = ComponentServer::new(runtime).unwrap();

    let client = common::setup_test_client(server_handler).await;

    // Test list_tools returns the expected tool
    let tools_result = client.list_tools(None).await.unwrap();
    assert_eq!(tools_result.tools.len(), 1);

    let tool = &tools_result.tools[0];
    assert!(
        tool.name.ends_with(".add-two"),
        "Tool name should end with .add-two, got: {}",
        tool.name
    );

    // Input schema should have one required param "x"
    let input_schema = &tool.input_schema;
    assert_eq!(input_schema.get("type").unwrap(), "object");

    let properties = input_schema.get("properties").unwrap().as_object().unwrap();
    assert!(properties.contains_key("x"), "Should have param 'x'");

    let required = input_schema.get("required").unwrap().as_array().unwrap();
    assert_eq!(required.len(), 1);
    assert_eq!(required[0], "x");

    // Test call_tool invokes the component
    let request = CallToolRequestParams {
        name: tool.name.clone().into(),
        arguments: Some(args!({"x": 5})),
        task: None,
        meta: None,
    };

    let result = client.call_tool(request).await.unwrap();
    assert!(!result.is_error.unwrap_or(false));

    let content = &result.content[0];
    let result_value: i32 = content.as_text().unwrap().text.trim().parse().unwrap();
    assert_eq!(result_value, 7);
}

#[tokio::test]
async fn test_missing_required_parameter() {
    let component_wasm = common::add_two_component();
    let graph = ComponentGraph::builder()
        .load_file(component_wasm.to_path_buf())
        .build()
        .unwrap();
    let runtime = Runtime::builder(&graph).build().await.unwrap();
    let server_handler = ComponentServer::new(runtime).unwrap();

    let client = common::setup_test_client(server_handler).await;

    let tools_result = client.list_tools(None).await.unwrap();
    let tool = &tools_result.tools[0];

    let request = CallToolRequestParams {
        name: tool.name.clone().into(),
        arguments: Some(args!({})),
        task: None,
        meta: None,
    };

    let result = client.call_tool(request).await.unwrap();
    assert!(result.is_error.unwrap_or(false));

    let content = &result.content[0];
    let text = content.as_text().unwrap().text.as_str();
    assert!(text.contains("Missing required parameter"));
    assert!(text.contains("x"));
}

#[tokio::test]
async fn test_tool_not_found() {
    let component_wasm = common::add_two_component();
    let graph = ComponentGraph::builder()
        .load_file(component_wasm.to_path_buf())
        .build()
        .unwrap();
    let runtime = Runtime::builder(&graph).build().await.unwrap();
    let server_handler = ComponentServer::new(runtime).unwrap();

    let client = common::setup_test_client(server_handler).await;

    let request = CallToolRequestParams {
        name: "nonexistent-tool".into(),
        arguments: None,
        task: None,
        meta: None,
    };

    let result = client.call_tool(request).await.unwrap();
    assert!(result.is_error.unwrap_or(false));

    let content = &result.content[0];
    let text = content.as_text().unwrap().text.as_str();
    assert!(text.contains("Tool not found"));
    assert!(text.contains("nonexistent-tool"));
}

#[tokio::test]
async fn test_optional_parameter_handling() {
    let wat = r#"
        (component
            (core module $m
                (func $get_value (param i32) (result i32)
                    local.get 0
                    i32.const 0
                    i32.eq
                    if (result i32)
                        i32.const 42
                    else
                        local.get 0
                    end
                )
                (export "get-value" (func $get_value))
            )
            (core instance $i (instantiate $m))
            (func $f
                (param "value" s32)
                (result s32)
                (canon lift (core func $i "get-value"))
            )
            (export "get-value" (func $f))
        )
    "#;
    let component_wasm = common::create_wasm_test_file(wat);
    let graph = ComponentGraph::builder()
        .load_file(component_wasm.to_path_buf())
        .build()
        .unwrap();
    let runtime = Runtime::builder(&graph).build().await.unwrap();
    let server_handler = ComponentServer::new(runtime).unwrap();

    let client = common::setup_test_client(server_handler).await;

    let tools_result = client.list_tools(None).await.unwrap();
    let tool = &tools_result.tools[0];

    let request = CallToolRequestParams {
        name: tool.name.clone().into(),
        arguments: Some(args!({"value": 0})),
        task: None,
        meta: None,
    };

    let result = client.call_tool(request).await.unwrap();
    assert!(!result.is_error.unwrap_or(false));

    let content = &result.content[0];
    let result_value: i32 = content.as_text().unwrap().text.trim().parse().unwrap();
    assert_eq!(result_value, 42);

    // Test with non-zero value echoes back
    let request = CallToolRequestParams {
        name: tool.name.clone().into(),
        arguments: Some(args!({"value": 99})),
        task: None,
        meta: None,
    };

    let result = client.call_tool(request).await.unwrap();
    assert!(!result.is_error.unwrap_or(false));

    let content = &result.content[0];
    let result_value: i32 = content.as_text().unwrap().text.trim().parse().unwrap();
    assert_eq!(result_value, 99);
}
