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
use composable_runtime::{
    ComponentRegistry, ComponentSpec, Function, Invoker, RuntimeFeatureRegistry,
};

#[derive(Clone)]
pub struct ComponentServer {
    tools: HashMap<String, (Tool, Function, ComponentSpec)>,
    invoker: Invoker,
    runtime_feature_registry: RuntimeFeatureRegistry,
}

impl ComponentServer {
    pub fn new(
        runtime_feature_registry: RuntimeFeatureRegistry,
        component_registry: ComponentRegistry,
    ) -> Result<Self> {
        let mut tools = HashMap::new();

        // Process components as tools
        for spec in component_registry.get_components() {
            if let Some(functions_map) = &spec.functions {
                let functions: Vec<Function> = functions_map.values().cloned().collect();
                let mcp_tools = McpMapper::functions_to_tools(functions.clone(), &spec.name)?;

                // Store tools with disambiguated tool names as keys
                for (tool, function) in mcp_tools.into_iter().zip(functions.into_iter()) {
                    tools.insert(tool.name.to_string(), (tool, function, spec.clone()));
                }
                let tool_count = functions_map.len();
                println!(
                    "Loaded {} {} from '{}' with runtime capabilities: {:?}",
                    tool_count,
                    if tool_count == 1 { "tool" } else { "tools" },
                    spec.name,
                    spec.runtime_features
                );
            } else {
                // This should not happen for tools, but handle gracefully
                eprintln!("Warning: Tool component '{}' has no functions", spec.name);
            }
        }
        let invoker = Invoker::new()?;
        Ok(Self {
            tools,
            invoker,
            runtime_feature_registry,
        })
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

    async fn handle_tool_call(
        &self,
        tool_name: &str,
        arguments: &JsonObject,
    ) -> Result<CallToolResult> {
        let (_tool, function, spec) = self
            .tools
            .get(tool_name)
            .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", tool_name))?;

        // Prepare arguments in parameter order
        let mut json_args = Vec::new();
        for param in function.params() {
            if let Some(value) = arguments.get(&param.name) {
                json_args.push(value.clone());
            } else {
                return Err(anyhow::anyhow!(
                    "Missing required parameter: {}",
                    param.name
                ));
            }
        }

        match self
            .invoker
            .invoke(
                &spec.bytes,
                &spec.runtime_features,
                &self.runtime_feature_registry,
                function.clone(),
                json_args,
            )
            .await
        {
            Ok(result) => {
                let result_text = if result.is_string() {
                    result.as_str().unwrap_or("").to_string()
                } else {
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
                };
                Ok(CallToolResult::success(vec![Content::text(result_text)]))
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
