use anyhow::Result;
use rmcp::model::Tool;
use serde_json::json;
use std::fmt;
use wit_parser::{Resolve, Type};

pub struct Parser;

#[derive(Debug, Clone, Copy)]
enum WorldItemType {
    Exports,
    Imports,
}

/// A validated WebAssembly Interface Type (WIT) interface name
/// Format: namespace:package/interface[@version]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Interface {
    namespace: String,
    package: String,
    interface: String,
    version: Option<String>,
    full_name: String,
}

impl Interface {
    /// Parse and validate a WIT interface string
    pub fn parse(s: &str) -> Result<Self> {
        if let Some((namespace, rest)) = s.split_once(':') {
            if let Some((package, after_slash)) = rest.split_once('/') {
                let (interface, version) = if let Some((i, v)) = after_slash.split_once('@') {
                    (i, Some(v.to_string()))
                } else {
                    (after_slash, None)
                };

                return Ok(Self {
                    namespace: namespace.to_string(),
                    package: package.to_string(),
                    interface: interface.to_string(),
                    version,
                    full_name: s.to_string(),
                });
            }
        }

        Err(anyhow::anyhow!(
            "Invalid WIT interface format: expected namespace:package/interface[@version], got: {}",
            s
        ))
    }

    /// Get the full interface string
    pub fn as_str(&self) -> &str {
        &self.full_name
    }

    /// Get the namespace (e.g., "wasi" from "wasi:http/outgoing-handler@0.2.3")
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Get the package (e.g., "http" from "wasi:http/outgoing-handler@0.2.3")
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Get the interface name (e.g., "outgoing-handler" from "wasi:http/outgoing-handler@0.2.3")
    pub fn interface_name(&self) -> &str {
        &self.interface
    }

    /// Get the version (e.g., Some("0.2.3") from "wasi:http/outgoing-handler@0.2.3")
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
}

impl fmt::Display for Interface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.full_name)
    }
}

/// A WebAssembly function call specification
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Function {
    interface: Interface,
    function: String,
}

impl Function {
    /// Create a new WIT function specification
    pub fn new(interface: Interface, function: String) -> Self {
        Self {
            interface,
            function,
        }
    }

    /// Get the interface
    pub fn interface(&self) -> &Interface {
        &self.interface
    }

    /// Get the function name
    pub fn function_name(&self) -> &str {
        &self.function
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.interface, self.function)
    }
}

#[derive(Clone, Debug)]
pub struct ComponentTool {
    pub tool: Tool,
    pub bytes: Vec<u8>,
    pub namespace: String,
    pub package: String,
    pub version: Option<String>,
    pub interface: String,
    pub function: String,
    pub params: Vec<FunctionParam>,
}

#[derive(Clone, Debug)]
struct ComponentFunction {
    pub component: String,
    pub namespace: String,
    pub package: String,
    pub version: Option<String>,
    pub interface: String,
    pub function: String,
    pub docs: String,
    pub params: Vec<FunctionParam>,
    pub returns: Vec<Type>,
}

#[derive(Clone, Debug)]
pub struct FunctionParam {
    pub name: String,
    pub wit_type: Type,
}

