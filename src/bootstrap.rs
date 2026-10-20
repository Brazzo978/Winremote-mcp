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
        run_host_with_ui(SocketAddr::new(bind, 8443), 3600, true, true).await
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
    run_host_with_ui(bind, ttl_secs, require_admin, false).await
}

pub async fn run_host_with_ui(
    bind: SocketAddr,
    ttl_secs: u64,
    require_admin: bool,
    show_ui: bool,
) -> Result<()> {
    #[cfg(windows)]
    let _firewall = if !bind.ip().is_loopback() && platform::is_elevated()? {
        Some(FirewallGuard::create(bind)?)
    } else {
        None
    };
    host::run_with_ui(bind, ttl_secs, require_admin, show_ui).await
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
                "$ErrorActionPreference='Stop'; $policy=New-Object -ComObject HNetCfg.FwPolicy2; $rule=New-Object -ComObject HNetCfg.FWRule; $rule.Name=$env:WINREMOTE_RULE; $rule.Description='Temporary Winremote-mcp LAN access'; $rule.Protocol=6; $rule.LocalPorts=$env:WINREMOTE_PORT; $rule.LocalAddresses=$env:WINREMOTE_BIND; $rule.RemoteAddresses='LocalSubnet'; $rule.ApplicationName=$env:WINREMOTE_EXE; $rule.Direction=1; $rule.Action=1; $rule.Profiles=2; $rule.Enabled=$true; $policy.Rules.Add($rule)"])
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
                    "$ErrorActionPreference='Stop'; $policy=New-Object -ComObject HNetCfg.FwPolicy2; $policy.Rules.Remove($env:WINREMOTE_RULE)"])
                .env("WINREMOTE_RULE", &self.name)
                .creation_flags(0x08000000)
                .output();
            if !matches!(result, Ok(output) if output.status.success()) {
                eprintln!("Could not remove firewall rule {}.", self.name);
            }
        }
    }
}

#[cfg(all(test, windows))]
mod firewall_tests {
    use super::*;

    #[test]
    fn loopback_firewall_rule_is_private_scoped_and_removed() {
        use std::os::windows::process::CommandExt;
        if !platform::is_elevated().unwrap_or(false) {
            eprintln!("SKIP firewall lifecycle: requires an elevated test process");
            return;
        }
        // No listener is opened. The rule only targets this test executable on loopback.
        let guard = FirewallGuard::create("127.0.0.1:18443".parse().unwrap()).unwrap();
        let name = guard.name.clone();
        let output = std::process::Command::new(powershell().unwrap())
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop'; $r=(New-Object -ComObject HNetCfg.FwPolicy2).Rules.Item($env:WINREMOTE_RULE); [Console]::WriteLine(('{0}|{1}|{2}|{3}|{4}|{5}|{6}|{7}|{8}' -f $r.Profiles,$r.Protocol,$r.LocalAddresses,$r.LocalPorts,$r.RemoteAddresses,$r.Direction,$r.Action,$r.Enabled,$r.ApplicationName))"])
            .env("WINREMOTE_RULE", &name).creation_flags(0x08000000)
            .output().unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let fields: Vec<_> = text.trim().split('|').collect();
        assert_eq!(fields.len(), 9);
        assert_eq!(&fields[..2], &["2", "6"]);
        assert!(
            matches!(
                fields[2],
                "127.0.0.1" | "127.0.0.1/32" | "127.0.0.1/255.255.255.255"
            ),
            "rule must remain restricted to exactly one loopback address"
        );
        assert_eq!(&fields[3..8], &["18443", "LocalSubnet", "1", "1", "True"]);
        assert!(fields[8].eq_ignore_ascii_case(std::env::current_exe().unwrap().to_str().unwrap()));
        drop(guard);
        let output = std::process::Command::new(powershell().unwrap())
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop'; $p=New-Object -ComObject HNetCfg.FwPolicy2; [Console]::WriteLine(@($p.Rules | Where-Object { $_.Name -eq $env:WINREMOTE_RULE }).Count)"])
            .env("WINREMOTE_RULE", name).creation_flags(0x08000000)
            .output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "0");
    }
}
