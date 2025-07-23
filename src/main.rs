use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;

mod composer;
mod interfaces;
mod loader;
mod registry;
mod runtime;
mod server;

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

    /// Capability definition files (.toml files with [capabilityname] sections)
    #[arg(
        short = 'c',
        long = "capabilities",
        help = "Capability definition files"
    )]
    capabilities: Vec<PathBuf>,

    /// Tool definition files (.toml files with [toolname] sections)
    #[arg(short = 't', long = "tools", help = "Tool definition files")]
    tools: Vec<PathBuf>,

    /// Multi-definition files and standalone .wasm files
    #[arg(
        help = "Multi-definition files (.toml with [capabilities.*] and [tools.*]) and standalone .wasm files"
    )]
    definitions_and_wasm: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;

    let (capability_definitions, tool_definitions) =
        load_definitions(&cli.capabilities, &cli.tools, &cli.definitions_and_wasm)?;
    let (capability_registry, tool_registry) =
        build_registries(capability_definitions, tool_definitions)?;

    let server = ComponentServer::new(capability_registry, tool_registry)?;
    server.run(addr).await?;
    Ok(())
}
