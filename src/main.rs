use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;

use composable_runtime::Runtime;
use toolbelt::origin::OriginPolicy;
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

    /// Allowed Origin hostnames. When omitted, defaults to localhost origins
    /// if --host is a loopback address (e.g. for local development), or
    /// denies all Origins otherwise. Use '*' to disable Origin validation.
    #[arg(long, value_delimiter = ',')]
    allowed_origins: Option<Vec<String>>,

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
    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;

    let origin_policy = match &cli.allowed_origins {
        Some(origins) => OriginPolicy::from_cli(origins),
        None => OriginPolicy::default_for_addr(addr.ip()),
    };

    let runtime = Runtime::builder()
        .from_paths(&cli.definitions)
        .build()
        .await?;

    let server = ComponentServer::new(runtime)?;
    server.run(addr, origin_policy).await?;
    Ok(())
}
