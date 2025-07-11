use anyhow::Result;
use rmcp::{
    ServerHandler,
    model::{
        CallToolRequestParam, CallToolResult, Content, JsonObject, ListToolsResult,
        PaginatedRequestParam, ServerCapabilities, ServerInfo,
    },
    service::{RequestContext, RoleServer},
    transport::sse_server::SseServer,
};
use std::net::SocketAddr;

mod interfaces;

use crate::components::{ComponentSpec, Invoker};
use interfaces::{ComponentTool, Parser};

#[derive(Clone)]
pub struct ComponentServer {
    tools: Vec<(ComponentTool, ComponentSpec)>,
    invoker: Invoker,
}

impl ComponentServer {
    pub fn new(component_specs: Vec<ComponentSpec>) -> Result<Self> {
        let mut tools = Vec::new();
        for spec in component_specs {
            match Parser::parse(&spec.bytes, &spec.name) {
                Ok(component_tools) => {
                    for tool in component_tools {
                        tools.push((tool, spec.clone()));
                    }
                }
                Err(e) => {
                    eprintln!("Failed to parse component {}: {}", spec.name, e);
                }
            }
        }
        let invoker = Invoker::new()?;
        Ok(Self { tools, invoker })
    }

    pub async fn run(self, addr: SocketAddr) -> Result<()> {
        println!("🔧 Modulewise Toolbelt MCP Server");
        println!("📡 SSE endpoint: http://{addr}/sse");
        let cancellation_token = SseServer::serve(addr)
            .await?
            .with_service(move || self.clone());
        tokio::signal::ctrl_c().await?;
        cancellation_token.cancel();
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
                &spec.capabilities,
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
    ) -> Result<CallToolResult, rmcp::Error> {
        let arguments = request.arguments.unwrap_or_default();
        if self.tools.iter().any(|(t, _)| t.tool.name == request.name) {
            self.handle_tool_call(&request.name, &arguments)
                .await
                .map_err(|e| {
                    rmcp::Error::internal_error(format!("Component tool error: {e}"), None)
                })
        } else {
            Err(rmcp::Error::invalid_params(
                format!("Unknown tool: {}", request.name),
                None,
            ))
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::Error> {
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
