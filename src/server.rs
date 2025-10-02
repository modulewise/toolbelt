use anyhow::Result;
use rmcp::{
    ServerHandler,
    model::{
        CallToolRequestParam, CallToolResult, Content, JsonObject, ListToolsResult,
        PaginatedRequestParam, ServerCapabilities, ServerInfo, Tool,
    },
    service::{RequestContext, RoleServer},
    transport::StreamableHttpService,
    transport::streamable_http_server::session::local::LocalSessionManager,
};
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::mapper::McpMapper;
use composable_runtime::{Function, Runtime};

type ComponentName = String;

#[derive(Clone)]
pub struct ComponentServer {
    tools: HashMap<String, (Tool, Function, ComponentName)>,
    runtime: Runtime,
}

impl ComponentServer {
    pub fn new(runtime: Runtime) -> Result<Self> {
        let mut tools = HashMap::new();

        // Process components as tools
        for component in runtime.list_components() {
            let functions: Vec<Function> = component.functions.values().cloned().collect();
            let mcp_tools = McpMapper::functions_to_tools(functions.clone(), &component.name)?;

            // Store tools with disambiguated tool names as keys
            for (tool, function) in mcp_tools.into_iter().zip(functions.into_iter()) {
                tools.insert(
                    tool.name.to_string(),
                    (tool, function, component.name.clone()),
                );
            }
            let tool_count = component.functions.len();
            println!(
                "Loaded {} {} from '{}'",
                tool_count,
                if tool_count == 1 { "tool" } else { "tools" },
                component.name
            );
        }
        Ok(Self { tools, runtime })
    }

    pub async fn run(self, addr: SocketAddr) -> Result<()> {
        println!("🔧 Modulewise Toolbelt MCP Server");

        let service = StreamableHttpService::new(
            move || Ok(self.clone()),
            LocalSessionManager::default().into(),
            Default::default(),
        );

        let router = axum::Router::new().nest_service("/mcp", service);
        let tcp_listener = tokio::net::TcpListener::bind(addr).await?;

        println!("📡 Streamable HTTP endpoint: http://{addr}/mcp");

        tokio::select! {
            result = axum::serve(tcp_listener, router) => {
                if let Err(err) = result {
                    eprintln!("Server error: {err}");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("Received Ctrl+C, shutting down...");
            }
        }

        Ok(())
    }

    fn result_to_structured_content(
        &self,
        tool: &Tool,
        raw_result: serde_json::Value,
    ) -> serde_json::Value {
        let parsed_result = if raw_result.is_string() {
            serde_json::from_str::<serde_json::Value>(raw_result.as_str().unwrap())
                .unwrap_or(raw_result)
        } else {
            raw_result
        };

        // Check if this is a wrapper schema (array or option) and wrap accordingly
        if let Some(schema) = &tool.output_schema {
            if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
                if properties.len() == 1 {
                    if let Some((property_name, property_schema)) = properties.iter().next() {
                        if property_schema.get("type").and_then(|t| t.as_str()) == Some("array")
                            || property_schema.get("oneOf").is_some()
                        {
                            return serde_json::json!({ property_name: parsed_result });
                        }
                    }
                }
            }
        }
        parsed_result
    }

    async fn handle_tool_call(
        &self,
        tool_name: &str,
        arguments: &JsonObject,
    ) -> Result<CallToolResult> {
        let (tool, function, component_name) = self
            .tools
            .get(tool_name)
            .ok_or_else(|| anyhow::anyhow!("Tool not found: {tool_name}"))?;

        // Prepare arguments in parameter order
        let mut json_args = Vec::new();
        for param in function.params() {
            if param.is_optional {
                if let Some(value) = arguments.get(&param.name) {
                    // Handle empty strings for optional non-string parameters
                    let is_string_type =
                        param.json_schema.get("type") == Some(&serde_json::json!("string"));
                    let processed_value = if value == &serde_json::json!("") && !is_string_type {
                        serde_json::Value::Null
                    } else {
                        value.clone()
                    };
                    json_args.push(processed_value);
                } else {
                    json_args.push(serde_json::Value::Null);
                }
            } else if let Some(value) = arguments.get(&param.name) {
                json_args.push(value.clone());
            } else {
                return Err(anyhow::anyhow!(
                    "Missing required parameter: {}",
                    param.name
                ));
            }
        }

        match self
            .runtime
            .invoke(component_name, function.function_name(), json_args)
            .await
        {
            Ok(result) => {
                if tool.output_schema.is_some() {
                    let structured_content = self.result_to_structured_content(tool, result);

                    // Per MCP spec: "For backwards compatibility, a tool that returns structured content
                    // SHOULD also return the serialized JSON in a TextContent block."
                    // https://modelcontextprotocol.io/specification/2025-06-18/server/tools#structured-content
                    let text_content = serde_json::to_string_pretty(&structured_content)
                        .unwrap_or_else(|_| structured_content.to_string());

                    Ok(CallToolResult {
                        content: vec![Content::text(text_content)],
                        is_error: Some(false),
                        structured_content: Some(structured_content),
                        meta: None,
                    })
                } else {
                    let result_text = if result.is_string() {
                        result.as_str().unwrap_or("").to_string()
                    } else {
                        serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
                    };
                    Ok(CallToolResult::success(vec![Content::text(result_text)]))
                }
            }
            Err(error) => Ok(CallToolResult::error(vec![Content::text(
                error.to_string(),
            )])),
        }
    }
}

impl ServerHandler for ComponentServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let arguments = request.arguments.unwrap_or_default();
        if self.tools.contains_key(request.name.as_ref()) {
            self.handle_tool_call(&request.name, &arguments)
                .await
                .map_err(|e| {
                    rmcp::ErrorData::internal_error(format!("Component tool error: {e}"), None)
                })
        } else {
            Err(rmcp::ErrorData::invalid_params(
                format!("Unknown tool: {}", request.name),
                None,
            ))
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let tools = self.tools.values().map(|(t, _, _)| t.clone()).collect();
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
        })
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: rmcp::model::ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: rmcp::model::Implementation {
                name: "modulewise-toolbelt".to_string(),
                version: "0.1.0".to_string(),
                icons: None,
                title: Some("Modulewise Toolbelt".to_string()),
                website_url: Some("https://github.com/modulewise/toolbelt".to_string()),
            },
            instructions: Some(format!(
                "Use the {} available tools to invoke the underlying Wasm Components. \
                Each tool corresponds to a function exported by a loaded component. \
                Call tools with their required parameters.",
                self.tools.len()
            )),
        }
    }
}
