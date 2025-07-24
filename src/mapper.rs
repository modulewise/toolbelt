use anyhow::Result;
use rmcp::model::Tool;
use serde_json::json;
use std::collections::HashMap;

use crate::wit::Function;

/// Mapper that converts core types to MCP Tools
pub struct McpMapper;

impl McpMapper {
    /// Convert wit::Function objects to MCP Tool objects
    pub fn functions_to_tools(functions: Vec<Function>, component_name: &str) -> Result<Vec<Tool>> {
        let mut tools = Vec::new();

        // Check for function name conflicts to determine if disambiguation is needed
        let mut function_counts: HashMap<String, u32> = HashMap::new();
        for func in &functions {
            *function_counts
                .entry(func.function_name().to_string())
                .or_insert(0) += 1;
        }
        let requires_disambiguation = function_counts.values().any(|&count| count > 1);

        for function in functions {
            let tool_name = if requires_disambiguation {
                format!(
                    "{}_{}_{}",
                    component_name,
                    function.interface().interface_name(),
                    function.function_name()
                )
            } else {
                format!("{}_{}", component_name, function.function_name())
            };

            let description = if function.docs().is_empty() {
                format!(
                    "Call {} function from {} component",
                    function.function_name(),
                    component_name
                )
            } else {
                function.docs().to_string()
            };

            let mut properties = serde_json::Map::new();
            let mut required = Vec::new();

            for param in function.params() {
                let mut param_schema = param.json_schema.clone();
                if let serde_json::Value::Object(ref mut schema_obj) = param_schema {
                    schema_obj.insert(
                        "description".to_string(),
                        serde_json::Value::String(format!("Parameter: {}", param.name)),
                    );
                }
                properties.insert(param.name.clone(), param_schema);
                if !param.is_optional {
                    required.push(param.name.clone());
                }
            }

            let input_schema = json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false
            });

            let tool = Tool {
                name: tool_name.into(),
                description: Some(description.into()),
                input_schema: input_schema.as_object().unwrap().clone().into(),
                annotations: None,
            };
            tools.push(tool);
        }
        Ok(tools)
    }
}
