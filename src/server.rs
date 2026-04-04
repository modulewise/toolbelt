use anyhow::Result;
use opentelemetry::trace::{Span, SpanKind, Status, Tracer, TracerProvider as _};
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{BatchSpanProcessor, SdkTracerProvider};
use rmcp::{
    ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, Content, JsonObject, ListToolsResult, Meta,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::{RequestContext, RoleServer},
    transport::StreamableHttpService,
    transport::streamable_http_server::session::local::LocalSessionManager,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::watch;

use crate::origin::{OriginPolicy, validate_origin};
use composable_runtime::{ComponentInvoker, Function, PROPAGATED_HEADERS};

type ComponentName = String;

#[derive(Clone)]
pub struct McpServer {
    tools: HashMap<String, (Tool, Function, ComponentName)>,
    invoker: Arc<dyn ComponentInvoker>,
    addr: SocketAddr,
    origin_policy: OriginPolicy,
    tracer_provider: Option<Arc<SdkTracerProvider>>,
}

impl McpServer {
    pub fn new(
        tools: HashMap<String, (Tool, Function, ComponentName)>,
        invoker: Arc<dyn ComponentInvoker>,
        addr: SocketAddr,
        origin_policy: OriginPolicy,
        tracer_provider: Option<SdkTracerProvider>,
    ) -> Self {
        Self {
            tools,
            invoker,
            addr,
            origin_policy,
            tracer_provider: tracer_provider.map(Arc::new),
        }
    }

    /// Run the MCP server, listening for HTTP requests until the shutdown signal fires.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let addr = self.addr;
        let origin_policy = self.origin_policy.clone();
        // Keep a handle to the tracer provider for shutdown.
        let tracer_provider = self.tracer_provider.clone();

        let service = StreamableHttpService::new(
            move || Ok(self.clone()),
            LocalSessionManager::default().into(),
            Default::default(),
        );

        let router = axum::Router::new()
            .nest_service("/mcp", service)
            .layer(axum::middleware::from_fn_with_state(
                origin_policy,
                validate_origin,
            ));
        let tcp_listener = tokio::net::TcpListener::bind(addr).await?;

        tracing::info!("Streamable HTTP endpoint: http://{addr}/mcp");

        tokio::select! {
            result = axum::serve(tcp_listener, router) => {
                if let Err(err) = result {
                    tracing::error!("Server error: {err}");
                }
            }
            _ = shutdown.changed() => {
                tracing::info!("MCP server on {addr} shutting down");
            }
        }

        // Shutdown via spawn_blocking since BatchSpanProcessor.shutdown() calls block_on.
        if let Some(provider) = tracer_provider {
            let _ = tokio::task::spawn_blocking(move || provider.shutdown()).await;
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
        if let Some(schema) = &tool.output_schema
            && let Some(properties) = schema.get("properties").and_then(|p| p.as_object())
            && properties.len() == 1
            && let Some((property_name, property_schema)) = properties.iter().next()
            && (property_schema.get("type").and_then(|t| t.as_str()) == Some("array")
                || property_schema.get("oneOf").is_some())
        {
            return serde_json::json!({ property_name: parsed_result });
        }
        parsed_result
    }

    // Coerce a JSON number or boolean to a string when the target WIT type is string.
    fn coerce_to_string(
        value: &serde_json::Value,
        schema: &serde_json::Value,
    ) -> serde_json::Value {
        let is_string_type = schema.get("type") == Some(&serde_json::json!("string"));
        match (is_string_type, value) {
            (true, serde_json::Value::Number(n)) => serde_json::Value::String(n.to_string()),
            (true, serde_json::Value::Bool(b)) => serde_json::Value::String(b.to_string()),
            _ => value.clone(),
        }
    }

    /// Create an MCP server span following the gen_ai semantic conventions.
    /// Returns the span and a propagation context map derived from it.
    ///
    /// Trace context is extracted from `_meta`.
    fn start_mcp_span(
        &self,
        method: &str,
        target: Option<&str>,
        mut attributes: Vec<KeyValue>,
        meta: Option<&Meta>,
    ) -> Option<(opentelemetry_sdk::trace::Span, HashMap<String, String>)> {
        let tp = self.tracer_provider.as_ref()?;
        let tracer = tp.tracer("modulewise-toolbelt");

        let span_name = match target {
            Some(t) => format!("{method} {t}"),
            None => method.to_string(),
        };

        // Extract propagated context from _meta (MCP spec trace propagation)
        let mut context: HashMap<String, String> = HashMap::new();
        if let Some(m) = meta {
            for key in PROPAGATED_HEADERS {
                if let Some(val) = m.0.get(*key).and_then(|v| v.as_str()) {
                    context.insert(key.to_string(), val.to_string());
                }
            }
        }

        let parent_cx = if context.contains_key("traceparent") {
            Some(TraceContextPropagator::new().extract(&context))
        } else {
            None
        };

        attributes.push(KeyValue::new("mcp.method.name", method.to_string()));

        let builder = tracer
            .span_builder(span_name)
            .with_kind(SpanKind::Server)
            .with_attributes(attributes);

        let span = match parent_cx {
            Some(cx) => builder.start_with_context(&tracer, &cx),
            None => builder.start(&tracer),
        };

        // Derive traceparent from the span
        let sc = span.span_context().clone();
        context.insert(
            "traceparent".to_string(),
            format!(
                "00-{:032x}-{:016x}-{:02x}",
                sc.trace_id(),
                sc.span_id(),
                sc.trace_flags()
            ),
        );

        Some((span, context))
    }

    async fn handle_tool_call(
        &self,
        tool_name: &str,
        arguments: &JsonObject,
        context: Option<HashMap<String, String>>,
    ) -> CallToolResult {
        let Some((tool, function, component_name)) = self.tools.get(tool_name) else {
            return CallToolResult::error(vec![Content::text(format!(
                "Tool not found: {tool_name}"
            ))]);
        };

        // Prepare arguments in parameter order
        let mut json_args = Vec::new();
        for param in function.params() {
            if param.is_optional {
                if let Some(value) = arguments.get(&param.name) {
                    let coerced = Self::coerce_to_string(value, &param.json_schema);
                    // Treat empty strings as null for optional non-string parameters
                    let is_string_type =
                        param.json_schema.get("type") == Some(&serde_json::json!("string"));
                    let processed_value = if coerced == serde_json::json!("") && !is_string_type {
                        serde_json::Value::Null
                    } else {
                        coerced
                    };
                    json_args.push(processed_value);
                } else {
                    json_args.push(serde_json::Value::Null);
                }
            } else if let Some(value) = arguments.get(&param.name) {
                json_args.push(Self::coerce_to_string(value, &param.json_schema));
            } else {
                return CallToolResult::error(vec![Content::text(format!(
                    "Missing required parameter: {}",
                    param.name
                ))]);
            }
        }

        match self
            .invoker
            .invoke(component_name, &function.key(), json_args, context, None)
            .await
        {
            Ok(result) => {
                if tool.output_schema.is_some() {
                    let structured_content = self.result_to_structured_content(tool, result);

                    CallToolResult::structured(structured_content)
                } else {
                    let result_text = if result.is_string() {
                        result.as_str().unwrap_or("").to_string()
                    } else {
                        serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
                    };
                    CallToolResult::success(vec![Content::text(result_text)])
                }
            }
            Err(error) => CallToolResult::error(vec![Content::text(error.to_string())]),
        }
    }
}

// Extract gen_ai semantic convention attributes from the request context.
fn request_attributes(context: &RequestContext<RoleServer>) -> Vec<KeyValue> {
    let mut attrs = vec![
        KeyValue::new("jsonrpc.request.id", context.id.to_string()),
        KeyValue::new("network.transport", "tcp"),
    ];

    if let Some(parts) = context.extensions.get::<axum::http::request::Parts>() {
        if let Some(session_id) = parts.headers.get("MCP-Session-Id").and_then(|v| v.to_str().ok())
        {
            attrs.push(KeyValue::new("mcp.session.id", session_id.to_string()));
        }
        if let Some(version) = parts
            .headers
            .get("MCP-Protocol-Version")
            .and_then(|v| v.to_str().ok())
        {
            attrs.push(KeyValue::new(
                "mcp.protocol.version",
                version.to_string(),
            ));
        }
    }

    attrs
}

impl ServerHandler for McpServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let tool_name = &request.name;
        let arguments = request.arguments.unwrap_or_default();

        // rmcp extracts _meta from params during deserialization and places it
        // in RequestContext.meta, not in CallToolRequestParams.meta.
        let meta = if context.meta.0.is_empty() {
            None
        } else {
            Some(&context.meta)
        };

        let mut attrs = vec![
            KeyValue::new("gen_ai.operation.name", "execute_tool"),
            KeyValue::new("gen_ai.tool.name", tool_name.to_string()),
        ];
        attrs.extend(request_attributes(&context));

        let span_ctx = self.start_mcp_span("tools/call", Some(tool_name), attrs, meta);

        let context = span_ctx
            .as_ref()
            .map(|(_, ctx)| ctx.clone());

        let (mut span, result) = {
            let result = self
                .handle_tool_call(tool_name, &arguments, context)
                .await;
            (span_ctx.map(|(span, _)| span), result)
        };

        if let Some(ref mut span) = span {
            if result.is_error.unwrap_or(false) {
                span.set_status(Status::error(""));
                span.set_attribute(KeyValue::new("error.type", "tool_error"));
            }
            span.end();
        }

        Ok(result)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let meta = if context.meta.0.is_empty() {
            None
        } else {
            Some(&context.meta)
        };
        let span_ctx =
            self.start_mcp_span("tools/list", None, request_attributes(&context), meta);

        let tools = self.tools.values().map(|(t, _, _)| t.clone()).collect();
        let result = ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        };

        if let Some((mut span, _)) = span_ctx {
            span.end();
        }

        Ok(result)
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                rmcp::model::Implementation::new("modulewise-toolbelt", env!("CARGO_PKG_VERSION"))
                    .with_title("Modulewise Toolbelt")
                    .with_website_url("https://github.com/modulewise/toolbelt"),
            )
            .with_instructions(format!(
                "This server provides {} tools. \
                Each tool has typed inputs and outputs described by its schema. \
                Call tools with their required parameters.",
                self.tools.len()
            ))
    }
}

