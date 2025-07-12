use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::components::ComponentSpec;

pub type Capability = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRegistry {
    pub capabilities: HashMap<String, CapabilityDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDefinition {
    /// URI indicating the capability implementation
    /// - "wasmtime:feature-name" for built-in wasmtime features
    /// - Future: "file://path" or "oci://registry" for Wasm components
    /// - Future: other schemes for host functions
    pub uri: String,

    /// Whether tools can directly request this capability
    /// If false, only other capabilities can depend on it
    #[serde(default = "default_exposed")]
    pub exposed: bool,

    /// Capabilities that this capability depends on
    #[serde(default)]
    pub capabilities: Vec<Capability>,

    #[serde(default)]
    pub description: Option<String>,
}

fn default_exposed() -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerConfig {
    #[serde(default)]
    pub capabilities: HashMap<String, CapabilityDefinition>,
}

#[derive(Debug, Clone)]
pub enum CapabilityImplementation {
    Wasmtime(WasmtimeFeature), // Current: handled in linker/WASI context
    Component(ComponentSpec),  // Future: loaded and composed
}

#[derive(Debug, Clone)]
pub struct WasmtimeFeature {
    pub feature: String,
}

impl CapabilityRegistry {
    /// Create a new registry with no capabilities available by default
    pub fn new() -> Self {
        Self {
            capabilities: HashMap::new(),
        }
    }

    pub fn from_config_file<P: AsRef<Path>>(config_path: P) -> Result<Self> {
        let content = fs::read_to_string(config_path)?;
        let server_config: ServerConfig = toml::from_str(&content)?;
        Self::from_server_config(server_config)
    }

    pub fn from_server_config(config: ServerConfig) -> Result<Self> {
        let registry = Self {
            capabilities: config.capabilities,
        };
        registry.validate_no_cycles()?;
        Ok(registry)
    }

    /// Check if a capability is exposed to tools
    pub fn is_exposed(&self, name: &str) -> bool {
        self.capabilities
            .get(name)
            .map(|def| def.exposed)
            .unwrap_or(false)
    }

    pub fn get_capability(&self, name: &str) -> Option<&CapabilityDefinition> {
        self.capabilities.get(name)
    }

    /// Check if all capabilities are available to tools
    pub fn check_availability(&self, requested: &[Capability]) -> bool {
        requested
            .iter()
            .all(|capability| self.is_exposed(capability))
    }

    fn validate_no_cycles(&self) -> Result<()> {
        let mut visited = HashSet::new();
        let mut visiting = HashSet::new();
        for capability_name in self.capabilities.keys() {
            if !visited.contains(capability_name) {
                self.check_cycles_recursive(capability_name, &mut visited, &mut visiting)?;
            }
        }
        Ok(())
    }

    fn check_cycles_recursive(
        &self,
        capability: &str,
        visited: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
    ) -> Result<()> {
        if visited.contains(capability) {
            return Ok(());
        }

        if visiting.contains(capability) {
            return Err(anyhow::anyhow!(
                "Circular dependency detected involving capability: {}",
                capability
            ));
        }

        if let Some(definition) = self.get_capability(capability) {
            visiting.insert(capability.to_string());
            for dep in &definition.capabilities {
                self.check_cycles_recursive(dep, visited, visiting)?;
            }
            visiting.remove(capability);
        }
        visited.insert(capability.to_string());
        Ok(())
    }
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::new()
    }
}
