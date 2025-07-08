use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::components::{Capability, ComponentSpec};

#[derive(Debug, Deserialize, Serialize)]
struct ComponentConfig {
    uri: String,
    #[serde(default)]
    capabilities: Vec<Capability>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ToolsConfig {
    #[serde(flatten)]
    components: HashMap<String, ComponentConfig>,
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
    let config: ToolsConfig = toml::from_str(&content)?;
    let mut specs = Vec::new();
    for (name, component_config) in config.components {
        let component_path = resolve_uri(&component_config.uri)?;
        let bytes = fs::read(&component_path)?;
        specs.push(ComponentSpec {
            name,
            bytes,
            capabilities: component_config.capabilities,
        });
    }
    Ok(specs)
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
