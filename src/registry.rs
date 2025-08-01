use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use crate::composer::Composer;
use crate::loader::{ComponentDefinition, RuntimeFeatureDefinition};
use crate::wit::{ComponentMetadata, Parser};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeFeature {
    pub uri: String,
    pub enables: String,
    pub interfaces: Vec<String>, // WASI interfaces this runtime feature provides
}

#[derive(Debug, Clone)]
pub struct ComponentSpec {
    pub name: String,
    pub namespace: Option<String>,
    pub package: Option<String>,
    pub bytes: Vec<u8>,
    pub imports: Vec<String>,
    pub exports: Vec<String>,
    pub runtime_features: Vec<String>,
    pub functions: Option<HashMap<String, crate::wit::Function>>,
}

#[derive(Debug, Clone)]
pub struct RuntimeFeatureRegistry {
    pub runtime_features: HashMap<String, RuntimeFeature>,
}

#[derive(Debug, Clone)]
pub struct ComponentRegistry {
    pub components: HashMap<String, ComponentSpec>,
    enabling_components: HashMap<String, EnablingComponent>,
}

#[derive(Debug, Clone)]
struct EnablingComponent {
    pub component: ComponentSpec,
    pub exposed: bool,
    pub enables: String,
    pub exports: Vec<String>, // Interfaces this enabling component provides
}

impl RuntimeFeatureRegistry {
    pub fn new(runtime_features: HashMap<String, RuntimeFeature>) -> Self {
        Self { runtime_features }
    }

    pub fn get_runtime_feature(&self, name: &str) -> Option<&RuntimeFeature> {
        self.runtime_features.get(name)
    }

    pub fn get_enabled_runtime_feature(
        &self,
        requesting_component: &ComponentDefinition,
        feature_name: &str,
    ) -> Option<&RuntimeFeature> {
        if let Some(runtime_feature) = self.runtime_features.get(feature_name) {
            match runtime_feature.enables.as_str() {
                "none" => None,
                "any" => Some(runtime_feature),
                "exposed" => {
                    if requesting_component.exposed {
                        Some(runtime_feature)
                    } else {
                        None
                    }
                }
                "unexposed" => {
                    if !requesting_component.exposed {
                        Some(runtime_feature)
                    } else {
                        None
                    }
                }
                "package" => None,
                "namespace" => None,
                _ => None, // Unknown enables scope
            }
        } else {
            None
        }
    }
}

impl ComponentRegistry {
    pub fn new(components: HashMap<String, ComponentSpec>) -> Self {
        Self {
            components,
            enabling_components: HashMap::new(),
        }
    }

    pub fn empty() -> Self {
        Self::new(HashMap::new())
    }

    pub fn get_components(&self) -> impl Iterator<Item = &ComponentSpec> {
        self.components.values()
    }

