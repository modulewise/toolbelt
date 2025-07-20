use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;

mod capabilities;
mod components;
mod composer;
mod interfaces;
mod resolver;
mod server;

use capabilities::CapabilityRegistry;
use resolver::{resolve_capabilities, resolve_tools};
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

    /// Component paths (.wasm files, .toml config files, or directories)
    #[arg(
        required = true,
        help = "Wasm component paths, TOML config files, or directories"
    )]
    components: Vec<PathBuf>,

    /// Optional server configuration file
    #[arg(
        short = 's',
        long = "server-config",
        help = "Server configuration file (.toml)"
    )]
    server_config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;

    let capability_registry = if let Some(server_config_path) = &cli.server_config {
        resolve_capabilities(server_config_path)?
    } else {
        CapabilityRegistry::empty() // Empty registry - no capabilities available
    };

    let component_specs = resolve_tools(&cli.components, &capability_registry)?;
    let server = ComponentServer::new(component_specs, capability_registry)?;
    server.run(addr).await?;
    Ok(())
}
