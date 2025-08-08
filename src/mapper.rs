use anyhow::Result;
use rmcp::model::Tool;
use serde_json::json;
use std::collections::HashMap;

use composable_runtime::Function;

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
                output_schema: Self::create_output_schema(&function).map(|s| s.into()),
                annotations: None,
            };
            tools.push(tool);
        }
        Ok(tools)
    }

    fn create_output_schema(function: &Function) -> Option<rmcp::model::JsonObject> {
        function
            .result()
            .and_then(|schema| schema.as_object())
            .and_then(|obj| {
                if let Some(schema_type) = obj.get("type").and_then(|t| t.as_str()) {
                    match schema_type {
                        "object" => {
                            // WIT record -> use object schema directly
                            Some(obj.clone())
                        }
                        "array" => {
                            // WIT list -> wrap with semantic property name based on item type
                            let property_name = Self::derive_array_property_name(obj);
                            let wrapped = json!({
                                "type": "object",
                                "properties": {
                                    property_name.clone(): obj.clone()
                                },
                                "required": [property_name]
                            });
                            Some(wrapped.as_object().unwrap().clone())
                        }
                        _ => {
                            // WIT primitive -> unstructured
                            None
                        }
                    }
                } else if let Some(_option) = obj.get("oneOf").and_then(|a| a.as_array()) {
                    // WIT option<T> -> wrap in object with nullable property
                    let wrapped = json!({
                        "type": "object",
                        "properties": {
                            "result": obj.clone()
                        },
                        "additionalProperties": false
                    });
                    Some(wrapped.as_object().unwrap().clone())
                } else {
                    // Other schema types -> unstructured
                    None
                }
            })
    }

    fn derive_array_property_name(
        array_schema: &serde_json::Map<String, serde_json::Value>,
    ) -> String {
        // Extract the item type name from the array schema
        if let Some(items) = array_schema.get("items") {
            if let Some(items_obj) = items.as_object() {
                if let Some(title) = items_obj.get("title") {
                    if let Some(type_name) = title.as_str() {
                        return Self::pluralize(type_name);
                    }
                }
            }
        }
        // Fallback to generic name if no title found
        "items".to_string()
    }

    fn pluralize(singular: &str) -> String {
        if singular.ends_with("s")
            || singular.ends_with("x")
            || singular.ends_with("z")
            || singular.ends_with("ch")
            || singular.ends_with("sh")
        {
            format!("{singular}es")
        } else if singular.ends_with("y") && singular.len() > 1 {
            let chars: Vec<char> = singular.chars().collect();
            if let Some(penultimate) = chars.get(chars.len() - 2) {
                if !"aeiou".contains(*penultimate) {
                    return format!("{}ies", &singular[..singular.len() - 1]);
                }
            }
            format!("{singular}s")
        } else {
            format!("{singular}s")
        }
    }
}
