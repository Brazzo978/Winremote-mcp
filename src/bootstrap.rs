use crate::{host, platform};
use anyhow::{bail, Context, Result};
use std::net::{IpAddr, SocketAddr, UdpSocket};

/// Default double-click path: local UAC consent, LAN discovery, one host with all capabilities.
pub async fn default_host() -> Result<()> {
    #[cfg(not(windows))]
    {
        bail!("The host requires Windows; use mcp on this platform");
    }
    #[cfg(windows)]
    {
        if !platform::is_elevated()? {
            use std::os::windows::process::CommandExt;
            let status = std::process::Command::new(powershell()?)
                .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
                    "$ErrorActionPreference='Stop'; Start-Process -FilePath $env:WINREMOTE_LAUNCH_EXE -Verb RunAs"])
                .env("WINREMOTE_LAUNCH_EXE", std::env::current_exe()?)
                .creation_flags(0x08000000)
                .status().context("Cannot request local UAC consent")?;
            if !status.success() {
                bail!("Administrator consent was declined or elevation failed");
            }
            return Ok(());
        }
        let bind = discover_lan_ip()?;
        eprintln!(
            "Starting Windows bridge on {bind}:8443. Close the console or press Ctrl+C to stop."
        );
        run_host(SocketAddr::new(bind, 8443), 3600, true).await
    }
}

fn discover_lan_ip() -> Result<IpAddr> {
    // UDP connect selects a local route; no packet is sent.
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket
        .connect("192.0.2.1:9")
        .context("No default IPv4 route; use host --bind YOUR_LAN_IP")?;
    let ip = socket.local_addr()?.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        bail!("No usable LAN IP; use host --bind YOUR_LAN_IP");
    }
    Ok(ip)
}

pub async fn run_host(bind: SocketAddr, ttl_secs: u64, require_admin: bool) -> Result<()> {
    #[cfg(windows)]
    let _firewall = if !bind.ip().is_loopback() && platform::is_elevated()? {
        Some(FirewallGuard::create(bind)?)
    } else {
        None
    };
    host::run(bind, ttl_secs, require_admin).await
}

#[cfg(windows)]
fn powershell() -> Result<std::path::PathBuf> {
    let root = std::env::var_os("SystemRoot").context("SystemRoot is missing")?;
    Ok(std::path::PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe"))
}

#[cfg(windows)]
struct FirewallGuard {
    name: String,
}

#[cfg(windows)]
impl FirewallGuard {
    fn create(bind: SocketAddr) -> Result<Self> {
        use rand::Rng;
        use std::os::windows::process::CommandExt;
        let name = format!(
            "Winremote-mcp-{}-{:016x}",
            std::process::id(),
            rand::thread_rng().gen::<u64>()
        );
        let output = std::process::Command::new(powershell()?)
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop'; New-NetFirewallRule -Name $env:WINREMOTE_RULE -DisplayName $env:WINREMOTE_RULE -Direction Inbound -Action Allow -Protocol TCP -LocalAddress $env:WINREMOTE_BIND -LocalPort $env:WINREMOTE_PORT -RemoteAddress LocalSubnet -Profile Private -Program $env:WINREMOTE_EXE | Out-Null"])
            .env("WINREMOTE_RULE", &name)
            .env("WINREMOTE_BIND", bind.ip().to_string())
            .env("WINREMOTE_PORT", bind.port().to_string())
            .env("WINREMOTE_EXE", std::env::current_exe()?)
            .creation_flags(0x08000000)
            .output().context("Cannot configure the temporary LAN firewall rule")?;
        if !output.status.success() {
            bail!("Cannot configure the LAN firewall rule; check Windows firewall policy");
        }
        eprintln!(
            "Temporary firewall access: Private networks, LocalSubnet, TCP {}.",
            bind.port()
        );
        Ok(Self { name })
    }
}
#[cfg(windows)]
impl Drop for FirewallGuard {
    fn drop(&mut self) {
        use std::os::windows::process::CommandExt;
        if let Ok(shell) = powershell() {
            let result = std::process::Command::new(shell)
                .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
                    "$ErrorActionPreference='Stop'; Remove-NetFirewallRule -Name $env:WINREMOTE_RULE -ErrorAction SilentlyContinue"])
                .env("WINREMOTE_RULE", &self.name)
                .creation_flags(0x08000000)
                .output();
            if !matches!(result, Ok(output) if output.status.success()) {
                eprintln!("Could not remove firewall rule {}.", self.name);
            }
        }
    }
}
