use anyhow::Result;
use wasmtime::{
    Cache, Config, Engine, Store,
    component::{Component, Linker, Type, Val},
};
use wasmtime_wasi::{
    ResourceTable,
    p2::{IoView, WasiCtx, WasiCtxBuilder, WasiView},
};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpView};

use crate::registry::RuntimeFeatureRegistry;
use crate::wit::Function;

pub struct ComponentState {
    pub wasi_ctx: WasiCtx,
    pub wasi_http_ctx: Option<WasiHttpCtx>,
    pub resource_table: ResourceTable,
}

impl IoView for ComponentState {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.resource_table
    }
}

impl WasiView for ComponentState {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.wasi_ctx
    }
}

impl WasiHttpView for ComponentState {
    fn ctx(&mut self) -> &mut WasiHttpCtx {
        self.wasi_http_ctx
            .as_mut()
            .expect("Component requires 'http' feature, so HTTP context should be available")
    }
}

#[derive(Clone)]
pub struct Invoker {
    engine: Engine,
}

impl Invoker {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.cache(Some(Cache::from_file(None)?));
        config.parallel_compilation(true);
        config.async_support(true);
        config.memory_init_cow(true);
        let engine = Engine::new(&config)?;
        Ok(Self { engine })
    }

    fn create_linker(
        &self,
        runtime_features: &[String],
        runtime_feature_registry: &RuntimeFeatureRegistry,
    ) -> Result<Linker<ComponentState>> {
        let mut linker = Linker::new(&self.engine);

        // Add WASI interfaces based on explicitly requested runtime_features
        for feature_name in runtime_features {
            if let Some(runtime_feature) =
                runtime_feature_registry.get_runtime_feature(feature_name)
            {
                match runtime_feature.uri.as_str() {
                    "wasmtime:wasip2" => {
                        // Comprehensive WASI Preview 2 support
                        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
                    }
                    "wasmtime:http" => {
                        wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;
                    }
                    "wasmtime:io" => {
                        wasmtime_wasi_io::add_to_linker_async(&mut linker)?;
                    }
                    "wasmtime:inherit-network" | "wasmtime:allow-ip-name-lookup" => {
                        // These runtime_features are handled in WASI context, not linker
                        // No linker functions to add, only context configuration
                    }
                    _ => {
                        println!(
                            "Unknown runtime feature for linker: {}",
                            runtime_feature.uri
                        );
                    }
                }
            }
            // Component runtime_features are handled during composition, not at runtime
        }
        Ok(linker)
    }

    pub async fn invoke(
        &self,
        bytes: &[u8],
        runtime_features: &[String],
        runtime_feature_registry: &RuntimeFeatureRegistry,
        function: Function,
        args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let component_bytes = bytes.to_vec();

        let interface_str = function.interface().as_str();
        let function_name = function.function_name();
        let linker = self.create_linker(runtime_features, runtime_feature_registry)?;
        let mut wasi_builder = WasiCtxBuilder::new();

        // Process wasmtime runtime_features for WASI context
        for feature_name in runtime_features {
            if let Some(runtime_feature) =
                runtime_feature_registry.get_runtime_feature(feature_name)
            {
                if let Some(wasmtime_feature) = runtime_feature.uri.strip_prefix("wasmtime:") {
                    match wasmtime_feature {
                        "inherit-network" => {
                            wasi_builder.inherit_network();
                        }
                        "allow-ip-name-lookup" => {
                            wasi_builder.allow_ip_name_lookup(true);
                        }
                        "http" => {
                            // HTTP feature only adds linker functions, no WASI context changes
                        }
                        _ => {}
                    }
                }
            }
        }

        let wasi = wasi_builder.build();

        // Check if any feature requires HTTP context
        let needs_http = runtime_features.iter().any(|feature_name| {
            runtime_feature_registry
                .get_runtime_feature(feature_name)
                .and_then(|cap| cap.uri.strip_prefix("wasmtime:"))
                == Some("http")
        });

        let wasi_http_ctx = if needs_http {
            Some(WasiHttpCtx::new())
        } else {
            None
        };
        let state = ComponentState {
            wasi_ctx: wasi,
            wasi_http_ctx,
            resource_table: ResourceTable::new(),
        };
        let mut store = Store::new(&self.engine, state);
        let component = Component::from_binary(&self.engine, &component_bytes)?;
        let instance = linker.instantiate_async(&mut store, &component).await?;

        let interface_export = instance
            .get_export(&mut store, None, interface_str)
            .ok_or_else(|| anyhow::anyhow!("Interface '{}' not found", interface_str))?;
        let parent_export_idx = Some(&interface_export.1);
        let func_export = instance
            .get_export(&mut store, parent_export_idx, function_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Function '{}' not found in interface '{}'",
                    function_name,
                    interface_str
                )
            })?;
        let func = instance
            .get_func(&mut store, func_export.1)
            .ok_or_else(|| anyhow::anyhow!("Function handle invalid for '{}'", function_name))?;

        let mut arg_vals: Vec<Val> = vec![];
        let params = func.params(&store).clone();
        if args.len() != params.len() {
            return Err(anyhow::anyhow!(
                "Wrong number of args: expected {}, got {}",
                params.len(),
                args.len()
            ));
        }
        for (index, json_arg) in args.iter().enumerate() {
            let param_type = &params[index].1;
            let val = json_to_val(json_arg, param_type)
                .map_err(|e| anyhow::anyhow!("Error converting parameter {}: {}", index, e))?;
            arg_vals.push(val);
        }

        let num_results = func.results(&store).len();
        let mut results = vec![Val::Bool(false); num_results];

        func.call_async(&mut store, &arg_vals, &mut results).await?;

        // Handle results according to WIT function signature
        match results.len() {
            0 => Ok(serde_json::Value::Null),
            1 => {
                let value = &results[0];
                match value {
                    Val::Result(Err(Some(error_val))) => {
                        let error_json = val_to_json(error_val);
                        Err(anyhow::anyhow!("Component returned error: {}", error_json))
                    }
                    Val::Result(Err(None)) => Err(anyhow::anyhow!("Component returned error")),
                    _ => Ok(val_to_json(value)),
                }
            }
            _ => {
                // Multiple wasmtime results - reconstruct WIT tuple/record structure
                Self::reconstruct_wit_return(&results, &function)
            }
        }
    }

    // This handles the case where wasmtime decomposes tuples/records into separate Val objects
    fn reconstruct_wit_return(results: &[Val], function: &Function) -> Result<serde_json::Value> {
        // Check if this is a record that needs field mapping to reconstruct as an object
        if let Some(return_schema) = function.result() {
            if let Some(schema_obj) = return_schema.as_object() {
                if schema_obj.get("type").and_then(|t| t.as_str()) == Some("object")
                    && schema_obj.contains_key("properties")
                {
                    return Self::reconstruct_record(results, schema_obj);
                }
            }
        }

        // All other cases (tuples, unknown schemas, malformed schemas) -> array
        let json_results: Vec<serde_json::Value> = results.iter().map(val_to_json).collect();
        Ok(serde_json::Value::Array(json_results))
    }

    // Reconstruct a WIT record from multiple wasmtime results
    fn reconstruct_record(
        results: &[Val],
        schema_obj: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let properties = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .ok_or_else(|| anyhow::anyhow!("Record schema missing properties"))?;

        let mut record = serde_json::Map::new();
        let field_names: Vec<&String> = properties.keys().collect();

        if results.len() != field_names.len() {
            return Err(anyhow::anyhow!(
                "Mismatch between wasmtime results ({}) and record fields ({})",
                results.len(),
                field_names.len()
            ));
        }

        for (i, field_name) in field_names.iter().enumerate() {
            record.insert(field_name.to_string(), val_to_json(&results[i]));
        }

        Ok(serde_json::Value::Object(record))
    }
}

