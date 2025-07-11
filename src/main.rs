use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;

mod capabilities;
mod components;
mod resolver;
mod server;

use capabilities::CapabilityRegistry;
use resolver::resolve_components;
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

    // Load capability registry from server config (if provided)
    let capability_registry = if let Some(server_config_path) = &cli.server_config {
        CapabilityRegistry::from_config_file(server_config_path)?
    } else {
        CapabilityRegistry::new() // Empty registry - no capabilities available
    };

    let component_specs = resolve_components(&cli.components)?;
    let server = ComponentServer::new(component_specs, capability_registry)?;
    server.run(addr).await?;
    Ok(())
}