impl Parser {
    pub fn parse(component_bytes: &[u8], component_name: &str) -> Result<Vec<ComponentTool>> {
        let bytes = component_bytes.to_vec();
        let decoded = wit_parser::decoding::decode(component_bytes)?;
        let resolve = decoded.resolve().clone();

        if resolve.worlds.len() != 1 {
            return Err(anyhow::anyhow!("Expected exactly one world in component"));
        }

        let mut functions = Vec::new();
        let (_, world) = resolve.worlds.iter().next().unwrap();

        for (_, item) in &world.exports {
            if let wit_parser::WorldItem::Interface { id, stability: _ } = item {
                let interface_functions = Self::parse_interface(id, &resolve, component_name)?;
                functions.extend(interface_functions);
            }
        }

        let requires_disambiguation = Self::has_function_name_conflicts(&functions);
        let component_tools = functions
            .into_iter()
            .map(|func| {
                Self::function_to_component_tool(func, &bytes, requires_disambiguation, &resolve)
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(component_tools)
    }

    /// Discover interface exports from a component
    /// Returns a list of full interface names (e.g., "wasi:keyvalue/store@0.2.0")
    pub fn discover_exports(component_bytes: &[u8]) -> Result<Vec<String>> {
        Self::discover_interfaces(component_bytes, WorldItemType::Exports)
    }

    /// Discover interface imports from a component
    /// Returns a list of full interface names (e.g., "wasi:http/outgoing-handler@0.2.0")
    pub fn discover_imports(component_bytes: &[u8]) -> Result<Vec<String>> {
        Self::discover_interfaces(component_bytes, WorldItemType::Imports)
    }

    fn discover_interfaces(
        component_bytes: &[u8],
        item_type: WorldItemType,
    ) -> Result<Vec<String>> {
        let decoded = wit_parser::decoding::decode(component_bytes)?;
        let resolve = decoded.resolve().clone();

        if resolve.worlds.len() != 1 {
            return Err(anyhow::anyhow!("Expected exactly one world in component"));
        }

        let mut interfaces = Vec::new();
        let (_, world) = resolve.worlds.iter().next().unwrap();

        let world_items = match item_type {
            WorldItemType::Exports => &world.exports,
            WorldItemType::Imports => &world.imports,
        };

        for (_, item) in world_items {
            if let wit_parser::WorldItem::Interface { id, stability: _ } = item {
                let interface_name = Self::build_full_interface_name(&resolve, *id)?;
                interfaces.push(interface_name);
            }
        }
        Ok(interfaces)
    }

    fn build_full_interface_name(
        resolve: &wit_parser::Resolve,
        interface_id: wit_parser::InterfaceId,
    ) -> Result<String> {
        let interface = resolve.interfaces.get(interface_id).unwrap();
        if let Some(interface_name) = &interface.name {
            if let Some(package_id) = &interface.package {
                let package = resolve.packages.get(*package_id).unwrap();
                let package_name = &package.name;
                let version_suffix = package_name
                    .version
                    .as_ref()
                    .map(|v| format!("@{v}"))
                    .unwrap_or_default();
                let full_interface_name = format!(
                    "{}:{}/{}{}",
                    package_name.namespace, package_name.name, interface_name, version_suffix
                );
                Ok(full_interface_name)
            } else {
                Err(anyhow::anyhow!(
                    "Interface '{}' missing required package metadata",
                    interface_name
                ))
            }
        } else {
            Err(anyhow::anyhow!("Interface missing name"))
        }
    }

    fn parse_interface(
        interface_id: &wit_parser::InterfaceId,
        resolve: &Resolve,
        component_name: &str,
    ) -> Result<Vec<ComponentFunction>> {
        let interface = resolve.interfaces.get(*interface_id).unwrap();
        let interface_name = interface
            .name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Interface missing required name"))?
            .clone();

        let (namespace, package, version) = if let Some(package_id) = &interface.package {
            let package = resolve.packages.get(*package_id).unwrap();
            let package_name = &package.name;
            (
                package_name.namespace.clone(),
                package_name.name.clone(),
                package_name.version.as_ref().map(|v| v.to_string()),
            )
        } else {
            return Err(anyhow::anyhow!(
                "Component interface missing required package metadata"
            ));
        };

        let mut functions = Vec::new();
        for (func_name, func) in &interface.functions {
            let params = func
                .params
                .iter()
                .map(|(param_name, param_type)| FunctionParam {
                    name: param_name.clone(),
                    wit_type: *param_type,
                })
                .collect();

            let returns = match &func.result {
                Some(return_type) => vec![*return_type],
                None => vec![],
            };

            functions.push(ComponentFunction {
                component: component_name.to_string(),
                namespace: namespace.clone(),
                package: package.clone(),
                version: version.clone(),
                interface: interface_name.clone(),
                function: func_name.clone(),
                docs: func.docs.contents.as_deref().unwrap_or("").to_string(),
                params,
                returns,
            });
        }
        Ok(functions)
    }

    fn validate_wit_type_for_json_rpc(wit_type: Type, resolve: &Resolve) -> Result<()> {
        match wit_type {
            // Primitives are all supported
            Type::Bool
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::S8
            | Type::S16
            | Type::S32
            | Type::S64
            | Type::F32
            | Type::F64
            | Type::Char
            | Type::String
            | Type::ErrorContext => Ok(()),

            // Complex types need validation
            Type::Id(type_id) => {
                let type_def = resolve
                    .types
                    .get(type_id)
                    .expect("Type definition not found for type ID");
                match &type_def.kind {
                    wit_parser::TypeDefKind::Type(inner_type) => {
                        Self::validate_wit_type_for_json_rpc(*inner_type, resolve)
                    }
                    wit_parser::TypeDefKind::Record(record) => {
                        for field in &record.fields {
                            Self::validate_wit_type_for_json_rpc(field.ty, resolve)?;
                        }
                        Ok(())
                    }
                    wit_parser::TypeDefKind::Variant(variant) => {
                        for case in &variant.cases {
                            if let Some(case_type) = case.ty {
                                Self::validate_wit_type_for_json_rpc(case_type, resolve)?;
                            }
                        }
                        Ok(())
                    }
                    wit_parser::TypeDefKind::Enum(_) => Ok(()),
                    wit_parser::TypeDefKind::Option(option_type) => {
                        Self::validate_wit_type_for_json_rpc(*option_type, resolve)
                    }
                    wit_parser::TypeDefKind::Result(result_type) => {
                        if let Some(ok_type) = result_type.ok {
                            Self::validate_wit_type_for_json_rpc(ok_type, resolve)?;
                        }
                        if let Some(err_type) = result_type.err {
                            Self::validate_wit_type_for_json_rpc(err_type, resolve)?;
                        }
                        Ok(())
                    }
                    wit_parser::TypeDefKind::List(element_type) => {
                        Self::validate_wit_type_for_json_rpc(*element_type, resolve)
                    }
                    wit_parser::TypeDefKind::Tuple(tuple) => {
                        for tuple_type in &tuple.types {
                            Self::validate_wit_type_for_json_rpc(*tuple_type, resolve)?;
                        }
                        Ok(())
                    }
                    wit_parser::TypeDefKind::Flags(_) => Ok(()),
                    wit_parser::TypeDefKind::Resource => Err(anyhow::anyhow!(
                        "Resource types cannot be represented in JSON-RPC"
                    )),
                    wit_parser::TypeDefKind::Handle(_) => Err(anyhow::anyhow!(
                        "Resource handles cannot be represented in JSON-RPC"
                    )),
                    _ => Err(anyhow::anyhow!("Unsupported WIT type: {:?}", type_def.kind)),
                }
            }
        }
    }

    fn wit_type_to_json_schema(wit_type: Type, resolve: &Resolve) -> serde_json::Value {
        match wit_type {
            // Primitives - direct mappings
            Type::Bool => json!({"type": "boolean"}),
            Type::U8 => json!({"type": "number", "minimum": 0, "maximum": 255}),
            Type::U16 => json!({"type": "number", "minimum": 0, "maximum": 65535}),
            Type::U32 => json!({"type": "number", "minimum": 0, "maximum": 4294967295_u64}),
            Type::U64 => json!({"type": "number", "minimum": 0}),
            Type::S8 => json!({"type": "number", "minimum": -128, "maximum": 127}),
            Type::S16 => json!({"type": "number", "minimum": -32768, "maximum": 32767}),
            Type::S32 => {
                json!({"type": "number", "minimum": -2147483648_i64, "maximum": 2147483647})
            }
            Type::S64 => json!({"type": "number"}),
            Type::F32 | Type::F64 => json!({"type": "number"}),
            Type::Char => json!({"type": "string", "minLength": 1, "maxLength": 1}),
            Type::String => json!({"type": "string"}),

            // Complex types
            Type::Id(type_id) => {
                let type_def = resolve
                    .types
                    .get(type_id)
                    .expect("Type definition not found for type ID");
                match &type_def.kind {
                    wit_parser::TypeDefKind::Type(inner_type) => {
                        Self::wit_type_to_json_schema(*inner_type, resolve)
                    }
                    wit_parser::TypeDefKind::Record(record) => {
                        let mut properties = serde_json::Map::new();
                        let mut required = Vec::new();

                        for field in &record.fields {
                            properties.insert(
                                field.name.clone(),
                                Self::wit_type_to_json_schema(field.ty, resolve),
                            );
                            required.push(field.name.clone());
                        }

                        json!({
                            "type": "object",
                            "properties": properties,
                            "required": required,
                            "additionalProperties": false
                        })
                    }
                    wit_parser::TypeDefKind::Variant(variant) => {
                        let cases: Vec<serde_json::Value> = variant.cases.iter().map(|case| {
                                if let Some(case_type) = case.ty {
                                    json!({
                                        "type": "object",
                                        "properties": {
                                            "type": {"const": case.name},
                                            "value": Self::wit_type_to_json_schema(case_type, resolve)
                                        },
                                        "required": ["type", "value"],
                                        "additionalProperties": false
                                    })
                                } else {
                                    json!({
                                        "type": "object", 
                                        "properties": {
                                            "type": {"const": case.name}
                                        },
                                        "required": ["type"],
                                        "additionalProperties": false
                                    })
                                }
                            }).collect();

                        json!({
                            "oneOf": cases
                        })
                    }
                    wit_parser::TypeDefKind::Enum(enum_def) => {
                        let enum_values: Vec<&String> =
                            enum_def.cases.iter().map(|case| &case.name).collect();
                        json!({
                            "type": "string",
                            "enum": enum_values
                        })
                    }
                    wit_parser::TypeDefKind::Option(option_type) => {
                        json!({
                            "anyOf": [
                                Self::wit_type_to_json_schema(*option_type, resolve),
                                {"type": "null"}
                            ]
                        })
                    }
                    wit_parser::TypeDefKind::Result(result_type) => {
                        let mut ok_schema = json!({"type": "null"});
                        let mut err_schema = json!({"type": "null"});

                        if let Some(ok_type) = result_type.ok {
                            ok_schema = Self::wit_type_to_json_schema(ok_type, resolve);
                        }
                        if let Some(err_type) = result_type.err {
                            err_schema = Self::wit_type_to_json_schema(err_type, resolve);
                        }

                        json!({
                            "oneOf": [
                                {
                                    "type": "object",
                                    "properties": {
                                        "ok": ok_schema
                                    },
                                    "required": ["ok"],
                                    "additionalProperties": false
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "error": err_schema
                                    },
                                    "required": ["error"],
                                    "additionalProperties": false
                                }
                            ]
                        })
                    }
                    wit_parser::TypeDefKind::List(element_type) => {
                        json!({
                            "type": "array",
                            "items": Self::wit_type_to_json_schema(*element_type, resolve)
                        })
                    }
                    wit_parser::TypeDefKind::Tuple(tuple) => {
                        let item_schemas: Vec<serde_json::Value> = tuple
                            .types
                            .iter()
                            .map(|t| Self::wit_type_to_json_schema(*t, resolve))
                            .collect();
                        json!({
                            "type": "array",
                            "items": item_schemas,
                            "minItems": item_schemas.len(),
                            "maxItems": item_schemas.len()
                        })
                    }
                    wit_parser::TypeDefKind::Flags(flags) => {
                        json!({
                            "type": "array",
                            "items": {
                                "type": "string",
                                "enum": flags.flags.iter().map(|f| &f.name).collect::<Vec<_>>()
                            },
                            "uniqueItems": true
                        })
                    }
                    wit_parser::TypeDefKind::Resource => {
                        unreachable!("Resource types should be caught by validation")
                    }
                    wit_parser::TypeDefKind::Handle(_) => {
                        unreachable!("Resource handles should be caught by validation")
                    }
                    _ => {
                        unreachable!("Unsupported types should be caught by validation")
                    }
                }
            }
            Type::ErrorContext => json!({"type": "string"}),
        }
    }

    fn has_function_name_conflicts(functions: &[ComponentFunction]) -> bool {
        use std::collections::HashMap;
        let mut function_counts: HashMap<String, u32> = HashMap::new();
        for func in functions {
            *function_counts.entry(func.function.clone()).or_insert(0) += 1;
        }
        function_counts.values().any(|&count| count > 1)
    }

    fn is_optional_type(wit_type: Type, resolve: &Resolve) -> bool {
        match wit_type {
            Type::Id(type_id) => {
                let type_def = resolve
                    .types
                    .get(type_id)
                    .expect("Type definition not found for type ID");
                match &type_def.kind {
                    wit_parser::TypeDefKind::Option(_) => true,
                    wit_parser::TypeDefKind::Type(inner_type) => {
                        Self::is_optional_type(*inner_type, resolve)
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn function_to_component_tool(
        func: ComponentFunction,
        bytes: &[u8],
        requires_disambiguation: bool,
        resolve: &Resolve,
    ) -> Result<ComponentTool> {
        let tool_name = if requires_disambiguation {
            format!("{}_{}_{}", func.component, func.interface, func.function)
        } else {
            format!("{}_{}", func.component, func.function)
        };
        let description = if func.docs.is_empty() {
            format!(
                "Call {} function from {} component",
                func.function, func.component
            )
        } else {
            func.docs
        };

        for param in &func.params {
            Self::validate_wit_type_for_json_rpc(param.wit_type, resolve)?;
        }

        for return_type in &func.returns {
            Self::validate_wit_type_for_json_rpc(*return_type, resolve)?;
        }

        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for param in &func.params {
            let mut param_schema = Self::wit_type_to_json_schema(param.wit_type, resolve);
            if let serde_json::Value::Object(ref mut schema_obj) = param_schema {
                schema_obj.insert(
                    "description".to_string(),
                    serde_json::Value::String(format!("Parameter: {}", param.name)),
                );
            }
            properties.insert(param.name.clone(), param_schema);
            if !Self::is_optional_type(param.wit_type, resolve) {
                required.push(param.name.clone());
            }
        }

        let input_schema = json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        });

        let tool = Tool {
            name: tool_name.into(),
            description: Some(description.into()),
            input_schema: input_schema.as_object().unwrap().clone().into(),
            annotations: None,
        };

        Ok(ComponentTool {
            tool,
            bytes: bytes.to_vec(),
            namespace: func.namespace,
            package: func.package,
            version: func.version,
            interface: func.interface,
            function: func.function,
            params: func.params,
        })
    }
}