    pub fn get_enabled_component_dependency(
        &self,
        requesting_component: &ComponentDefinition,
        requesting_metadata: &ComponentMetadata,
        dependency_name: &str,
    ) -> Option<&ComponentSpec> {
        if let Some(enabling_component) = self.enabling_components.get(dependency_name) {
            match enabling_component.enables.as_str() {
                "none" => None,
                "any" => Some(&enabling_component.component),
                "exposed" => {
                    if requesting_component.exposed {
                        Some(&enabling_component.component)
                    } else {
                        None
                    }
                }
                "unexposed" => {
                    if !requesting_component.exposed {
                        Some(&enabling_component.component)
                    } else {
                        None
                    }
                }
                "package" => {
                    match (
                        requesting_metadata.package.as_deref(),
                        enabling_component.component.package.as_deref(),
                    ) {
                        (Some(req_pkg), Some(enable_pkg)) if req_pkg == enable_pkg => {
                            Some(&enabling_component.component)
                        }
                        _ => None,
                    }
                }
                "namespace" => {
                    match (
                        requesting_metadata.namespace.as_deref(),
                        enabling_component.component.namespace.as_deref(),
                    ) {
                        (Some(req_ns), Some(enable_ns)) if req_ns == enable_ns => {
                            Some(&enabling_component.component)
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        } else {
            None
        }
    }
}

impl Default for ComponentRegistry {
    fn default() -> Self {
        Self::empty()
    }
}

/// Build registries from definitions
pub async fn build_registries(
    runtime_feature_definitions: Vec<RuntimeFeatureDefinition>,
    component_definitions: Vec<ComponentDefinition>,
) -> Result<(RuntimeFeatureRegistry, ComponentRegistry)> {
    let runtime_feature_registry =
        create_runtime_feature_registry(runtime_feature_definitions).await?;
    let component_registry =
        create_component_registry(component_definitions, &runtime_feature_registry).await?;
    Ok((runtime_feature_registry, component_registry))
}

struct ComponentRegistryBuilder {
    components: HashMap<String, ComponentSpec>,
    enabling_components: HashMap<String, EnablingComponent>,
    runtime_features: HashMap<String, RuntimeFeature>,
    pending: VecDeque<ComponentDefinition>,
}

impl ComponentRegistryBuilder {
    fn new() -> Self {
        Self {
            components: HashMap::new(),
            enabling_components: HashMap::new(),
            runtime_features: HashMap::new(),
            pending: VecDeque::new(),
        }
    }

    fn add_pending_component_definition(&mut self, definition: ComponentDefinition) {
        self.pending.push_back(definition);
    }

    async fn try_next(&mut self) -> Result<Option<bool>> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let definition = self.pending.pop_front().unwrap();

        for dependency_name in &definition.expects {
            if !self.runtime_features.contains_key(dependency_name)
                && !self.enabling_components.contains_key(dependency_name)
            {
                // Dependency missing - will retry
                self.pending.push_back(definition);
                return Ok(Some(false));
            }
        }

        // Create temporary registries for dependency resolution
        let temp_runtime_registry = RuntimeFeatureRegistry::new(self.runtime_features.clone());
        let temp_component_registry = ComponentRegistry {
            components: self.components.clone(),
            enabling_components: self.enabling_components.clone(),
        };

        match process_component(
            &definition,
            &temp_runtime_registry,
            &temp_component_registry,
        )
        .await
        {
            Ok(component_spec) => {
                // Store only exposed components in final registry
                if definition.exposed {
                    self.components
                        .insert(definition.name.clone(), component_spec.clone());
                }

                // Create enabling wrapper if this component can enable others
                if definition.enables != "none" {
                    let enabling_component = EnablingComponent {
                        component: component_spec.clone(),
                        exposed: definition.exposed,
                        enables: definition.enables.clone(),
                        exports: component_spec.exports.clone(),
                    };
                    self.enabling_components
                        .insert(definition.name.clone(), enabling_component);
                }

                Ok(Some(true)) // Successfully processed
            }
            Err(e) => {
                if definition.exposed {
                    // Skip exposed components on any error
                    eprintln!(
                        "Warning: Skipping exposed component '{}': {}",
                        definition.name, e
                    );
                    Ok(Some(true)) // Continue processing (no component added to registry)
                } else {
                    // Fail for non-exposed components
                    Err(anyhow::anyhow!(
                        "Failed to resolve component '{}': {}",
                        definition.name,
                        e
                    ))
                }
            }
        }
    }

    async fn build_registry(mut self) -> Result<ComponentRegistry> {
        let mut attempts = 0;
        let max_attempts = self.pending.len() * self.pending.len(); // Prevent infinite loops

        let mut consecutive_retries = 0;
        while !self.pending.is_empty() && attempts < max_attempts {
            match self.try_next().await? {
                Some(true) => {
                    // Component processed successfully (or exposed component skipped)
                    consecutive_retries = 0;
                }
                Some(false) => {
                    // Component retried due to missing dependencies
                    consecutive_retries += 1;
                    if consecutive_retries >= self.pending.len() {
                        let (exposed_failures, unexposed_failures): (Vec<_>, Vec<_>) =
                            self.pending.iter().partition(|def| def.exposed);

                        // Skip exposed components with warnings
                        for definition in &exposed_failures {
                            eprintln!(
                                "Warning: Skipping exposed component '{}' due to missing dependencies",
                                definition.name
                            );
                        }

                        // Fail if any non-exposed components cannot be resolved
                        if !unexposed_failures.is_empty() {
                            let failed_names: Vec<String> = unexposed_failures
                                .iter()
                                .map(|definition| format!("'{}'", definition.name))
                                .collect();

                            return Err(anyhow::anyhow!(
                                "Cannot resolve component dependencies: {}",
                                failed_names.join(", ")
                            ));
                        }

                        // All remaining failures were exposed components - continue
                        break;
                    }
                }
                None => {
                    // Queue is empty
                    break;
                }
            }
            attempts += 1;
        }

        Ok(ComponentRegistry::new(self.components))
    }
}

async fn create_runtime_feature_registry(
    runtime_feature_definitions: Vec<RuntimeFeatureDefinition>,
) -> Result<RuntimeFeatureRegistry> {
    let mut runtime_features = HashMap::new();

    for def in runtime_feature_definitions {
        let interfaces = get_interfaces_for_runtime_feature(&def.uri);
        let runtime_feature = RuntimeFeature {
            uri: def.uri.clone(),
            enables: def.enables.clone(),
            interfaces,
        };
        runtime_features.insert(def.name.clone(), runtime_feature);
    }

    Ok(RuntimeFeatureRegistry::new(runtime_features))
}

async fn create_component_registry(
    component_definitions: Vec<ComponentDefinition>,
    runtime_feature_registry: &RuntimeFeatureRegistry,
) -> Result<ComponentRegistry> {
    let mut builder = ComponentRegistryBuilder::new();

    // Add runtime features for dependency resolution
    builder.runtime_features = runtime_feature_registry.runtime_features.clone();

    // Add all component definitions to pending queue
    for def in component_definitions {
        builder.add_pending_component_definition(def);
    }

    builder.build_registry().await
}

fn get_interfaces_for_runtime_feature(uri: &str) -> Vec<String> {
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
            println!("Unknown runtime feature URI: {uri}");
            vec![]
        }
    }
}

async fn process_component(
    definition: &ComponentDefinition,
    runtime_feature_registry: &RuntimeFeatureRegistry,
    component_registry: &ComponentRegistry,
) -> Result<ComponentSpec> {
    let mut bytes = read_bytes(&definition.uri).await?;

    let (metadata, mut imports, exports, functions) = Parser::parse(&bytes, definition.exposed)
        .map_err(|e| anyhow::anyhow!("Failed to parse component: {}", e))?;

    let imports_config = imports
        .iter()
        .any(|import| import.starts_with("wasi:config/store"));

    if imports_config {
        let config_to_use = match &definition.config {
            Some(c) => c,
            None => &HashMap::new(),
        };
        bytes = Composer::compose_with_config(&bytes, config_to_use).map_err(|e| {
            anyhow::anyhow!(
                "Failed to compose component '{}' with config: {}",
                definition.name,
                e
            )
        })?;

        let config_keys: Vec<_> = config_to_use.keys().collect();
        println!(
            "Composed component '{}' with config: {config_keys:?}",
            definition.name
        );

        imports.retain(|import| !import.starts_with("wasi:config/store"));
    } else if definition.config.is_some() {
        println!(
            "Warning: Config provided for component '{}' but component doesn't import wasi:config/store",
            definition.name
        );
    }

    let mut remaining_expects = Vec::new();
    let mut all_runtime_features = HashSet::new();

    for dependency_name in &definition.expects {
        if let Some(component_spec) = component_registry.get_enabled_component_dependency(
            definition,
            &metadata,
            dependency_name,
        ) {
            bytes = Composer::compose_components(&bytes, &component_spec.bytes).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to compose component '{}' with dependency '{}': {}",
                    definition.name,
                    dependency_name,
                    e
                )
            })?;