fn json_to_val(json_value: &serde_json::Value, val_type: &Type) -> Result<Val> {
    match (json_value, val_type) {
        // Direct JSON type mappings
        (serde_json::Value::Bool(b), wasmtime::component::Type::Bool) => Ok(Val::Bool(*b)),
        (serde_json::Value::String(s), wasmtime::component::Type::String) => {
            Ok(Val::String(s.clone()))
        }
        (serde_json::Value::String(s), wasmtime::component::Type::Char) => {
            let chars: Vec<char> = s.chars().collect();
            if chars.len() == 1 {
                Ok(Val::Char(chars[0]))
            } else {
                Err(anyhow::anyhow!("Expected single character, got: {}", s))
            }
        }

        // Number types - JSON number maps to all WIT numeric types
        (serde_json::Value::Number(n), wasmtime::component::Type::U8) => {
            let val = n
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("Invalid number for u8: {}", n))?
                as u8;
            Ok(Val::U8(val))
        }
        (serde_json::Value::Number(n), wasmtime::component::Type::U16) => {
            let val = n
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("Invalid number for u16: {}", n))?
                as u16;
            Ok(Val::U16(val))
        }
        (serde_json::Value::Number(n), wasmtime::component::Type::U32) => {
            let val = n
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("Invalid number for u32: {}", n))?
                as u32;
            Ok(Val::U32(val))
        }
        (serde_json::Value::Number(n), wasmtime::component::Type::U64) => {
            let val = n
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("Invalid number for u64: {}", n))?;
            Ok(Val::U64(val))
        }
        (serde_json::Value::Number(n), wasmtime::component::Type::S8) => {
            let val = n
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("Invalid number for s8: {}", n))?
                as i8;
            Ok(Val::S8(val))
        }
        (serde_json::Value::Number(n), wasmtime::component::Type::S16) => {
            let val = n
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("Invalid number for s16: {}", n))?
                as i16;
            Ok(Val::S16(val))
        }
        (serde_json::Value::Number(n), wasmtime::component::Type::S32) => {
            let val = n
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("Invalid number for s32: {}", n))?
                as i32;
            Ok(Val::S32(val))
        }
        (serde_json::Value::Number(n), wasmtime::component::Type::S64) => {
            let val = n
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("Invalid number for s64: {}", n))?;
            Ok(Val::S64(val))
        }
        (serde_json::Value::Number(n), wasmtime::component::Type::Float32) => {
            let val = n
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("Invalid number for f32: {}", n))?
                as f32;
            Ok(Val::Float32(val))
        }
        (serde_json::Value::Number(n), wasmtime::component::Type::Float64) => {
            let val = n
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("Invalid number for f64: {}", n))?;
            Ok(Val::Float64(val))
        }

        // Arrays map to lists
        (serde_json::Value::Array(arr), wasmtime::component::Type::List(list_type)) => {
            let element_type = list_type.ty();
            let mut items = Vec::new();
            for (index, item) in arr.iter().enumerate() {
                items.push(json_to_val(item, &element_type).map_err(|e| {
                    anyhow::anyhow!("Error converting list item at index {}: {}", index, e)
                })?);
            }
            Ok(Val::List(items))
        }

        // Arrays map to tuples
        (serde_json::Value::Array(arr), wasmtime::component::Type::Tuple(tuple_type)) => {
            let tuple_types: Vec<_> = tuple_type.types().collect();
            if arr.len() != tuple_types.len() {
                return Err(anyhow::anyhow!(
                    "Tuple length mismatch: expected {}, got {}",
                    tuple_types.len(),
                    arr.len()
                ));
            }
            let mut items = Vec::new();
            for (index, (item, item_type)) in arr.iter().zip(tuple_types.iter()).enumerate() {
                items.push(json_to_val(item, item_type).map_err(|e| {
                    anyhow::anyhow!("Error converting tuple item at index {}: {}", index, e)
                })?);
            }
            Ok(Val::Tuple(items))
        }

        // Objects map to records
        (serde_json::Value::Object(obj), wasmtime::component::Type::Record(record_type)) => {
            let mut fields = Vec::new();
            for field in record_type.fields() {
                let field_name = field.name.to_string();
                let field_type = &field.ty;

                if let Some(json_value) = obj.get(&field_name) {
                    let field_val = json_to_val(json_value, field_type)?;
                    fields.push((field_name, field_val));
                } else {
                    // Check if field is optional
                    match field_type {
                        wasmtime::component::Type::Option(_) => {
                            fields.push((field_name, Val::Option(None)));
                        }
                        _ => {
                            return Err(anyhow::anyhow!(
                                "Missing required field '{}' in record",
                                field_name
                            ));
                        }
                    }
                }
            }

            // Check for extra fields that aren't in the WIT record
            for (key, _) in obj {
                if !record_type.fields().any(|field| field.name == key) {
                    return Err(anyhow::anyhow!("Unexpected field '{}' in record", key));
                }
            }

            Ok(Val::Record(fields))
        }

        // Handle null for options
        (serde_json::Value::Null, wasmtime::component::Type::Option(_)) => Ok(Val::Option(None)),

        // Handle non-null values for options
        (json_val, wasmtime::component::Type::Option(option_type)) => {
            let inner_type = option_type.ty();
            let inner_val = json_to_val(json_val, &inner_type)?;
            Ok(Val::Option(Some(Box::new(inner_val))))
        }

        // Type mismatches
        _ => Err(anyhow::anyhow!(
            "Type mismatch: cannot convert JSON {:?} to WIT type {:?}",
            json_value,
            val_type
        )),
    }
}

