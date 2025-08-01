use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;

mod composer;
mod loader;
mod mapper;
mod registry;
mod runtime;
mod server;
mod wit;

use loader::load_definitions;
use registry::build_registries;
use server::ComponentServer;

#[derive(Parser)]
#[command(name = "toolbelt")]
#[command(about = "Modulewise Toolbelt is an MCP Server for Wasm Components")]
struct Cli {
    /// Host to bind to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to bind to
    #[arg(short, long, default_value_t = 3001)]
    port: u16,

    /// Component definition files (.toml) and standalone .wasm files
    #[arg(help = "Component definition files (.toml) and standalone .wasm files")]
    definitions: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;

    let (runtime_feature_definitions, component_definitions) = load_definitions(&cli.definitions)?;
    let (runtime_feature_registry, component_registry) =
        build_registries(runtime_feature_definitions, component_definitions).await?;

    let server = ComponentServer::new(runtime_feature_registry, component_registry)?;
    server.run(addr).await?;
    Ok(())
}
