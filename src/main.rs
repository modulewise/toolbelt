use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

use composable_mcp::McpGatewayService;
use composable_runtime::Runtime;

#[derive(Parser)]
#[command(name = "toolbelt")]
#[command(about = "Modulewise Toolbelt is an MCP Server for Wasm Components")]
struct Cli {
    /// Component definition files (.toml) and standalone .wasm files
    #[arg(help = "Component definition files (.toml) and standalone .wasm files")]
    definitions: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let runtime = Runtime::builder()
        .from_paths(&cli.definitions)
        .with_service::<McpGatewayService>()
        .build()
        .await?;

    runtime.run().await
}
