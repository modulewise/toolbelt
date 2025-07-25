use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

pub type CapabilityName = String;

#[derive(Debug, Deserialize, Serialize)]
pub struct ComponentDefinition {
    pub uri: String,
    pub config: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityName>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CapabilityDefinition {
    pub name: String,
    #[serde(flatten)]
    pub base: ComponentDefinition,
    #[serde(default)]
    pub exposed: bool,
}

impl std::ops::Deref for CapabilityDefinition {
    type Target = ComponentDefinition;
    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(flatten)]
    pub base: ComponentDefinition,
}

impl std::ops::Deref for ToolDefinition {
    type Target = ComponentDefinition;
    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

/// Load capability and tool definitions from configuration files
///
/// Processes capability files (.toml), tool files (.toml), and mixed definition files
/// (.toml and .wasm) to extract component definitions.
pub fn load_definitions(
    capability_files: &[PathBuf],
    tool_files: &[PathBuf],
    mixed_definition_files: &[PathBuf], // .toml and .wasm files
) -> Result<(Vec<CapabilityDefinition>, Vec<ToolDefinition>)> {
    let mut definition_files = Vec::new();
    let mut wasm_files = Vec::new();

    for path in mixed_definition_files {
        let path_str = path.to_string_lossy();

        // Handle OCI URIs as wasm components
        if path_str.starts_with("oci://") {
            wasm_files.push(path.clone());
        } else if let Some(extension) = path.extension().and_then(|s| s.to_str()) {
            match extension {
                "wasm" => wasm_files.push(path.clone()),
                "toml" => definition_files.push(path.clone()),
                _ => return Err(anyhow::anyhow!("Unsupported file type: {}", path.display())),
            }
        } else {
            return Err(anyhow::anyhow!(
                "File without extension: {}",
                path.display()
            ));
        }
    }
    build_definitions(capability_files, tool_files, &definition_files, &wasm_files)
}

fn build_definitions(
    capability_files: &[PathBuf],
    tool_files: &[PathBuf],
    definition_files: &[PathBuf],
    wasm_files: &[PathBuf],
) -> Result<(Vec<CapabilityDefinition>, Vec<ToolDefinition>)> {
    let mut capability_definitions = Vec::new();
    let mut tool_definitions = Vec::new();

    for file in capability_files {
        capability_definitions.extend(parse_capability_file(file)?);
    }
    for file in tool_files {
        tool_definitions.extend(parse_tool_file(file)?);
    }
    for file in definition_files {
        let (caps, tools) = parse_definition_file(file)?;
        capability_definitions.extend(caps);
        tool_definitions.extend(tools);
    }
    tool_definitions.extend(create_implicit_tool_definitions(wasm_files)?);

    // Collision detection for capabilities
    let mut capability_names = HashSet::new();
    for def in &capability_definitions {
        if !capability_names.insert(&def.name) {
            return Err(anyhow::anyhow!("Duplicate capability name: '{}'", def.name));
        }
    }

    // Validate capability dependencies exist
    for def in &capability_definitions {
        for dep_name in &def.capabilities {
            if !capability_names.contains(dep_name) {
                return Err(anyhow::anyhow!(
                    "Capability '{}' depends on undefined capability '{}'",
                    def.name,
                    dep_name
                ));
            }
        }
    }

    // Collision detection for tools
    let mut tool_names = HashSet::new();
    for def in &tool_definitions {
        if !tool_names.insert(&def.name) {
            return Err(anyhow::anyhow!("Duplicate tool name: '{}'", def.name));
        }
    }
    for capability_name in &capability_names {
        if tool_names.contains(capability_name) {
            return Err(anyhow::anyhow!(
                "Name collision: '{}' is defined as both a capability and a tool",
                capability_name
            ));
        }
    }

    Ok((capability_definitions, tool_definitions))
}

fn parse_capability_file(path: &PathBuf) -> Result<Vec<CapabilityDefinition>> {
    let content = fs::read_to_string(path)?;
    let toml_doc: toml::Value = toml::from_str(&content)?;
    parse_capabilities_from_toml(&toml_doc)
}

fn parse_tool_file(path: &PathBuf) -> Result<Vec<ToolDefinition>> {
    let content = fs::read_to_string(path)?;
    let toml_doc: toml::Value = toml::from_str(&content)?;
    parse_tools_from_toml(&toml_doc)
}

fn parse_definition_file(
    path: &PathBuf,
) -> Result<(Vec<CapabilityDefinition>, Vec<ToolDefinition>)> {
    let content = fs::read_to_string(path)?;
    let toml_doc: toml::Value = toml::from_str(&content)?;

    let (caps_section, tools_section) = split_namespaced_toml(&toml_doc);

    let capabilities = if let Some(section) = caps_section {
        parse_capabilities_from_toml(section)?
    } else {
        Vec::new()
    };

    let tools = if let Some(section) = tools_section {
        parse_tools_from_toml(section)?
    } else {
        Vec::new()
    };

    // If no namespaced sections found, check if there are top-level sections that might be tools
    if capabilities.is_empty() && tools.is_empty() {
        if let toml::Value::Table(table) = &toml_doc {
            if !table.is_empty() {
                return Err(anyhow::anyhow!(
                    "No [capabilities.*] or [tools.*] sections found in '{}', but found top-level sections. Use the -t flag if it is a tool definition file.",
                    path.display()
                ));
            }
        }
    }

    Ok((capabilities, tools))
}

fn split_namespaced_toml(toml_doc: &toml::Value) -> (Option<&toml::Value>, Option<&toml::Value>) {
    if let toml::Value::Table(table) = toml_doc {
        let capabilities_section = table.get("capabilities");
        let tools_section = table.get("tools");
        (capabilities_section, tools_section)
    } else {
        (None, None)
    }
}

fn parse_capabilities_from_toml(
    capabilities_section: &toml::Value,
) -> Result<Vec<CapabilityDefinition>> {
    let mut definitions = Vec::new();
    if let toml::Value::Table(table) = capabilities_section {
        for (name, value) in table {
            if let toml::Value::Table(capability_table) = value {
                let definition = parse_capability_toml_table(name, capability_table)?;
                definitions.push(definition);
            }
        }
    }
    Ok(definitions)
}

fn parse_tools_from_toml(tools_section: &toml::Value) -> Result<Vec<ToolDefinition>> {
    let mut definitions = Vec::new();
    if let toml::Value::Table(table) = tools_section {
        for (name, value) in table {
            if let toml::Value::Table(tool_table) = value {
                let component = parse_component_toml_table(name, tool_table)?;
                let definition = ToolDefinition {
                    name: name.to_string(),
                    base: component,
                };
                definitions.push(definition);
            }
        }
    }
    Ok(definitions)
}

fn parse_capability_toml_table(
    name: &str,
    table: &toml::map::Map<String, toml::Value>,
) -> Result<CapabilityDefinition> {
    let component = parse_component_toml_table(name, table)?;
    let exposed = table
        .get("exposed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(CapabilityDefinition {
        name: name.to_string(),
        base: component,
        exposed,
    })
}

