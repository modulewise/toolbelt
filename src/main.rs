use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;

use composable_runtime::{ComponentGraph, Runtime};
use toolbelt::server::ComponentServer;

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

    let mut builder = ComponentGraph::builder();
    for path in &cli.definitions {
        builder = builder.load_file(path);
    }
    let graph = builder.build()?;
    let runtime = Runtime::builder(&graph).build().await?;

    let server = ComponentServer::new(runtime)?;
    server.run(addr).await?;
    Ok(())
}
