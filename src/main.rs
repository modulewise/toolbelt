use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;

mod components;
mod resolver;
mod server;

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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;
    let component_specs = resolve_components(&cli.components)?;
    let server = ComponentServer::new(component_specs)?;
    server.run(addr).await?;
    Ok(())
}
