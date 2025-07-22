use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::components::ComponentCapability;

pub type CapabilityName = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCapability {
    pub uri: String,
    pub exposed: bool,
    pub interfaces: Vec<String>, // WASI interfaces this capability provides
}

#[derive(Debug, Clone)]
pub struct CapabilityRegistry {
    pub runtime_capabilities: HashMap<String, RuntimeCapability>,
    pub component_capabilities: HashMap<String, ComponentCapability>,
}

impl CapabilityRegistry {
    /// Create a new registry from resolved capability maps
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