fn val_to_json(val: &Val) -> serde_json::Value {
    match val {
        // Direct mappings
        Val::Bool(b) => serde_json::Value::Bool(*b),
        Val::String(s) => serde_json::Value::String(s.clone()),
        Val::Char(c) => serde_json::Value::String(c.to_string()),

        // All numbers become JSON numbers
        Val::U8(n) => serde_json::Value::Number((*n as u64).into()),
        Val::U16(n) => serde_json::Value::Number((*n as u64).into()),
        Val::U32(n) => serde_json::Value::Number((*n as u64).into()),
        Val::U64(n) => serde_json::Value::Number((*n).into()),
        Val::S8(n) => serde_json::Value::Number((*n as i64).into()),
        Val::S16(n) => serde_json::Value::Number((*n as i64).into()),
        Val::S32(n) => serde_json::Value::Number((*n as i64).into()),
        Val::S64(n) => serde_json::Value::Number((*n).into()),
        Val::Float32(n) => serde_json::Number::from_f64(*n as f64)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Val::Float64(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),

        // Collections
        Val::List(items) => {
            let json_items: Vec<serde_json::Value> = items.iter().map(val_to_json).collect();
            serde_json::Value::Array(json_items)
        }

        Val::Record(fields) => {
            let mut obj = serde_json::Map::new();
            for (name, val) in fields {
                obj.insert(name.clone(), val_to_json(val));
            }
            serde_json::Value::Object(obj)
        }

        // Options
        Val::Option(opt) => match opt {
            Some(val) => val_to_json(val),
            None => serde_json::Value::Null,
        },

        Val::Tuple(vals) => {
            let json_items: Vec<serde_json::Value> = vals.iter().map(val_to_json).collect();
            serde_json::Value::Array(json_items)
        }

        Val::Variant(name, val) => {
            let mut obj = serde_json::Map::new();
            obj.insert("type".to_string(), serde_json::Value::String(name.clone()));
            if let Some(v) = val {
                obj.insert("value".to_string(), val_to_json(v));
            }
            serde_json::Value::Object(obj)
        }

        Val::Enum(variant) => serde_json::Value::String(variant.clone()),

        Val::Flags(items) => {
            let json_items: Vec<serde_json::Value> = items
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect();
            serde_json::Value::Array(json_items)
        }

        Val::Result(result) => {
            let mut obj = serde_json::Map::new();
            match result {
                Ok(Some(v)) => {
                    obj.insert("ok".to_string(), val_to_json(v));
                }
                Ok(None) => {
                    obj.insert("ok".to_string(), serde_json::Value::Null);
                }
                Err(Some(v)) => {
                    obj.insert("error".to_string(), val_to_json(v));
                }
                Err(None) => {
                    obj.insert("error".to_string(), serde_json::Value::Null);
                }
            }
            serde_json::Value::Object(obj)
        }

        Val::Resource(resource_any) => {
            unreachable!(
                "Resource types should be caught by validation: {:?}",
                resource_any
            )
        }
    }
}