            println!(
                "Composed component '{}' with dependency '{}'",
                definition.name, dependency_name
            );

            // Track satisfied imports from this dependency
            for export in &component_spec.exports {
                imports.retain(|import| import != export);
            }

            // Merge runtime expects from composed dependency component
            all_runtime_features.extend(component_spec.runtime_features.iter().cloned());
        } else if let Some(_runtime_feature) =
            runtime_feature_registry.get_enabled_runtime_feature(definition, dependency_name)
        {
            // Runtime feature dependency - keep for later context/linker setup
            remaining_expects.push(dependency_name.clone());
        } else {
            return Err(anyhow::anyhow!(
                "Component '{}' requested unavailable dependency '{}'",
                definition.name,
                dependency_name
            ));
        }
    }

    // Merge direct runtime expects with composed ones
    all_runtime_features.extend(remaining_expects);

    let runtime_interfaces: std::collections::HashSet<String> = all_runtime_features
        .iter()
        .filter_map(|name| runtime_feature_registry.get_runtime_feature(name))
        .flat_map(|rf| rf.interfaces.iter().cloned())
        .collect();

    // Check for imports not satisfied by runtime features
    let unsatisfied: Vec<_> = imports
        .iter()
        .filter(|import| !runtime_interfaces.contains(*import))
        .cloned()
        .collect();

    if !unsatisfied.is_empty() {
        return Err(anyhow::anyhow!(
            "Component '{}' has unsatisfied imports: {:?}",
            definition.name,
            unsatisfied
        ));
    }

    Ok(ComponentSpec {
        name: definition.name.clone(),
        namespace: metadata.namespace,
        package: metadata.package,
        bytes,
        imports,
        exports,
        runtime_features: all_runtime_features.into_iter().collect(),
        functions,
    })
}

async fn read_bytes(uri: &str) -> Result<Vec<u8>> {
    if let Some(oci_ref) = uri.strip_prefix("oci://") {
        let client = wasm_pkg_client::oci::client::Client::new(Default::default());
        let image_ref = oci_ref.parse()?;
        let auth = oci_client::secrets::RegistryAuth::Anonymous;
        let media_types = vec!["application/wasm", "application/vnd.wasm.component"];

        let image_data = client.pull(&image_ref, &auth, media_types).await?;

        // Get the component bytes from the first layer
        if let Some(layer) = image_data.layers.first() {
            Ok(layer.data.clone())
        } else {
            Err(anyhow::anyhow!("No layers found in OCI image: {}", oci_ref))
        }
    } else {
        // Handle both file:// and plain paths
        let path = if let Some(path_str) = uri.strip_prefix("file://") {
            PathBuf::from(path_str)
        } else {
            PathBuf::from(uri)
        };
        Ok(std::fs::read(path)?)
    }
}
