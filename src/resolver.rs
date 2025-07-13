use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::components::{Capability, ComponentSpec};
use crate::composer::Composer;

type ToolsAndConfigs = (
    HashMap<String, ComponentConfig>,
    HashMap<String, HashMap<String, serde_json::Value>>,
);

#[derive(Debug, Deserialize, Serialize)]
struct ComponentConfig {
    uri: String,
    #[serde(default)]
    capabilities: Vec<Capability>,
}

pub fn resolve_components(input_paths: &[PathBuf]) -> Result<Vec<ComponentSpec>> {
    let mut component_specs = Vec::new();
    for path in input_paths {
        if path.is_file() {
            if let Some(extension) = path.extension().and_then(|s| s.to_str()) {
                match extension {
                    "wasm" => {
                        let spec = resolve_wasm_file(path)?;
                        component_specs.push(spec);
                    }
                    "toml" => {
                        let specs = resolve_toml_file(path)?;
                        component_specs.extend(specs);
                    }
                    _ => {
                        return Err(anyhow::anyhow!("Unsupported file type: {}", path.display()));
                    }
                }
            }
        } else if path.is_dir() {
            let dir_specs = resolve_directory(path)?;
            component_specs.extend(dir_specs);
        } else {
            return Err(anyhow::anyhow!("Path does not exist: {}", path.display()));
        }
    }
    Ok(component_specs)
}

fn resolve_wasm_file(path: &PathBuf) -> Result<ComponentSpec> {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cannot extract component name from path: {}",
                path.display()
            )
        })?
        .to_string();
    let bytes = fs::read(path)?;
    Ok(ComponentSpec {
        name,
        bytes,
        capabilities: Vec::new(),
    })
}

fn resolve_toml_file(path: &PathBuf) -> Result<Vec<ComponentSpec>> {
    let content = fs::read_to_string(path)?;
    let toml_doc: toml::Value = toml::from_str(&content)?;

    let (tools, configs) = extract_tools_and_configs(&toml_doc)?;

    let mut specs = Vec::new();
    for (name, component_config) in tools {
        let component_path = resolve_uri(&component_config.uri)?;
        let mut bytes = fs::read(&component_path)?;
        let config = configs.get(&name).cloned().unwrap_or_default();
        //println!("Tool '{name}' has config: {config:?}");

        // Compose if config exists (even if empty) - empty config satisfies imports with defaults
        if configs.contains_key(&name) {
            println!(
                "Composing {} with config: {:?}",
                name,
                config.keys().collect::<Vec<_>>()
            );
            bytes = Composer::compose_tool_with_config(&bytes, &config)
                .map_err(|e| anyhow::anyhow!("Failed to compose {} with config: {}", name, e))?;
            //println!("✓ Composed {name} successfully");
        }

        specs.push(ComponentSpec {
            name,
            bytes,
            capabilities: component_config.capabilities,
        });
    }
    Ok(specs)
}

fn extract_tools_and_configs(toml_doc: &toml::Value) -> Result<ToolsAndConfigs> {
    let mut tools = HashMap::new();
    let mut configs = HashMap::new();

    if let toml::Value::Table(table) = toml_doc {
        for (key, value) in table {
            if let toml::Value::Table(tool_table) = value {
                // Check if this tool has a "config" subtable
                if let Some(toml::Value::Table(config_table)) = tool_table.get("config") {
                    let config_map = convert_toml_table_to_json_map(config_table)?;
                    configs.insert(key.clone(), config_map);
                }

                // Parse the tool definition (excluding the config subtable)
                let mut tool_value = tool_table.clone();
                tool_value.remove("config"); // Remove config before parsing as ComponentConfig
                let component_config: ComponentConfig =
                    toml::Value::Table(tool_value)
                        .try_into()
                        .map_err(|e| anyhow::anyhow!("Failed to parse tool '{}': {}", key, e))?;
                tools.insert(key.clone(), component_config);
            }
        }
    }
    Ok((tools, configs))
}

fn convert_toml_table_to_json_map(
    table: &toml::map::Map<String, toml::Value>,
) -> Result<HashMap<String, serde_json::Value>> {
    let mut map = HashMap::new();
    for (key, value) in table {
        let json_value = convert_toml_value_to_json(value)?;
        map.insert(key.clone(), json_value);
    }
    Ok(map)
}

fn convert_toml_value_to_json(value: &toml::Value) -> Result<serde_json::Value> {
    match value {
        toml::Value::String(s) => Ok(serde_json::Value::String(s.clone())),
        toml::Value::Integer(i) => Ok(serde_json::Value::Number((*i).into())),
        toml::Value::Float(f) => Ok(serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null)),
        toml::Value::Boolean(b) => Ok(serde_json::Value::Bool(*b)),
        toml::Value::Array(arr) => {
            let json_arr: Result<Vec<_>, _> = arr.iter().map(convert_toml_value_to_json).collect();
            Ok(serde_json::Value::Array(json_arr?))
        }
        toml::Value::Table(table) => {
            let json_map = convert_toml_table_to_json_map(table)?;
            let json_obj: serde_json::Map<String, serde_json::Value> =
                json_map.into_iter().collect();
            Ok(serde_json::Value::Object(json_obj))
        }
        toml::Value::Datetime(dt) => Ok(serde_json::Value::String(dt.to_string())),
    }
}

fn resolve_directory(dir_path: &PathBuf) -> Result<Vec<ComponentSpec>> {
    let mut specs = Vec::new();
    for entry in fs::read_dir(dir_path)? {
        let entry_path = entry?.path();
        if entry_path.is_file() && entry_path.extension().and_then(|s| s.to_str()) == Some("wasm") {
            let spec = resolve_wasm_file(&entry_path)?;
            specs.push(spec);
        }
    }
    Ok(specs)
}

fn resolve_uri(uri: &str) -> Result<PathBuf> {
    if let Some(path_str) = uri.strip_prefix("file://") {
        Ok(PathBuf::from(path_str))
    } else {
        Ok(PathBuf::from(uri))
    }
}
