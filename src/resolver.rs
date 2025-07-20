use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::PathBuf;

use crate::capabilities::{Capability, CapabilityRegistry, RuntimeCapability};
use crate::components::{CapabilityName, ComponentCapability, ComponentSpec};
use crate::composer::Composer;
use crate::interfaces::Parser;

#[derive(Debug)]
struct ResolvedCapability {
    name: String,
    capability: Capability,
    original_table: toml::map::Map<String, toml::Value>,
}

struct CapabilityRegistryBuilder {
    runtime_capabilities: HashMap<String, RuntimeCapability>,
    component_capabilities: HashMap<String, ComponentCapability>,
    pending: VecDeque<(String, toml::map::Map<String, toml::Value>)>,
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

    fn add_pending_component_capability(
        &mut self,
        name: String,
        table: toml::map::Map<String, toml::Value>,
    ) {
        self.pending.push_back((name, table));
    }

    fn try_next(&mut self) -> Result<Option<bool>> {
        if self.pending.is_empty() {
            return Ok(None);
        }

        let (name, table) = self.pending.pop_front().unwrap();

        // Create a temporary registry with current state for dependency checking
        let temp_registry = CapabilityRegistry::new(
            self.runtime_capabilities.clone(),
            self.component_capabilities.clone(),
        );

        match resolve_capability_from_toml(&name, &table, &temp_registry) {
            Ok(component_capability) => {
                self.component_capabilities
                    .insert(name, component_capability);
                Ok(Some(true)) // Successfully resolved
            }
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("unavailable dependency")
                    || error_msg.contains("unauthorized interface")
                {
                    self.last_errors.insert(name.clone(), error_msg);
                    self.pending.push_back((name, table));
                    Ok(Some(false)) // Failed but retryable
                } else {
                    // Other errors are real failures
                    Err(anyhow::anyhow!(
                        "Failed to resolve capability '{}': {}",
                        name,
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
                            .map(|(name, _)| {
                                if let Some(error) = self.last_errors.get(name) {
                                    format!("'{name}': {error}")
                                } else {
                                    format!("'{name}': unknown error")
                                }
                            })
                            .collect();

                        return Err(anyhow::anyhow!(
                            "Cannot resolve remaining capabilities:\n{}",
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

type ToolsAndConfigs = (
    HashMap<String, ComponentConfig>,
    HashMap<String, HashMap<String, serde_json::Value>>,
);

#[derive(Debug, Deserialize, Serialize)]
struct ComponentConfig {
    uri: String,
    #[serde(default)]
    capabilities: Vec<CapabilityName>,
}

/// Resolve tools from config files
pub fn resolve_tools(
    input_paths: &[PathBuf],
    capability_registry: &CapabilityRegistry,
) -> Result<Vec<ComponentSpec>> {
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
                        let specs = resolve_tool_toml_file(path, capability_registry)?;
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

/// Resolve capabilities (runtime + component) from server config file
pub fn resolve_capabilities(server_config_path: &PathBuf) -> Result<CapabilityRegistry> {
    let content = fs::read_to_string(server_config_path)?;
    let toml_doc: toml::Value = toml::from_str(&content)?;
    let resolved_capabilities = parse_all_capabilities(&toml_doc)?;
    create_capability_registry(&resolved_capabilities)
}

fn parse_all_capabilities(toml_doc: &toml::Value) -> Result<Vec<ResolvedCapability>> {
    let mut resolved_capabilities = Vec::new();
    if let toml::Value::Table(table) = toml_doc {
        if let Some(toml::Value::Table(capabilities_table)) = table.get("capabilities") {
            for (name, value) in capabilities_table {
                if let toml::Value::Table(capability_table) = value {
                    // Parse capability definition (excluding config subtable)
                    let mut capability_value = capability_table.clone();
                    capability_value.remove("config");

                    let capability: Capability = toml::Value::Table(capability_value)
                        .try_into()
                        .map_err(|e| {
                            anyhow::anyhow!("Failed to parse capability '{}': {}", name, e)
                        })?;

                    resolved_capabilities.push(ResolvedCapability {
                        name: name.clone(),
                        capability,
                        original_table: capability_table.clone(),
                    });
                }
            }
        }
    }
    Ok(resolved_capabilities)
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

/// Validate that component only imports interfaces covered by requested capabilities
fn validate_imports(
    component_bytes: &[u8],
    requested_capabilities: &[CapabilityName],
    capability_registry: &CapabilityRegistry,
    is_tool: bool, // true for tools (only exposed capabilities), false for capabilities (any dependency)
) -> Result<()> {
    let component_imports = Parser::discover_imports(component_bytes)
        .map_err(|e| anyhow::anyhow!("Failed to discover component imports: {}", e))?;

    let allowed_interfaces = get_allowed_interfaces_from_capabilities(
        requested_capabilities,
        capability_registry,
        is_tool,
    );

    // Validate: all component imports are covered by allowed interfaces
    for import in &component_imports {
        if !allowed_interfaces.contains(import) {
            return Err(anyhow::anyhow!(
                "Component imports unauthorized interface '{}' - must request appropriate capability",
                import
            ));
        }
    }
    Ok(())
}

fn get_allowed_interfaces_from_capabilities(
    capabilities: &[CapabilityName],
    capability_registry: &CapabilityRegistry,
    is_tool: bool,
) -> std::collections::HashSet<String> {
    let mut interfaces = std::collections::HashSet::new();
    for capability_name in capabilities {
        // Add interfaces from runtime capabilities
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

fn create_capability_registry(
    resolved_capabilities: &[ResolvedCapability],
) -> Result<CapabilityRegistry> {
    let mut builder = CapabilityRegistryBuilder::new();

    for resolved_cap in resolved_capabilities {
        if resolved_cap.capability.uri.starts_with("wasmtime:") {
            let interfaces = get_interfaces_for_runtime_capability(&resolved_cap.capability.uri);
            let runtime_capability = RuntimeCapability {
                uri: resolved_cap.capability.uri.clone(),
                exposed: resolved_cap.capability.exposed,
                interfaces,
            };
            builder.add_runtime_capability(resolved_cap.name.clone(), runtime_capability);
        } else {
            builder.add_pending_component_capability(
                resolved_cap.name.clone(),
                resolved_cap.original_table.clone(),
            );
        }
    }
    builder.build_registry()
}

fn resolve_capability_from_toml(
    name: &str,
    capability_table: &toml::map::Map<String, toml::Value>,
    capability_registry: &CapabilityRegistry,
) -> Result<ComponentCapability> {
    // Extract config subtable if present
    let config = if let Some(toml::Value::Table(config_table)) = capability_table.get("config") {
        Some(convert_toml_table_to_json_map(config_table)?)
    } else {
        None
    };

    // Parse capability definition (excluding config subtable)
    let mut capability_value = capability_table.clone();
    capability_value.remove("config");

    let capability: Capability = toml::Value::Table(capability_value)
        .try_into()
        .map_err(|e| anyhow::anyhow!("Failed to parse capability '{}': {}", name, e))?;

    if capability.uri.starts_with("wasmtime:") {
        return Err(anyhow::anyhow!(
            "Wasmtime capability '{}' should not be resolved as component",
            name
        ));
    }

    let component_path = resolve_uri(&capability.uri)?;
    let mut bytes = fs::read(&component_path)?;

    // Compose if config exists, even if empty (satisfies imports with defaults)
    if let Some(config) = &config {
        bytes = Composer::compose_tool_with_config(&bytes, config).map_err(|e| {
            anyhow::anyhow!("Failed to compose capability '{}' with config: {}", name, e)
        })?;
    }

    validate_imports(&bytes, &capability.capabilities, capability_registry, false)?;

    // Compose capability dependencies into this capability component
    let mut remaining_capabilities = Vec::new();
    let mut all_runtime_capabilities = Vec::new();

    for dependency_name in &capability.capabilities {
        if let Some(dependency_capability) =
            capability_registry.get_component_capability(dependency_name)
        {
            bytes = Composer::compose_components(&bytes, &dependency_capability.component.bytes)
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to compose capability '{}' with dependency '{}': {}",
                        name,
                        dependency_name,
                        e
                    )
                })?;

            // Merge runtime capabilities from composed dependency capability
            all_runtime_capabilities
                .extend(dependency_capability.component.runtime_capabilities.clone());
        } else if capability_registry
            .get_runtime_capability(dependency_name)
            .is_some()
        {
            // Runtime capability - keep for later linker setup
            remaining_capabilities.push(dependency_name.clone());
        } else {
            return Err(anyhow::anyhow!(
                "Capability '{}' requested unavailable dependency '{}'",
                name,
                dependency_name
            ));
        }
    }

    // Merge capability's direct runtime capabilities with those from composed dependencies
    all_runtime_capabilities.extend(remaining_capabilities);

    let exports = Parser::discover_exports(&bytes).map_err(|e| {
        anyhow::anyhow!(
            "Failed to discover exports for capability '{}': {}",
            name,
            e
        )
    })?;

    // Log successful composition operations
    if let Some(config) = &config {
        let config_keys: Vec<_> = config.keys().collect();
        println!(
            "Composed capability '{}' with config: {:?}",
            name, config_keys
        );
    }

    for dependency_name in &capability.capabilities {
        if capability_registry
            .get_component_capability(dependency_name)
            .is_some()
        {
            println!(
                "Composed capability '{}' with dependency '{}'",
                name, dependency_name
            );
        }
    }

    let component_spec = ComponentSpec {
        name: name.to_string(),
        bytes,
        runtime_capabilities: all_runtime_capabilities,
    };

    Ok(ComponentCapability {
        component: component_spec,
        exposed: capability.exposed,
        exports,
    })
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
        runtime_capabilities: Vec::new(),
    })
}

fn resolve_tool_toml_file(
    path: &PathBuf,
    capability_registry: &CapabilityRegistry,
) -> Result<Vec<ComponentSpec>> {
    let content = fs::read_to_string(path)?;
    let toml_doc: toml::Value = toml::from_str(&content)?;

    let (tools, configs) = extract_tools_and_configs(&toml_doc)?;

    let mut specs = Vec::new();
    for (name, component_config) in tools {
        let config = configs.get(&name).cloned();
        match resolve_tool(&name, component_config, config, capability_registry) {
            Ok(spec) => {
                specs.push(spec);
            }
            Err(e) => {
                eprintln!("Warning: Skipping tool '{name}' due to error: {e}");
                continue;
            }
        }
    }
    Ok(specs)
}

fn resolve_tool(
    name: &str,
    component_config: ComponentConfig,
    config: Option<HashMap<String, serde_json::Value>>,
    capability_registry: &CapabilityRegistry,
) -> Result<ComponentSpec> {
    let component_path = resolve_uri(&component_config.uri)?;
    let mut bytes = fs::read(&component_path)?;

    // Check if all requested capabilities exist before doing import validation
    for capability_name in &component_config.capabilities {
        if capability_registry
            .get_exposed_runtime_capability(capability_name)
            .is_none()
            && capability_registry
                .get_exposed_component_capability(capability_name)
                .is_none()
        {
            return Err(anyhow::anyhow!(
                "Tool '{}' requested unavailable capability '{}'",
                name,
                capability_name
            ));
        }
    }

    // Compose if config exists, even if empty (satisfies imports with defaults)
    if let Some(config) = &config {
        bytes = Composer::compose_tool_with_config(&bytes, config)
            .map_err(|e| anyhow::anyhow!("Failed to compose {} with config: {}", name, e))?;
    }

    // Validate imports after config composition (config provides wasi:config/store interfaces)
    validate_imports(
        &bytes,
        &component_config.capabilities,
        capability_registry,
        true,
    )?;

    // Compose with component capabilities
    let mut remaining_capabilities = Vec::new();
    let mut all_runtime_capabilities = Vec::new();

    for capability_name in &component_config.capabilities {
        if let Some(component_capability) =
            capability_registry.get_exposed_component_capability(capability_name)
        {
            bytes = Composer::compose_components(&bytes, &component_capability.component.bytes)
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to compose tool '{}' with capability '{}': {}",
                        name,
                        capability_name,
                        e
                    )
                })?;

            // Merge runtime capabilities from composed component capability
            all_runtime_capabilities
                .extend(component_capability.component.runtime_capabilities.clone());
        } else if capability_registry
            .get_exposed_runtime_capability(capability_name)
            .is_some()
        {
            // Runtime capability - keep for later linker setup
            remaining_capabilities.push(capability_name.clone());
        } else {
            return Err(anyhow::anyhow!(
                "Tool '{}' requested unavailable capability '{}'",
                name,
                capability_name
            ));
        }
    }

    // Merge tool's direct runtime capabilities with those from composed capabilities
    all_runtime_capabilities.extend(remaining_capabilities);

    // Log successful composition operations
    if let Some(config) = &config {
        let config_keys: Vec<_> = config.keys().collect();
        println!("Composed tool '{}' with config: {:?}", name, config_keys);
    }

    for capability_name in &component_config.capabilities {
        if capability_registry
            .get_exposed_component_capability(capability_name)
            .is_some()
        {
            println!(
                "Composed tool '{}' with capability '{}'",
                name, capability_name
            );
        }
    }

    Ok(ComponentSpec {
        name: name.to_string(),
        bytes,
        runtime_capabilities: all_runtime_capabilities,
    })
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
