wit_bindgen::generate!({
    path: "../wit",
    world: "mcp-client-otel-interceptor",
    generate_all
});

use composable::mcp::client as target;
use composable::mcp::types::{
    CallToolRequest, CallToolResult, InitializeRequest, InitializeResult, ListToolsRequest,
    ListToolsResult,
};
use wasi::clocks::wall_clock;
use wasi::otel::{tracing, types};

struct McpOtelInterceptor;

const SCOPE_NAME: &str = "modulewise.composable.mcp.client";
const SCOPE_VERSION: &str = "0.1.0";

fn kv(key: &str, value: &str) -> tracing::KeyValue {
    tracing::KeyValue {
        key: key.to_string(),
        value: value.to_string(),
    }
}

fn scope() -> types::InstrumentationScope {
    types::InstrumentationScope {
        name: SCOPE_NAME.to_string(),
        version: Some(SCOPE_VERSION.to_string()),
        schema_url: None,
        attributes: vec![],
    }
}

fn new_span_id() -> String {
    wasi::random::random::get_random_bytes(8)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn new_trace_id() -> String {
    wasi::random::random::get_random_bytes(16)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// Returns true if the SpanContext is the "empty" context (all-zero trace and
// span IDs), which the host returns when there's no propagated outer trace.
fn is_empty_context(ctx: &tracing::SpanContext) -> bool {
    ctx.trace_id.chars().all(|c| c == '0') && ctx.span_id.chars().all(|c| c == '0')
}

// Parse server.address and server.port from an MCP server URL.
// Falls back to the raw URL as address with no port if parsing fails.
fn parse_server_address(server_url: &str) -> (String, Option<u16>) {
    match url::Url::parse(server_url) {
        Ok(u) => (u.host_str().unwrap_or(server_url).to_string(), u.port()),
        Err(_) => (server_url.to_string(), None),
    }
}

// Wraps an MCP call with a client-kind span. The `do_call` closure receives
// the W3C traceparent and optional serialized tracestate so it can inject
// them into the outgoing request's `_meta` before invoking the target.
// The `finalize` closure runs after the call returns. It may push
// result-derived attributes, and it returns the span Status.
//
// The `mcp.method.name`, `server.address`, and `server.port` attributes are
// added by the helper, and `initial_attributes` includes the input-derived
// attributes that the caller knows before making this call.
fn traced_call<R, DoCall, Finalize>(
    method_name: &str,
    span_name: String,
    server_url: &str,
    initial_attributes: Vec<tracing::KeyValue>,
    do_call: DoCall,
    finalize: Finalize,
) -> Result<R, String>
where
    DoCall: FnOnce(String, Option<String>) -> Result<R, String>,
    Finalize: FnOnce(&Result<R, String>, &mut Vec<tracing::KeyValue>) -> tracing::Status,
{
    let outer = tracing::outer_span_context();
    let start = wall_clock::now();

    // If there's no outer context, start a fresh trace. Otherwise inherit
    // the trace identity, sampling flag, and tracestate from the parent.
    let (trace_id, parent_span_id, trace_flags, trace_state) = if is_empty_context(&outer) {
        (
            new_trace_id(),
            String::new(),
            tracing::TraceFlags::SAMPLED,
            vec![],
        )
    } else {
        (
            outer.trace_id.clone(),
            outer.span_id.clone(),
            outer.trace_flags,
            outer.trace_state.clone(),
        )
    };

    let span_context = tracing::SpanContext {
        trace_id,
        span_id: new_span_id(),
        trace_flags,
        is_remote: false,
        trace_state: trace_state.clone(),
    };
    tracing::on_start(&span_context);

    let flags_hex = if span_context
        .trace_flags
        .contains(tracing::TraceFlags::SAMPLED)
    {
        "01"
    } else {
        "00"
    };
    let traceparent = format!(
        "00-{}-{}-{}",
        span_context.trace_id, span_context.span_id, flags_hex
    );
    let tracestate = if trace_state.is_empty() {
        None
    } else {
        Some(
            trace_state
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(","),
        )
    };

    let result = do_call(traceparent, tracestate);

    let end = wall_clock::now();

    let (server_address, server_port) = parse_server_address(server_url);

    let mut attributes = initial_attributes;
    attributes.push(kv("mcp.method.name", method_name));
    attributes.push(kv("network.transport", "tcp"));
    attributes.push(kv("network.protocol.name", "http"));
    attributes.push(kv("server.address", &server_address));
    if let Some(port) = server_port {
        attributes.push(kv("server.port", &port.to_string()));
    }

    let status = finalize(&result, &mut attributes);

    tracing::on_end(&tracing::SpanData {
        span_context,
        parent_span_id,
        span_kind: tracing::SpanKind::Client,
        name: span_name,
        start_time: start,
        end_time: end,
        attributes,
        events: vec![],
        links: vec![],
        status,
        instrumentation_scope: scope(),
        dropped_attributes: 0,
        dropped_events: 0,
        dropped_links: 0,
    });

    result
}

// Inject traceparent and optional tracestate into a meta-entries list.
fn inject_trace_context(
    meta: &mut Vec<(String, String)>,
    traceparent: String,
    tracestate: Option<String>,
) {
    meta.push(("traceparent".to_string(), traceparent));
    if let Some(ts) = tracestate {
        meta.push(("tracestate".to_string(), ts));
    }
}

impl exports::composable::mcp::client::Guest for McpOtelInterceptor {
    fn initialize(
        server_url: String,
        request: Option<InitializeRequest>,
    ) -> Result<InitializeResult, String> {
        traced_call(
            "initialize",
            "initialize".to_string(),
            &server_url,
            vec![kv("jsonrpc.request.id", "1")],
            |traceparent, tracestate| {
                let mut req = request.unwrap_or(InitializeRequest {
                    protocol_version: None,
                    capabilities: None,
                    client_info: None,
                    meta: None,
                });
                let mut meta = req.meta.unwrap_or_default();
                inject_trace_context(&mut meta, traceparent, tracestate);
                req.meta = Some(meta);
                target::initialize(&server_url, Some(&req))
            },
            |result, attrs| match result {
                Ok(init_result) => {
                    attrs.push(kv("mcp.session.id", &init_result.session_id));
                    attrs.push(kv("mcp.protocol.version", &init_result.protocol_version));
                    tracing::Status::Ok
                }
                Err(err) => {
                    attrs.push(kv("error.type", "transport_error"));
                    tracing::Status::Error(err.clone())
                }
            },
        )
    }

    fn list_tools(
        server_url: String,
        session_id: String,
        request_id: i32,
        request: Option<ListToolsRequest>,
    ) -> Result<ListToolsResult, String> {
        let initial_attributes = vec![
            kv("jsonrpc.request.id", &request_id.to_string()),
            kv("mcp.session.id", &session_id),
        ];

        traced_call(
            "tools/list",
            "tools/list".to_string(),
            &server_url,
            initial_attributes,
            |traceparent, tracestate| {
                let mut req = request.unwrap_or(ListToolsRequest {
                    cursor: None,
                    meta: None,
                });
                let mut meta = req.meta.unwrap_or_default();
                inject_trace_context(&mut meta, traceparent, tracestate);
                req.meta = Some(meta);
                target::list_tools(&server_url, &session_id, request_id, Some(&req))
            },
            |result, attrs| match result {
                Ok(_) => tracing::Status::Ok,
                Err(err) => {
                    attrs.push(kv("error.type", "transport_error"));
                    tracing::Status::Error(err.clone())
                }
            },
        )
    }

    fn call_tool(
        server_url: String,
        session_id: String,
        request_id: i32,
        request: CallToolRequest,
    ) -> Result<CallToolResult, String> {
        let name = request.name.clone();
        let initial_attributes = vec![
            kv("gen_ai.operation.name", "execute_tool"),
            kv("gen_ai.tool.name", &name),
            kv("jsonrpc.request.id", &request_id.to_string()),
            kv("mcp.session.id", &session_id),
        ];

        traced_call(
            "tools/call",
            format!("tools/call {name}"),
            &server_url,
            initial_attributes,
            |traceparent, tracestate| {
                let mut meta = request.meta.unwrap_or_default();
                inject_trace_context(&mut meta, traceparent, tracestate);
                let enriched = CallToolRequest {
                    name: request.name,
                    arguments: request.arguments,
                    meta: Some(meta),
                };
                target::call_tool(&server_url, &session_id, request_id, &enriched)
            },
            |result, attrs| match result {
                Ok(tool_result) if tool_result.is_error => {
                    attrs.push(kv("error.type", "tool_error"));
                    tracing::Status::Error("tool returned error".to_string())
                }
                Ok(_) => tracing::Status::Ok,
                Err(err) => {
                    attrs.push(kv("error.type", "transport_error"));
                    tracing::Status::Error(err.clone())
                }
            },
        )
    }

    fn terminate(server_url: String, session_id: String) -> Result<(), String> {
        target::terminate(&server_url, &session_id)
    }
}

export!(McpOtelInterceptor);