pub fn build_tracer_provider(
    endpoint: &str,
    protocol: &str,
    service_name: &str,
) -> Result<SdkTracerProvider> {
    let exporter = match protocol {
        "http/protobuf" => SpanExporter::builder()
            .with_http()
            .with_endpoint(endpoint)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build span exporter: {e}"))?,
        _ => {
            if protocol != "grpc" {
                tracing::warn!(protocol, "unrecognized OTLP protocol, defaulting to grpc");
            }
            SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .build()
                .map_err(|e| anyhow::anyhow!("failed to build span exporter: {e}"))?
        }
    };
    let resource = opentelemetry_sdk::Resource::builder()
        .with_attribute(opentelemetry::KeyValue::new(
            "service.name",
            service_name.to_string(),
        ))
        .build();
    let processor = BatchSpanProcessor::builder(exporter).build();
    Ok(SdkTracerProvider::builder()
        .with_resource(resource)
        .with_span_processor(processor)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapper::McpMapper;
    use composable_runtime::Runtime;
    use rmcp::model::ClientInfo;
    use rmcp::{ClientHandler, ServiceExt};
    use std::io::Write as _;
    use tempfile::Builder;

    macro_rules! args {
        ($($json:tt)+) => {
            serde_json::json!($($json)+).as_object().unwrap().clone()
        };
    }

    fn create_wasm(wat_content: &str) -> tempfile::NamedTempFile {
        let bytes = wat::parse_str(wat_content).unwrap();
        let mut f = Builder::new().suffix(".wasm").tempfile().unwrap();
        f.write_all(&bytes).unwrap();
        f
    }

    fn add_two_wat() -> &'static str {
        r#"
        (component
            (core module $m
                (func $add_two (param i32) (result i32)
                    local.get 0
                    i32.const 2
                    i32.add
                )
                (export "add-two" (func $add_two))
            )
            (core instance $i (instantiate $m))
            (func $f (param "x" s32) (result s32) (canon lift (core func $i "add-two")))
            (export "add-two" (func $f))
        )
        "#
    }

    // Build an McpServer from a Runtime by auto-discovering all components.
    fn build_test_server(runtime: &Runtime) -> McpServer {
        let invoker = runtime.invoker();
        let mut tools = HashMap::new();

        for component in runtime.list_components(None) {
            for function in component.functions.values() {
                let tool_name = format!("{}.{}", component.metadata.name, function.key());
                let tool = McpMapper::function_to_tool(function, &tool_name, None);
                tools.insert(
                    tool_name,
                    (tool, function.clone(), component.metadata.name.clone()),
                );
            }
        }

        let dummy_addr = "127.0.0.1:0".parse().unwrap();
        McpServer::new(tools, invoker, dummy_addr, OriginPolicy::AllowAll, None)
    }

    #[derive(Debug, Clone, Default)]
    struct TestClientHandler;

    impl ClientHandler for TestClientHandler {
        fn get_info(&self) -> ClientInfo {
            ClientInfo::default()
        }
    }

    struct TestClient {
        client: Option<rmcp::service::RunningService<rmcp::RoleClient, TestClientHandler>>,
        server_handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    }

    impl std::ops::Deref for TestClient {
        type Target = rmcp::service::RunningService<rmcp::RoleClient, TestClientHandler>;
        fn deref(&self) -> &Self::Target {
            self.client.as_ref().unwrap()
        }
    }

    impl Drop for TestClient {
        fn drop(&mut self) {
            if let Some(client) = &self.client {
                client.cancellation_token().cancel();
            }
            if let Some(handle) = self.server_handle.take() {
                handle.abort();
            }
        }
    }

    async fn setup_test_client(server_handler: McpServer) -> TestClient {
        let (server_transport, client_transport) = tokio::io::duplex(4096);

        let server_handle = tokio::spawn(async move {
            server_handler
                .serve(server_transport)
                .await?
                .waiting()
                .await?;
            anyhow::Ok(())
        });

        let client = TestClientHandler::default()
            .serve(client_transport)
            .await
            .unwrap();

        TestClient {
            client: Some(client),
            server_handle: Some(server_handle),
        }
    }

    async fn build_runtime(wasm_path: &std::path::Path) -> Runtime {
        Runtime::builder()
            .from_path(wasm_path.to_path_buf())
            .build()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_tool_invocation() {
        let wasm = create_wasm(add_two_wat());
        let runtime = build_runtime(wasm.path()).await;
        let client = setup_test_client(build_test_server(&runtime)).await;

        let tools_result = client.list_tools(None).await.unwrap();
        assert_eq!(tools_result.tools.len(), 1);

        let tool = &tools_result.tools[0];
        assert!(
            tool.name.ends_with(".add-two"),
            "Tool name should end with .add-two, got: {}",
            tool.name
        );

        let input_schema = &tool.input_schema;
        assert_eq!(input_schema.get("type").unwrap(), "object");

        let properties = input_schema.get("properties").unwrap().as_object().unwrap();
        assert!(properties.contains_key("x"));

        let required = input_schema.get("required").unwrap().as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "x");

        let request = CallToolRequestParams::new(tool.name.clone()).with_arguments(args!({"x": 5}));
        let result = client.call_tool(request).await.unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let result_value: i32 = result.content[0]
            .as_text()
            .unwrap()
            .text
            .trim()
            .parse()
            .unwrap();
        assert_eq!(result_value, 7);
    }

    #[tokio::test]
    async fn test_missing_required_parameter() {
        let wasm = create_wasm(add_two_wat());
        let runtime = build_runtime(wasm.path()).await;
        let client = setup_test_client(build_test_server(&runtime)).await;

        let tools_result = client.list_tools(None).await.unwrap();
        let tool = &tools_result.tools[0];

        let request = CallToolRequestParams::new(tool.name.clone()).with_arguments(args!({}));
        let result = client.call_tool(request).await.unwrap();
        assert!(result.is_error.unwrap_or(false));

        let text = result.content[0].as_text().unwrap().text.as_str();
        assert!(text.contains("Missing required parameter"));
        assert!(text.contains("x"));
    }

    #[tokio::test]
    async fn test_tool_not_found() {
        let wasm = create_wasm(add_two_wat());
        let runtime = build_runtime(wasm.path()).await;
        let client = setup_test_client(build_test_server(&runtime)).await;

        let request = CallToolRequestParams::new("nonexistent-tool");
        let result = client.call_tool(request).await.unwrap();
        assert!(result.is_error.unwrap_or(false));

        let text = result.content[0].as_text().unwrap().text.as_str();
        assert!(text.contains("Tool not found"));
        assert!(text.contains("nonexistent-tool"));
    }
}
