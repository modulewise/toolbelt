use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;

use crate::capabilities::{Capability, CapabilityRegistry, RuntimeCapability};
use crate::components::{CapabilityName, ComponentCapability, ComponentSpec};
use crate::composer::Composer;
use crate::interfaces::Parser;

pub type ToolRegistry = HashMap<String, ComponentSpec>;

struct CapabilityRegistryBuilder {
    runtime_capabilities: HashMap<String, RuntimeCapability>,
    component_capabilities: HashMap<String, ComponentCapability>,
    pending: VecDeque<CapabilityDefinition>,
    last_errors: HashMap<String, String>, // Track last error for each capability
}

impl CapabilityRegistryBuilder {
    fn new() -> Self {
        Self {
            runtime_capabilities: HashMap::new(),
            component_capabilities: HashMap::new(),
            pending: VecDeque::new(),
            last_errors: HashMap::new(),
        }
    }

    fn add_runtime_capability(&mut self, name: String, capability: RuntimeCapability) {
        self.runtime_capabilities.insert(name, capability);
    }

    fn add_pending_component_capability_definition(&mut self, definition: CapabilityDefinition) {
        self.pending.push_back(definition);
    }

    fn try_next(&mut self) -> Result<Option<bool>> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let definition = self.pending.pop_front().unwrap();

        // Create a temporary registry with current state for dependency checking
        let temp_registry = CapabilityRegistry::new(
            self.runtime_capabilities.clone(),
            self.component_capabilities.clone(),
        );

