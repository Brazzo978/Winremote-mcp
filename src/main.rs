use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::{
    io::{self, BufRead, Read},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};
use winremote_mcp::{bootstrap, connection, mcp, transfer};

#[derive(Parser)]
#[command(
    name = "winremote-mcp",
    version,
    about = "Temporary Windows LAN bridge for MCP agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run on the Windows machine being controlled; inherits this process's privileges.
    Host {
        /// A concrete local IP address. Launch without arguments for automatic LAN selection and UAC.
        #[arg(long, default_value = "127.0.0.1")]
        bind: IpAddr,
        #[arg(long, default_value_t = 8443)]
        port: u16,
        #[arg(long, default_value_t = 3600)]
        ttl_secs: u64,
        /// Require an already elevated process. False permits a deliberately non-admin host.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        require_admin: bool,
    },
    /// Read an invitation from stdin and save a new private connection file.
    Pair {
        #[arg(long)]
        connection: PathBuf,
    },
    /// Upload a local file to the paired Windows host over authenticated HTTPS.
    Upload {
        #[arg(long)]
        connection: PathBuf,
        #[arg(long)]
        local: PathBuf,
        #[arg(long)]
        remote: String,
        #[arg(long)]
        overwrite: bool,
    },
    /// Download a remote file into an atomic local destination.
    Download {
        #[arg(long)]
        connection: PathBuf,
        #[arg(long)]
        local: PathBuf,
        #[arg(long)]
        remote: String,
        #[arg(long)]
        overwrite: bool,
    },
    /// Run the agent-side MCP server over stdio (stdout contains protocol messages only).
    Mcp {
        #[arg(long)]
        connection: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    let interactive_launch = std::env::args_os().len() == 1;
    if let Err(error) = run().await {
        eprintln!("winremote-mcp: {error:#}");
        if interactive_launch {
            eprintln!("Press Enter to close.");
            let mut line = String::new();
            let _ = io::stdin().read_line(&mut line);
        }
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    match Cli::parse().command {
        Some(Commands::Host {
            bind,
            port,
            ttl_secs,
            require_admin,
        }) => {
            if bind.is_unspecified() || bind.is_multicast() {
                bail!("Use a concrete local IP address");
            }
            if port == 0 {
                bail!("Port must be between 1 and 65535");
            }
            if !(1..=86_400).contains(&ttl_secs) {
                bail!("Lifetime must be between 1 and 86400 seconds");
            }
            bootstrap::run_host(SocketAddr::new(bind, port), ttl_secs, require_admin).await?;
        }
        Some(Commands::Pair { connection: path }) => {
            eprintln!(
                "Paste the IP:CODE invitation and press Enter (treat it as an admin credential)."
            );
            let mut invitation = String::new();
            io::stdin()
                .lock()
                .take(32 * 1024 + 1)
                .read_line(&mut invitation)?;
            if invitation.len() > 32 * 1024 {
                bail!("Invitation too large");
            }
            let connection = connection::resolve_invitation(&invitation).await?;
            // Verify connectivity and pinned TLS before storing credentials.
            let response = connection::http_client(&connection)?
                .get(format!(
                    "{}/v1/info",
                    connection.endpoint.trim_end_matches('/')
                ))
                .bearer_auth(&connection.token)
                .send()
                .await
                .context("Cannot connect to the Windows bridge")?;
            if !response.status().is_success() {
                bail!(
                    "Bridge rejected pairing (HTTP {})",
                    response.status().as_u16()
                );
            }
            connection::save(&path, &connection)?;
            eprintln!(
                "Connection saved to {}. It expires with this host invitation.",
                path.display()
            );
        }
        Some(Commands::Upload {
            connection: path,
            local,
            remote,
            overwrite,
        }) => {
            let paired = connection::load(&path)?;
            let result = transfer::upload(&paired, &local, &remote, overwrite).await?;
            println!("{}", serde_json::to_string(&result)?);
        }
        Some(Commands::Download {
            connection: path,
            local,
            remote,
            overwrite,
        }) => {
            let paired = connection::load(&path)?;
            let result = transfer::download(&paired, &local, &remote, overwrite).await?;
            println!("{}", serde_json::to_string(&result)?);
        }
        Some(Commands::Mcp { connection }) => mcp::run(connection).await?,
        None => bootstrap::default_host().await?,
    }
    Ok(())
}
