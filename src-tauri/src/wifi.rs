//! Détection du réseau courant : interface par défaut, passerelle, SSID Wi-Fi.
//!
//! L'identifiant du réseau est construit à partir de l'adresse MAC de la
//! passerelle : deux machines connectées au même routeur obtiennent le même
//! identifiant, même si le SSID n'est pas lisible (macOS sans autorisation de
//! localisation, réseau filaire, etc.).

use serde::Serialize;
use std::process::Command;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NetworkInfo {
    /// Identifiant stable partagé par toutes les machines du même réseau.
    pub id: String,
    /// Nom lisible (SSID si disponible).
    pub name: String,
    pub ssid: Option<String>,
    pub kind: String, // "wifi" | "ethernet" | "autre"
    pub interface: String,
    pub local_ipv4: Option<String>,
    pub broadcast_ipv4: Option<String>,
    pub gateway_ip: Option<String>,
    pub gateway_mac: Option<String>,
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let mut c = Command::new(cmd);
    c.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = c.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

fn clean_ssid(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("<redacted>") || s.contains("not associated") {
        None
    } else {
        Some(s.to_string())
    }
}

#[cfg(target_os = "linux")]
fn current_ssid(_iface: &str) -> Option<String> {
    if let Some(out) = run("nmcli", &["-t", "-f", "ACTIVE,SSID", "dev", "wifi"]) {
        for line in out.lines() {
            if let Some(rest) = line.strip_prefix("yes:").or_else(|| line.strip_prefix("oui:")) {
                if let Some(s) = clean_ssid(&rest.replace("\\:", ":")) {
                    return Some(s);
                }
            }
        }
    }
    run("iwgetid", &["-r"]).and_then(|s| clean_ssid(&s))
}

#[cfg(target_os = "macos")]
fn current_ssid(iface: &str) -> Option<String> {
    if let Some(out) = run("ipconfig", &["getsummary", iface]) {
        for line in out.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("SSID :") {
                if let Some(s) = clean_ssid(rest) {
                    return Some(s);
                }
            }
        }
    }
    run("networksetup", &["-getairportnetwork", iface]).and_then(|out| {
        out.split_once(':').and_then(|(_, s)| clean_ssid(s))
    })
}

#[cfg(target_os = "windows")]
fn current_ssid(_iface: &str) -> Option<String> {
    let out = run("netsh", &["wlan", "show", "interfaces"])?;
    for line in out.lines() {
        let t = line.trim();
        if t.starts_with("SSID") && !t.starts_with("BSSID") {
            if let Some((_, v)) = t.split_once(':') {
                if let Some(s) = clean_ssid(v) {
                    return Some(s);
                }
            }
        }
    }
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn current_ssid(_iface: &str) -> Option<String> {
    None
}

pub fn detect() -> Option<NetworkInfo> {
    let iface = netdev::get_default_interface().ok()?;
    let ssid = current_ssid(&iface.name);

    let kind = match iface.if_type {
        netdev::interface::InterfaceType::Wireless80211 => "wifi",
        netdev::interface::InterfaceType::Ethernet => {
            if ssid.is_some() { "wifi" } else { "ethernet" }
        }
        _ => {
            if ssid.is_some() { "wifi" } else { "autre" }
        }
    }
    .to_string();

    let v4 = iface.ipv4.first();
    let local_ipv4 = v4.map(|n| n.addr().to_string());
    let broadcast_ipv4 = v4.map(|n| n.broadcast().to_string());

    let (gateway_ip, gateway_mac) = match &iface.gateway {
        Some(gw) => {
            let mac = gw.mac_addr.to_string().to_lowercase();
            let mac = if mac == "00:00:00:00:00:00" { None } else { Some(mac) };
            (gw.ipv4.first().map(|ip| ip.to_string()), mac)
        }
        None => (None, None),
    };

    let id = if let Some(m) = &gateway_mac {
        format!("gw-{}", m.replace(':', ""))
    } else if let Some(s) = &ssid {
        format!("ssid-{s}")
    } else {
        format!(
            "if-{}-{}",
            iface.name,
            gateway_ip.clone().unwrap_or_else(|| "sans-passerelle".into())
        )
    };

    let name = match (&ssid, kind.as_str()) {
        (Some(s), _) => s.clone(),
        (None, "ethernet") => format!(
            "Réseau filaire ({})",
            gateway_ip.clone().unwrap_or_else(|| iface.name.clone())
        ),
        _ => format!(
            "Réseau {}",
            gateway_ip.clone().unwrap_or_else(|| iface.name.clone())
        ),
    };

    Some(NetworkInfo {
        id,
        name,
        ssid,
        kind,
        interface: iface.name,
        local_ipv4,
        broadcast_ipv4,
        gateway_ip,
        gateway_mac,
    })
}
