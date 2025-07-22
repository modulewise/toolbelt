use anyhow::Result;
use rmcp::{
    ServerHandler,
    model::{
        CallToolRequestParam, CallToolResult, Content, JsonObject, ListToolsResult,
        PaginatedRequestParam, ServerCapabilities, ServerInfo,
    },
    service::{RequestContext, RoleServer},
    transport::StreamableHttpService,
    transport::streamable_http_server::session::local::LocalSessionManager,
};
use std::net::SocketAddr;

use crate::capabilities::CapabilityRegistry;
use crate::components::{ComponentSpec, Invoker};
use crate::interfaces::{ComponentTool, Parser};
use crate::resolver::ToolRegistry;

#[derive(Clone)]
pub struct ComponentServer {
    tools: Vec<(ComponentTool, ComponentSpec)>,
    invoker: Invoker,
    capability_registry: CapabilityRegistry,
}

impl ComponentServer {
    pub fn new(
        capability_registry: CapabilityRegistry,
        tool_registry: ToolRegistry,
    ) -> Result<Self> {
        let mut tools = Vec::new();
        for (_name, spec) in tool_registry {
            match Parser::parse(&spec.bytes, &spec.name) {
                Ok(component_tools) => {
                    for tool in component_tools {
                        tools.push((tool, spec.clone()));
                    }
                    println!(
                        "Loaded tool '{}' with runtime capabilities: {:?}",
                        spec.name, spec.runtime_capabilities
                    );
                }
                Err(e) => {
                    eprintln!("Failed to parse component {}: {}", spec.name, e);
                }
            }
        }
        let invoker = Invoker::new()?;
        Ok(Self {
            tools,
            invoker,
            capability_registry,
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
        let (tool, spec) = self
            .tools
            .iter()
            .find(|(t, _)| t.tool.name == tool_name)
            .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", tool_name))?;

        // Prepare arguments in parameter order
        let mut json_args = Vec::new();
        for param in &tool.params {
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
                &tool.bytes,
                &spec.runtime_capabilities,
                &self.capability_registry,
                tool.namespace.clone(),
                tool.package.clone(),
                tool.version.clone(),
                tool.interface.clone(),
                tool.function.clone(),
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
        if self.tools.iter().any(|(t, _)| t.tool.name == request.name) {
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
        let tools = self.tools.iter().map(|(t, _)| t.tool.clone()).collect();
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
