use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use composable_runtime::{
    CategoryClaim, Condition, ConfigHandler, Operator, PropertyMap, Selector,
};

// Default component selector for auto-discovery: top-level components only.
const DEFAULT_COMPONENT_SELECTOR: &str = "!dependents";

/// Parsed tool within an MCP server.
#[derive(Debug, Clone)]
pub struct ToolConfig {
    pub name: String,
    pub component: String,
    pub function: String,
    pub description: Option<String>,
}

/// Parsed MCP server definition.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub allowed_origins: Option<Vec<String>>,
    pub component_selector: Option<Selector>,
    pub tools: Vec<ToolConfig>,
    pub otlp_endpoint: Option<String>,
    pub otlp_protocol: String,
}

pub type SharedConfig = Arc<Mutex<Vec<McpServerConfig>>>;

pub fn shared_config() -> SharedConfig {
    Arc::new(Mutex::new(Vec::new()))
}

/// Create a default server config for auto-discovery of top-level components.
pub fn default_server() -> McpServerConfig {
    McpServerConfig {
        name: "mcp".to_string(),
        host: "127.0.0.1".to_string(),
        port: 3001,
        allowed_origins: None,
        component_selector: Some(
            Selector::parse(DEFAULT_COMPONENT_SELECTOR)
                .expect("default component selector is valid"),
        ),
        tools: Vec::new(),
        otlp_endpoint: None,
        otlp_protocol: "grpc".to_string(),
    }
}

/// Claims `[server.*]` definitions where `type = "mcp"`.
pub struct McpServerConfigHandler {
    servers: SharedConfig,
}

impl McpServerConfigHandler {
    pub fn new(servers: SharedConfig) -> Self {
        Self { servers }
    }
}

impl ConfigHandler for McpServerConfigHandler {
    fn claimed_categories(&self) -> Vec<CategoryClaim> {
        vec![CategoryClaim::with_selector(
            "server",
            Selector {
                conditions: vec![Condition {
                    key: "type".to_string(),
                    operator: Operator::Equals("mcp".to_string()),
                }],
            },
        )]
    }

    fn claimed_properties(&self) -> HashMap<&str, &[&str]> {
        HashMap::from([(
            "server",
            [
                "type",
                "host",
                "port",
                "allowed-origins",
                "component-selector",
                "otlp-endpoint",
                "otlp-protocol",
                "tool",
            ]
            .as_slice(),
        )])
    }

    fn handle_category(
        &mut self,
        category: &str,
        name: &str,
        mut properties: PropertyMap,
    ) -> Result<()> {
        if category != "server" {
            return Err(anyhow::anyhow!(
                "McpServerConfigHandler received unexpected category '{category}'"
            ));
        }

        // type is only used by the selector
        properties.remove("type");

        let port = match properties.remove("port") {
            Some(serde_json::Value::Number(n)) => n
                .as_u64()
                .and_then(|p| u16::try_from(p).ok())
                .ok_or_else(|| {
                    anyhow::anyhow!("Server '{name}': 'port' must be a valid port number")
                })?,
            Some(got) => {
                return Err(anyhow::anyhow!(
                    "Server '{name}': 'port' must be a number, got {got}"
                ));
            }
            None => {
                return Err(anyhow::anyhow!(
                    "Server '{name}' missing required 'port' field"
                ));
            }
        };

        let host = match properties.remove("host") {
            Some(serde_json::Value::String(s)) => s,
            Some(got) => {
                return Err(anyhow::anyhow!(
                    "Server '{name}': 'host' must be a string, got {got}"
                ));
            }
            None => "127.0.0.1".to_string(),
        };

        let allowed_origins = match properties.remove("allowed-origins") {
            Some(serde_json::Value::Array(arr)) => {
                let mut origins = Vec::new();
                for item in arr {
                    match item {
                        serde_json::Value::String(s) => origins.push(s),
                        got => {
                            return Err(anyhow::anyhow!(
                                "Server '{name}': 'allowed-origins' items must be strings, got {got}"
                            ));
                        }
                    }
                }
                Some(origins)
            }
            Some(serde_json::Value::String(s)) if s == "*" => Some(vec!["*".to_string()]),
            Some(got) => {
                return Err(anyhow::anyhow!(
                    "Server '{name}': 'allowed-origins' must be an array or '*', got {got}"
                ));
            }
            None => None,
        };

        let component_selector = match properties.remove("component-selector") {
            Some(serde_json::Value::String(s)) => Some(Selector::parse(&s).map_err(|e| {
                anyhow::anyhow!("Server '{name}': invalid component-selector '{s}': {e}")
            })?),
            Some(got) => {
                return Err(anyhow::anyhow!(
                    "Server '{name}': 'component-selector' must be a string, got {got}"
                ));
            }
            None => None,
        };

        let otlp_endpoint = match properties.remove("otlp-endpoint") {
            Some(serde_json::Value::String(s)) => Some(s),
            Some(got) => {
                return Err(anyhow::anyhow!(
                    "Server '{name}': 'otlp-endpoint' must be a string, got {got}"
                ));
            }
            None => None,
        };

        let otlp_protocol = match properties.remove("otlp-protocol") {
            Some(serde_json::Value::String(s)) => s,
            Some(got) => {
                return Err(anyhow::anyhow!(
                    "Server '{name}': 'otlp-protocol' must be a string, got {got}"
                ));
            }
            None => "grpc".to_string(),
        };

        let tools = parse_tools(name, &mut properties)?;

        if component_selector.is_none() && tools.is_empty() {
            return Err(anyhow::anyhow!(
                "Server '{name}' has no tools and no component-selector. \
                 At least one must be specified."
            ));
        }

        if !properties.is_empty() {
            let unknown: Vec<_> = properties.keys().collect();
            return Err(anyhow::anyhow!(
                "Server '{name}' has unknown properties: {unknown:?}"
            ));
        }

        self.servers.lock().unwrap().push(McpServerConfig {
            name: name.to_string(),
            host,
            port,
            allowed_origins,
            component_selector,
            tools,
            otlp_endpoint,
            otlp_protocol,
        });
        Ok(())
    }
}