fn parse_component_toml_table(
    name: &str,
    table: &toml::map::Map<String, toml::Value>,
) -> Result<ComponentDefinition> {
    let mut definition_value = table.clone();
    let config = if let Some(toml::Value::Table(config_table)) = definition_value.remove("config") {
        Some(convert_toml_table_to_json_map(&config_table)?)
    } else {
        None
    };

    let mut component: ComponentDefinition = toml::Value::Table(definition_value)
        .try_into()
        .map_err(|e| anyhow::anyhow!("Failed to parse component '{}': {}", name, e))?;

    component.config = config;
    Ok(component)
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

fn create_implicit_tool_definitions(wasm_files: &[PathBuf]) -> Result<Vec<ToolDefinition>> {
    let mut definitions = Vec::new();
    for path in wasm_files {
        let path_str = path.to_string_lossy();
        let name = if path_str.starts_with("oci://") {
            // Extract component name from OCI URI: oci://ghcr.io/modulewise/hello:0.1.0 -> hello
            let oci_ref = path_str.strip_prefix("oci://").unwrap();
            if let Some((pkg_part, _version)) = oci_ref.rsplit_once(':') {
                if let Some(name_part) = pkg_part.rsplit_once('/') {
                    name_part.1.to_string()
                } else {
                    pkg_part.to_string()
                }
            } else {
                return Err(anyhow::anyhow!("Invalid OCI URI format: {}", path_str));
            }
        } else {
            path.file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Cannot extract component name from path: {}",
                        path.display()
                    )
                })?
                .to_string()
        };
        let definition = ToolDefinition {
            name,
            base: ComponentDefinition {
                uri: path.to_string_lossy().to_string(),
                config: None,
                capabilities: Vec::new(),
            },
        };
        definitions.push(definition);
    }
    Ok(definitions)
}
