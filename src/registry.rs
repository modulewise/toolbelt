use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::composer::Composer;
use crate::loader::{CapabilityDefinition, CapabilityName, ToolDefinition};
use crate::wit::Parser;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ComponentSpec {
    pub name: String,
    pub bytes: Vec<u8>,
    pub imports: Vec<String>,
    pub exports: Vec<String>,
    pub runtime_capabilities: Vec<CapabilityName>,
    pub functions: Option<HashMap<String, crate::wit::Function>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCapability {
    pub uri: String,
    pub exposed: bool,
    pub interfaces: Vec<String>, // WASI interfaces this capability provides
}

#[derive(Debug, Clone)]
pub struct ComponentCapability {
    pub component: ComponentSpec,
    pub exposed: bool,
    pub exports: Vec<String>, // Interfaces this component capability provides
}

#[derive(Debug, Clone)]
pub struct CapabilityRegistry {
    pub runtime_capabilities: HashMap<String, RuntimeCapability>,
    pub component_capabilities: HashMap<String, ComponentCapability>,
}

impl CapabilityRegistry {
    pub fn new(
        runtime_capabilities: HashMap<String, RuntimeCapability>,
        component_capabilities: HashMap<String, ComponentCapability>,
    ) -> Self {
        Self {
            runtime_capabilities,
            component_capabilities,
        }
    }

    /// Create an empty registry with no capabilities
    pub fn empty() -> Self {
        Self {
            runtime_capabilities: HashMap::new(),
            component_capabilities: HashMap::new(),
        }
    }

    pub fn get_runtime_capability(&self, name: &str) -> Option<&RuntimeCapability> {
        self.runtime_capabilities.get(name)
    }

    /// Get runtime capability only if it's exposed to tools
    pub fn get_exposed_runtime_capability(&self, name: &str) -> Option<&RuntimeCapability> {
        self.runtime_capabilities
            .get(name)
            .filter(|cap| cap.exposed)
    }

    pub fn get_component_capability(&self, name: &str) -> Option<&ComponentCapability> {
        self.component_capabilities.get(name)
    }

    /// Get component capability only if it's exposed to tools
    pub fn get_exposed_component_capability(&self, name: &str) -> Option<&ComponentCapability> {
        self.component_capabilities
            .get(name)
            .filter(|cap| cap.exposed)
    }
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::empty()
    }
}

/// Tool registry type alias - contains tool component specifications
pub type ToolRegistry = HashMap<String, ComponentSpec>;

/// Build registries from definitions
pub fn build_registries(
    capability_definitions: Vec<CapabilityDefinition>,
    tool_definitions: Vec<ToolDefinition>,
) -> Result<(CapabilityRegistry, ToolRegistry)> {
    let capability_registry = create_capability_registry(capability_definitions)?;
    let tool_registry = create_tool_registry(tool_definitions, &capability_registry)?;
    Ok((capability_registry, tool_registry))
}

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

fn create_capability_registry(
    definitions: Vec<CapabilityDefinition>,
) -> Result<CapabilityRegistry> {
    let mut builder = CapabilityRegistryBuilder::new();
    for def in definitions {
        if def.uri.starts_with("wasmtime:") {
            let interfaces = get_interfaces_for_runtime_capability(&def.uri);
            let runtime_capability = RuntimeCapability {
                uri: def.uri.clone(),
                exposed: def.exposed,
                interfaces,
            };
            builder.add_runtime_capability(def.name.clone(), runtime_capability);
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
    let exports = component_spec.exports.clone();
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

    let (mut imports, exports, functions) = Parser::parse(&bytes, is_tool)
        .map_err(|e| anyhow::anyhow!("Failed to parse component: {}", e))?;

    let imports_config = imports
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
        imports.retain(|import| !import.starts_with("wasi:config/store"));
    } else if config.is_some() {
        println!(
            "Warning: Config provided for {} '{}' but component doesn't import wasi:config/store",
            if is_tool { "tool" } else { "capability" },
            name
        );
    }

    validate_imports(&imports, capabilities, capability_registry, is_tool)?;

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
        imports,
        exports,
        runtime_capabilities: all_runtime_capabilities.into_iter().collect(),
        functions,
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
