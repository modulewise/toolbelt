use anyhow::Result;
use std::collections::HashMap;
use wac_graph::{CompositionGraph, EncodeOptions};
use wac_types::Package;

pub struct Composer;

impl Composer {
    pub fn compose_tool_with_config(
        tool_bytes: &[u8],
        config: &HashMap<String, serde_json::Value>,
    ) -> Result<Vec<u8>> {
        // Note: config can be empty - this creates an empty config component
        // that satisfies wasi:config/store imports but provides no values (uses defaults)

        let config_component_bytes = create_config_component(config)?;
        // println!(
        //     "Generated config component: {} bytes",
        //     config_component_bytes.len()
        // );

        let mut graph = CompositionGraph::new();

        let tool_package = Package::from_bytes("tool", None, tool_bytes, graph.types_mut())?;
        let config_package =
            Package::from_bytes("config", None, config_component_bytes, graph.types_mut())?;

        let tool_package_id = graph.register_package(tool_package)?;
        let config_package_id = graph.register_package(config_package)?;

        // compose config component (plug) into tool component (socket)
        wac_graph::plug(&mut graph, vec![config_package_id], tool_package_id)?;

        let encode_options = EncodeOptions {
            define_components: true,
            ..Default::default()
        };

        let composed_bytes = graph
            .encode(encode_options)
            .map_err(|e| anyhow::anyhow!("Failed to encode composition: {}", e))?;

        //println!("Composition: {} bytes", composed_bytes.len());
        Ok(composed_bytes)
    }
}

/// Generate a wasi:config/store component from key/value configuration
fn create_config_component(config: &HashMap<String, serde_json::Value>) -> Result<Vec<u8>> {
    let mut config_properties = Vec::new();
    for (key, value) in config {
        let string_value = convert_json_value_to_string(value)?;
        config_properties.push((key.clone(), string_value));
    }
    static_config::create_component(config_properties)
        .map_err(|e| anyhow::anyhow!("Failed to create config component: {}", e))
}

fn convert_json_value_to_string(value: &serde_json::Value) -> Result<String> {
    match value {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        serde_json::Value::Array(arr) => {
            // Convert array to comma-separated string
            let string_items: Result<Vec<String>, _> =
                arr.iter().map(convert_json_value_to_string).collect();
            Ok(string_items?.join(","))
        }
        serde_json::Value::Object(_) => {
            Err(anyhow::anyhow!("Nested objects not supported in config"))
        }
        serde_json::Value::Null => Err(anyhow::anyhow!("Null values not supported in config")),
    }
}