fn parse_tools(server_name: &str, properties: &mut PropertyMap) -> Result<Vec<ToolConfig>> {
    let tool_table = match properties.remove("tool") {
        Some(serde_json::Value::Object(map)) => map,
        Some(got) => {
            return Err(anyhow::anyhow!(
                "Server '{server_name}': 'tool' must be a table, got {got}"
            ));
        }
        None => return Ok(Vec::new()),
    };

    let mut tools = Vec::new();
    for (tool_name, tool_value) in tool_table {
        let mut tool_props = match tool_value {
            serde_json::Value::Object(map) => map,
            got => {
                return Err(anyhow::anyhow!(
                    "Server '{server_name}': tool '{tool_name}' must be a table, got {got}"
                ));
            }
        };

        let component = match tool_props.remove("component") {
            Some(serde_json::Value::String(s)) => s,
            Some(got) => {
                return Err(anyhow::anyhow!(
                    "Server '{server_name}': tool '{tool_name}' 'component' must be a string, got {got}"
                ));
            }
            None => {
                return Err(anyhow::anyhow!(
                    "Server '{server_name}': tool '{tool_name}' missing required 'component' field"
                ));
            }
        };

        let function = match tool_props.remove("function") {
            Some(serde_json::Value::String(s)) => s,
            Some(got) => {
                return Err(anyhow::anyhow!(
                    "Server '{server_name}': tool '{tool_name}' 'function' must be a string, got {got}"
                ));
            }
            None => {
                return Err(anyhow::anyhow!(
                    "Server '{server_name}': tool '{tool_name}' missing required 'function' field"
                ));
            }
        };

        let description = match tool_props.remove("description") {
            Some(serde_json::Value::String(s)) => Some(s),
            Some(got) => {
                return Err(anyhow::anyhow!(
                    "Server '{server_name}': tool '{tool_name}' 'description' must be a string, got {got}"
                ));
            }
            None => None,
        };

        if !tool_props.is_empty() {
            let unknown: Vec<_> = tool_props.keys().collect();
            return Err(anyhow::anyhow!(
                "Server '{server_name}': tool '{tool_name}' has unknown properties: {unknown:?}"
            ));
        }

        tools.push(ToolConfig {
            name: tool_name,
            component,
            function,
            description,
        });
    }

    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_handler() -> (McpServerConfigHandler, SharedConfig) {
        let config = shared_config();
        let handler = McpServerConfigHandler::new(Arc::clone(&config));
        (handler, config)
    }

    fn props(pairs: Vec<(&str, serde_json::Value)>) -> PropertyMap {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn parse_basic_server() {
        let (mut handler, config) = make_handler();
        let properties = props(vec![
            ("type", serde_json::json!("mcp")),
            ("port", serde_json::json!(3001)),
            (
                "tool",
                serde_json::json!({
                    "add-two": {
                        "component": "math",
                        "function": "add-two"
                    }
                }),
            ),
        ]);

        handler
            .handle_category("server", "mcp", properties)
            .unwrap();

        let servers = config.lock().unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "mcp");
        assert_eq!(servers[0].host, "127.0.0.1");
        assert_eq!(servers[0].port, 3001);
        assert!(servers[0].allowed_origins.is_none());
        assert_eq!(servers[0].tools.len(), 1);
        assert_eq!(servers[0].tools[0].name, "add-two");
        assert_eq!(servers[0].tools[0].component, "math");
        assert_eq!(servers[0].tools[0].function, "add-two");
        assert!(servers[0].tools[0].description.is_none());
    }

    #[test]
    fn parse_server_with_all_options() {
        let (mut handler, config) = make_handler();
        let properties = props(vec![
            ("type", serde_json::json!("mcp")),
            ("host", serde_json::json!("0.0.0.0")),
            ("port", serde_json::json!(8080)),
            (
                "allowed-origins",
                serde_json::json!(["example.com", "localhost"]),
            ),
            (
                "tool",
                serde_json::json!({
                    "greet": {
                        "component": "greeter",
                        "function": "greet",
                        "description": "Greet someone by name"
                    }
                }),
            ),
        ]);

        handler
            .handle_category("server", "api", properties)
            .unwrap();

        let servers = config.lock().unwrap();
        assert_eq!(servers[0].host, "0.0.0.0");
        assert_eq!(servers[0].port, 8080);
        assert_eq!(
            servers[0].allowed_origins.as_deref(),
            Some(["example.com".to_string(), "localhost".to_string()].as_slice())
        );
        assert_eq!(
            servers[0].tools[0].description.as_deref(),
            Some("Greet someone by name")
        );
    }

    #[test]
    fn missing_port() {
        let (mut handler, _) = make_handler();
        let properties = props(vec![("type", serde_json::json!("mcp"))]);

        let result = handler.handle_category("server", "mcp", properties);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing required 'port'")
        );
    }

    #[test]
    fn missing_tool_component() {
        let (mut handler, _) = make_handler();
        let properties = props(vec![
            ("type", serde_json::json!("mcp")),
            ("port", serde_json::json!(3001)),
            (
                "tool",
                serde_json::json!({
                    "bad": {
                        "function": "do-stuff"
                    }
                }),
            ),
        ]);

        let result = handler.handle_category("server", "mcp", properties);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing required 'component'")
        );
    }

    #[test]
    fn missing_tool_function() {
        let (mut handler, _) = make_handler();
        let properties = props(vec![
            ("type", serde_json::json!("mcp")),
            ("port", serde_json::json!(3001)),
            (
                "tool",
                serde_json::json!({
                    "bad": {
                        "component": "math"
                    }
                }),
            ),
        ]);

        let result = handler.handle_category("server", "mcp", properties);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing required 'function'")
        );
    }

    #[test]
    fn no_tools_no_selector_errors() {
        let (mut handler, _) = make_handler();
        let properties = props(vec![
            ("type", serde_json::json!("mcp")),
            ("port", serde_json::json!(3001)),
        ]);

        let result = handler.handle_category("server", "mcp", properties);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no tools and no component-selector")
        );
    }

    #[test]
    fn selector_only_valid() {
        let (mut handler, config) = make_handler();
        let properties = props(vec![
            ("type", serde_json::json!("mcp")),
            ("port", serde_json::json!(3001)),
            ("component-selector", serde_json::json!("!dependents")),
        ]);

        handler
            .handle_category("server", "mcp", properties)
            .unwrap();

        let servers = config.lock().unwrap();
        assert!(servers[0].component_selector.is_some());
        assert!(servers[0].tools.is_empty());
    }

    #[test]
    fn selector_matches_mcp_type() {
        let handler = McpServerConfigHandler::new(shared_config());
        let claims = handler.claimed_categories();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].category, "server");
        assert!(claims[0].selector.is_some());

        let selector = claims[0].selector.as_ref().unwrap();
        let mut matching = HashMap::new();
        matching.insert("type".to_string(), Some("mcp".to_string()));
        assert!(selector.matches(&matching));

        let mut non_matching = HashMap::new();
        non_matching.insert("type".to_string(), Some("http".to_string()));
        assert!(!selector.matches(&non_matching));
    }

    #[test]
    fn unknown_tool_property() {
        let (mut handler, _) = make_handler();
        let properties = props(vec![
            ("type", serde_json::json!("mcp")),
            ("port", serde_json::json!(3001)),
            (
                "tool",
                serde_json::json!({
                    "bad": {
                        "component": "math",
                        "function": "add-two",
                        "bogus": "value"
                    }
                }),
            ),
        ]);

        let result = handler.handle_category("server", "mcp", properties);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unknown properties")
        );
    }

    #[test]
    fn wildcard_allowed_origins() {
        let (mut handler, config) = make_handler();
        let properties = props(vec![
            ("type", serde_json::json!("mcp")),
            ("port", serde_json::json!(3001)),
            ("allowed-origins", serde_json::json!("*")),
            ("component-selector", serde_json::json!("!dependents")),
        ]);

        handler
            .handle_category("server", "mcp", properties)
            .unwrap();

        let servers = config.lock().unwrap();
        assert_eq!(
            servers[0].allowed_origins.as_deref(),
            Some(["*".to_string()].as_slice())
        );
    }

    #[test]
    fn selector_and_tools_coexist() {
        let (mut handler, config) = make_handler();
        let properties = props(vec![
            ("type", serde_json::json!("mcp")),
            ("port", serde_json::json!(3001)),
            ("component-selector", serde_json::json!("labels.domain=api")),
            (
                "tool",
                serde_json::json!({
                    "custom-tool": {
                        "component": "math",
                        "function": "add-two",
                        "description": "Custom description"
                    }
                }),
            ),
        ]);

        handler
            .handle_category("server", "mcp", properties)
            .unwrap();

        let servers = config.lock().unwrap();
        assert!(servers[0].component_selector.is_some());
        assert_eq!(servers[0].tools.len(), 1);
    }
}