        match resolve_component_capability_from_definition(&definition, &temp_registry) {
            Ok(component_capability) => {
                self.component_capabilities
                    .insert(definition.name.clone(), component_capability);
                Ok(Some(true)) // Successfully resolved
            }
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("unavailable dependency")
                    || error_msg.contains("unauthorized interface")
                {
                    self.last_errors.insert(definition.name.clone(), error_msg);
                    self.pending.push_back(definition);
                    Ok(Some(false)) // Failed but retryable
                } else {
                    // Other errors are real failures
                    Err(anyhow::anyhow!(
                        "Failed to resolve capability '{}': {}",
                        definition.name,
                        e
                    ))
                }
            }
        }
    }

    fn build_registry(mut self) -> Result<CapabilityRegistry> {
        let mut attempts = 0;
        let max_attempts = self.pending.len() * self.pending.len(); // Prevent infinite loops

        let mut consecutive_failures = 0;
        while !self.pending.is_empty() && attempts < max_attempts {
            match self.try_next()? {
                Some(true) => {
                    // Successfully resolved a capability
                    consecutive_failures = 0;
                }
                Some(false) => {
                    // Failed but moved to back for retry
                    consecutive_failures += 1;
                    if consecutive_failures >= self.pending.len() {
                        let detailed_errors: Vec<String> = self
                            .pending
                            .iter()
                            .map(|definition| {
                                if let Some(error) = self.last_errors.get(&definition.name) {
                                    format!("'{}': {error}", definition.name)
                                } else {
                                    format!("'{}': unknown error", definition.name)
                                }
                            })
                            .collect();

                        return Err(anyhow::anyhow!(
                            "Cannot resolve capability dependencies:\n{}",
                            detailed_errors.join("\n")
                        ));
                    }
                }
                None => {
                    // Queue is empty
                    break;
                }
            }
            attempts += 1;
        }

        Ok(CapabilityRegistry::new(
            self.runtime_capabilities,
            self.component_capabilities,
        ))
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct CapabilityDefinition {
    name: String,
    uri: String,
    config: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    capabilities: Vec<CapabilityName>,
    exposed: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct ToolDefinition {
    name: String,
    uri: String,
    config: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    capabilities: Vec<CapabilityName>,
}

pub fn build_registries(
    capability_files: &[PathBuf],
    tool_files: &[PathBuf],
    mixed_definition_files: &[PathBuf], // .toml and .wasm files
) -> Result<(CapabilityRegistry, ToolRegistry)> {
    let mut definition_files = Vec::new();
    let mut wasm_files = Vec::new();

    for path in mixed_definition_files {
        if let Some(extension) = path.extension().and_then(|s| s.to_str()) {
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

    let (capability_definitions, tool_definitions) =
        build_definitions(capability_files, tool_files, &definition_files, &wasm_files)?;

    let capability_registry = create_capability_registry(capability_definitions)?;
    let tool_registry = create_tool_registry(tool_definitions, &capability_registry)?;

    Ok((capability_registry, tool_registry))
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
                let definition = parse_tool_toml_table(name, tool_table)?;
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
    let (name, uri, config, capabilities, exposed) = parse_component_toml_table(name, table)?;
    Ok(create_capability_definition(
        name,
        uri,
        config,
        capabilities,
        exposed,
    ))
}

fn parse_tool_toml_table(
    name: &str,
    table: &toml::map::Map<String, toml::Value>,
) -> Result<ToolDefinition> {
    let (name, uri, config, capabilities, exposed) = parse_component_toml_table(name, table)?;
    create_tool_definition(name, uri, config, capabilities, exposed)
}

fn parse_component_toml_table(
    name: &str,
    table: &toml::map::Map<String, toml::Value>,
) -> Result<(
    String,
    String,
    Option<HashMap<String, serde_json::Value>>,
    Vec<CapabilityName>,
    bool,
)> {
    let config = extract_config_from_table(table);

    let mut definition_value = table.clone();
    definition_value.remove("config");

    let capability: Capability = toml::Value::Table(definition_value)
        .try_into()
        .map_err(|e| anyhow::anyhow!("Failed to parse component '{}': {}", name, e))?;

    Ok((
        name.to_string(),
        capability.uri,
        config,
        capability.capabilities,
        capability.exposed,
    ))
}

fn extract_config_from_table(
    table: &toml::map::Map<String, toml::Value>,
) -> Option<HashMap<String, serde_json::Value>> {
    if let Some(toml::Value::Table(config_table)) = table.get("config") {
        Some(convert_toml_table_to_json_map(config_table).unwrap())
    } else {
        None
    }
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

fn create_capability_definition(
    name: String,
    uri: String,
    config: Option<HashMap<String, serde_json::Value>>,
    capabilities: Vec<CapabilityName>,
    exposed: bool,
) -> CapabilityDefinition {
    CapabilityDefinition {
        name,
        uri,
        config,
        capabilities,
        exposed,
    }
}

fn create_tool_definition(
    name: String,
    uri: String,
    config: Option<HashMap<String, serde_json::Value>>,
    capabilities: Vec<CapabilityName>,
    exposed: bool,
) -> Result<ToolDefinition> {
    if exposed {
        return Err(anyhow::anyhow!("Tool '{}' cannot have exposed=true", name));
    }
    Ok(ToolDefinition {
        name,
        uri,
        config,
        capabilities,
    })
}

fn create_implicit_tool_definitions(wasm_files: &[PathBuf]) -> Result<Vec<ToolDefinition>> {
    let mut definitions = Vec::new();
    for path in wasm_files {
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
        let definition = ToolDefinition {
            name,
            uri: path.to_string_lossy().to_string(),
            config: None,
            capabilities: Vec::new(),
        };
        definitions.push(definition);
    }
    Ok(definitions)
}

fn create_capability_registry(
    definitions: Vec<CapabilityDefinition>,
) -> Result<CapabilityRegistry> {
    let mut builder = CapabilityRegistryBuilder::new();
    for def in definitions {
        if def.uri.starts_with("wasmtime:") {
            let interfaces = get_interfaces_for_runtime_capability(&def.uri);
            let runtime_capability = RuntimeCapability {
                uri: def.uri,
                exposed: def.exposed,
                interfaces,
            };
            builder.add_runtime_capability(def.name, runtime_capability);
        } else {
            builder.add_pending_component_capability_definition(def);
        }
    }
    builder.build_registry()
}

fn create_tool_registry(
    definitions: Vec<ToolDefinition>,
    capability_registry: &CapabilityRegistry,
) -> Result<ToolRegistry> {
    let mut tool_registry = HashMap::new();
    for def in definitions {
        match process_tool_definition(&def, capability_registry) {
            Ok(spec) => {
                tool_registry.insert(def.name.clone(), spec);
            }
            Err(e) => {
                eprintln!("Warning: Skipping tool '{}' due to error: {e}", def.name);
                continue;
            }
        }
    }
    Ok(tool_registry)
}

fn get_interfaces_for_runtime_capability(uri: &str) -> Vec<String> {
    match uri {
        "wasmtime:http" => vec![
            "wasi:http/outgoing-handler@0.2.3".to_string(),
            "wasi:http/types@0.2.3".to_string(),
        ],
        "wasmtime:io" => vec![
            "wasi:io/error@0.2.3".to_string(),
            "wasi:io/poll@0.2.3".to_string(),
            "wasi:io/streams@0.2.3".to_string(),
        ],
        "wasmtime:inherit-network" => vec![
            "wasi:sockets/tcp@0.2.3".to_string(),
            "wasi:sockets/udp@0.2.3".to_string(),
            "wasi:sockets/network@0.2.3".to_string(),
            "wasi:sockets/instance-network@0.2.3".to_string(),
        ],
        "wasmtime:allow-ip-name-lookup" => vec!["wasi:sockets/ip-name-lookup@0.2.3".to_string()],
        "wasmtime:wasip2" => vec![
            "wasi:cli/environment@0.2.3".to_string(),
            "wasi:cli/exit@0.2.3".to_string(),
            "wasi:cli/stderr@0.2.3".to_string(),
            "wasi:cli/stdin@0.2.3".to_string(),
            "wasi:cli/stdout@0.2.3".to_string(),
            "wasi:clocks/monotonic-clock@0.2.3".to_string(),
            "wasi:clocks/wall-clock@0.2.3".to_string(),
            "wasi:filesystem/preopens@0.2.3".to_string(),
            "wasi:filesystem/types@0.2.3".to_string(),
            "wasi:io/error@0.2.3".to_string(),
            "wasi:io/poll@0.2.3".to_string(),
            "wasi:io/streams@0.2.3".to_string(),
            "wasi:random/random@0.2.3".to_string(),
            "wasi:sockets/tcp@0.2.3".to_string(),
            "wasi:sockets/udp@0.2.3".to_string(),
            "wasi:sockets/network@0.2.3".to_string(),
            "wasi:sockets/instance-network@0.2.3".to_string(),
            "wasi:sockets/ip-name-lookup@0.2.3".to_string(),
            "wasi:sockets/tcp-create-socket@0.2.3".to_string(),
            "wasi:sockets/udp-create-socket@0.2.3".to_string(),
        ],
        _ => {
            println!("Unknown runtime capability URI: {uri}");
            vec![]
        }
    }
}

fn resolve_component_capability_from_definition(
    definition: &CapabilityDefinition,
    capability_registry: &CapabilityRegistry,
) -> Result<ComponentCapability> {
    if definition.uri.starts_with("wasmtime:") {
        return Err(anyhow::anyhow!(
            "Wasmtime capability '{}' should not be resolved as component",
            definition.name
        ));
    }
    let component_spec = process_capability_definition(definition, capability_registry)?;
    let exports = Parser::discover_exports(&component_spec.bytes).map_err(|e| {
        anyhow::anyhow!(
            "Failed to discover exports for capability '{}': {}",
            definition.name,
            e
        )
    })?;
    Ok(ComponentCapability {
        component: component_spec,
        exposed: definition.exposed,
        exports,
    })
}

fn process_capability_definition(
    definition: &CapabilityDefinition,
    capability_registry: &CapabilityRegistry,
) -> Result<ComponentSpec> {
    let component_spec = process_component_core(
        &definition.name,
        &definition.uri,
        &definition.config,
        &definition.capabilities,
        capability_registry,
        false, // is_tool
    )?;
    for dependency_name in &definition.capabilities {
        if capability_registry
            .get_component_capability(dependency_name)
            .is_some()
        {
            println!(
                "Composed capability '{}' with dependency '{dependency_name}'",
                definition.name
            );
        }
    }
    Ok(component_spec)
}

fn process_tool_definition(
    definition: &ToolDefinition,
    capability_registry: &CapabilityRegistry,
) -> Result<ComponentSpec> {
    for capability_name in &definition.capabilities {
        if capability_registry
            .get_exposed_runtime_capability(capability_name)
            .is_none()
            && capability_registry
                .get_exposed_component_capability(capability_name)
                .is_none()
        {
            return Err(anyhow::anyhow!(
                "Tool '{}' requested unavailable capability '{}'",
                definition.name,
                capability_name
            ));
        }
    }
    let component_spec = process_component_core(
        &definition.name,
        &definition.uri,
        &definition.config,
        &definition.capabilities,
        capability_registry,
        true, // is_tool
    )?;
    for capability_name in &definition.capabilities {
        if capability_registry
            .get_exposed_component_capability(capability_name)
            .is_some()
        {
            println!(
                "Composed tool '{}' with capability '{capability_name}'",
                definition.name
            );
        }
    }
    Ok(component_spec)
}

fn process_component_core(
    name: &str,
    uri: &str,
    config: &Option<HashMap<String, serde_json::Value>>,
    capabilities: &[CapabilityName],
    capability_registry: &CapabilityRegistry,
    is_tool: bool,
) -> Result<ComponentSpec> {
    let component_path = resolve_uri(uri)?;
    let mut bytes = fs::read(&component_path)?;

    let mut component_imports = Parser::discover_imports(&bytes)
        .map_err(|e| anyhow::anyhow!("Failed to discover component imports: {}", e))?;

    let imports_config = component_imports
        .iter()
        .any(|import| import.starts_with("wasi:config/store"));

    if imports_config {
        let config_to_use = match config {
            Some(c) => c,
            None => &HashMap::new(),
        };
        bytes = Composer::compose_with_config(&bytes, config_to_use).map_err(|e| {
            anyhow::anyhow!(
                "Failed to compose {} '{}' with config: {}",
                if is_tool { "tool" } else { "capability" },
                name,
                e
            )
        })?;
        component_imports.retain(|import| !import.starts_with("wasi:config/store"));
    } else if config.is_some() {
        println!(
            "Warning: Config provided for {} '{}' but component doesn't import wasi:config/store",
            if is_tool { "tool" } else { "capability" },
            name
        );
    }

    validate_imports(
        &component_imports,
        capabilities,
        capability_registry,
        is_tool,
    )?;

    let mut remaining_capabilities = Vec::new();
    let mut all_runtime_capabilities = HashSet::new();

    for capability_name in capabilities {
        let component_capability = if is_tool {
            capability_registry.get_exposed_component_capability(capability_name)
        } else {
            capability_registry.get_component_capability(capability_name)
        };

        if let Some(component_capability) = component_capability {
            bytes = Composer::compose_components(&bytes, &component_capability.component.bytes)
                .map_err(|e| {
                    if is_tool {
                        anyhow::anyhow!(
                            "Failed to compose tool '{}' with capability '{}': {}",
                            name,
                            capability_name,
                            e
                        )
                    } else {
                        anyhow::anyhow!(
                            "Failed to compose capability '{}' with dependency '{}': {}",
                            name,
                            capability_name,
                            e
                        )
                    }
                })?;

            // Merge runtime capabilities from composed dependency capability
            all_runtime_capabilities.extend(
                component_capability
                    .component
                    .runtime_capabilities
                    .iter()
                    .cloned(),
            );
        } else {
            let runtime_capability = if is_tool {
                capability_registry.get_exposed_runtime_capability(capability_name)
            } else {
                capability_registry.get_runtime_capability(capability_name)
            };

            if runtime_capability.is_some() {
                // Runtime capability - keep for later linker setup
                remaining_capabilities.push(capability_name.clone());
            } else {
                return Err(anyhow::anyhow!(
                    "{} '{}' requested unavailable {} '{}'",
                    if is_tool { "Tool" } else { "Capability" },
                    name,
                    if is_tool { "capability" } else { "dependency" },
                    capability_name
                ));
            }
        }
    }

    if imports_config {
        if let Some(config) = config {
            let config_keys: Vec<_> = config.keys().collect();
            println!(
                "Composed {} '{}' with config: {config_keys:?}",
                if is_tool { "tool" } else { "capability" },
                name
            );
        }
    }

    // Merge direct runtime capabilities with composed ones
    all_runtime_capabilities.extend(remaining_capabilities);

    Ok(ComponentSpec {
        name: name.to_string(),
        bytes,
        runtime_capabilities: all_runtime_capabilities.into_iter().collect(),
    })
}

fn resolve_uri(uri: &str) -> Result<PathBuf> {
    if let Some(path_str) = uri.strip_prefix("file://") {
        Ok(PathBuf::from(path_str))
    } else {
        Ok(PathBuf::from(uri))
    }
}

/// Validate that component only imports interfaces covered by requested capabilities
fn validate_imports(
    component_imports: &[String],
    requested_capabilities: &[CapabilityName],
    capability_registry: &CapabilityRegistry,
    is_tool: bool,
) -> Result<()> {
    let expected_interfaces = get_expected_interfaces_from_capabilities(
        requested_capabilities,
        capability_registry,
        is_tool,
    );

    // Check that all component imports are covered by expected interfaces
    for import in component_imports {
        if !expected_interfaces.contains(import) {
            return Err(anyhow::anyhow!(
                "Component imports unauthorized interface '{}' - must request appropriate capability",
                import
            ));
        }
    }
    Ok(())
}

fn get_expected_interfaces_from_capabilities(
    capabilities: &[CapabilityName],
    capability_registry: &CapabilityRegistry,
    is_tool: bool,
) -> std::collections::HashSet<String> {
    let mut interfaces = std::collections::HashSet::new();
    for capability_name in capabilities {
        // Use get_runtime_capability even for is_tool (not get_exposed_runtime_capability) because:
        // 1. The tool definition was already checked against exposed runtime capabilities
        // 2. Here we are gathering all runtime capabilities expected by composed components
        // 3. The wasmtime linker needs to know those capabilities when instantiating the component
        if let Some(runtime_capability) =
            capability_registry.get_runtime_capability(capability_name)
        {
            interfaces.extend(runtime_capability.interfaces.iter().cloned());
        }

        // Add exported interfaces from component capabilities
        let component_capability = if is_tool {
            capability_registry.get_exposed_component_capability(capability_name)
        } else {
            capability_registry.get_component_capability(capability_name)
        };
        if let Some(component_capability) = component_capability {
            interfaces.extend(component_capability.exports.iter().cloned());
        }
    }
    interfaces
}
