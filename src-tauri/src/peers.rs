//! Découverte et partage entre machines du même réseau local.
//!
//! Chaque instance diffuse toutes les 5 secondes une annonce UDP (broadcast)
//! contenant son identité, l'identifiant du réseau et ses totaux de
//! consommation par période. Les instances qui reçoivent l'annonce et sont
//! sur le même réseau affichent la machine émettrice comme pair.

use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use crate::db::PeriodTotals;

pub const PORT: u16 = 47321;
pub const APP_TAG: &str = "lanpulse";
/// Un pair est considéré en ligne s'il s'est annoncé dans ce délai.
pub const ONLINE_SECS: i64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Announce {
    pub app: String,
    pub v: u8,
    pub device_id: String,
    pub device_name: String,
    pub os: String,
    pub network_id: String,
    pub network_name: String,
    /// Débit instantané en octets/s.
    pub rate_rx: f64,
    pub rate_tx: f64,
    pub totals: PeriodTotals,
    pub ts: i64,
}

pub fn bind() -> std::io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    let u: UdpSocket = s.into();
    u.set_read_timeout(Some(Duration::from_secs(1)))?;
    Ok(u)
}

/// Envoie l'annonce en broadcast global et sur l'adresse de diffusion du
/// sous-réseau (certains systèmes ignorent 255.255.255.255).
pub fn send(sock: &UdpSocket, a: &Announce, subnet_broadcast: Option<&str>) {
    let Ok(payload) = serde_json::to_vec(a) else { return };
    let _ = sock.send_to(&payload, (Ipv4Addr::BROADCAST, PORT));
    if let Some(b) = subnet_broadcast.and_then(|s| s.parse::<Ipv4Addr>().ok()) {
        if b != Ipv4Addr::BROADCAST {
            let _ = sock.send_to(&payload, (b, PORT));
        }
    }
}

pub fn parse(buf: &[u8]) -> Option<Announce> {
    let a: Announce = serde_json::from_slice(buf).ok()?;
    if a.app != APP_TAG || a.device_id.is_empty() || a.device_name.len() > 128 {
        return None;
    }
    Some(a)
}
