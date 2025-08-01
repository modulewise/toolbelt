use anyhow::Result;
use std::collections::HashMap;
use wac_graph::{CompositionGraph, EncodeOptions};
use wac_types::Package;

pub struct Composer;

impl Composer {
    /// Compose a component with a wasi:config/store generated from the provided config
    pub fn compose_with_config(
        component_bytes: &[u8],
        config: &HashMap<String, serde_json::Value>,
    ) -> Result<Vec<u8>> {
        // Note: empty config will create an empty wasi:config/store component
        let config_component_bytes = create_config_component(config)?;
        Self::compose_components(component_bytes, &config_component_bytes)
    }

    /// Compose two components: plug_bytes gets plugged into socket_bytes
    pub fn compose_components(socket_bytes: &[u8], plug_bytes: &[u8]) -> Result<Vec<u8>> {
        let mut graph = CompositionGraph::new();

        let socket_package = Package::from_bytes("socket", None, socket_bytes, graph.types_mut())?;
        let plug_package = Package::from_bytes("plug", None, plug_bytes, graph.types_mut())?;

        let socket_package_id = graph.register_package(socket_package)?;
        let plug_package_id = graph.register_package(plug_package)?;

        wac_graph::plug(&mut graph, vec![plug_package_id], socket_package_id)?;

        let encode_options = EncodeOptions {
            define_components: true,
            ..Default::default()
        };

        let composed_bytes = graph
            .encode(encode_options)
            .map_err(|e| anyhow::anyhow!("Failed to encode composition: {}", e))?;

        Ok(composed_bytes)
    }
}

// Generate a wasi:config/store component from key/value configuration
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
