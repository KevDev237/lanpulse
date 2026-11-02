//! Capture des paquets sur l'interface par défaut et attribution du trafic
//! aux processus (couches 3 et 4 : IPv4/IPv6, TCP/UDP).
//!
//! Principe : chaque paquet capturé est décodé jusqu'aux ports TCP/UDP. Le
//! port local est ensuite rapproché de la table des sockets du système
//! (netstat2) pour retrouver le PID propriétaire. Les octets sont cumulés
//! par PID et par protocole, puis relevés chaque seconde par le moniteur.
//!
//! La capture exige des droits élevés (root/sudo, ou capacité CAP_NET_RAW
//! sous Linux, groupe access_bpf sous macOS, Npcap sous Windows). Sans ces
//! droits, l'application continue de fonctionner avec les totaux par
//! interface ; seule la vue par application est indisponible.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::db::Bytes;

#[derive(Debug, Clone, Serialize, Default)]
pub struct CaptureStatus {
    pub active: bool,
    pub message: String,
    pub interface: Option<String>,
}

#[derive(Default)]
pub struct CaptureShared {
    /// Octets par nom d'application depuis le dernier relevé.
    pub per_app: Mutex<HashMap<String, Bytes>>,
    pub per_proto: Mutex<HashMap<String, Bytes>>,
    pub status: Mutex<CaptureStatus>,
}

impl CaptureShared {
    pub fn drain(&self) -> (HashMap<String, Bytes>, HashMap<String, Bytes>) {
        let a = std::mem::take(&mut *self.per_app.lock().unwrap());
        let b = std::mem::take(&mut *self.per_proto.lock().unwrap());
        (a, b)
    }
    fn set_status(&self, active: bool, message: impl Into<String>, iface: Option<String>) {
        *self.status.lock().unwrap() = CaptureStatus {
            active,
            message: message.into(),
            interface: iface,
        };
    }
}

/// Libellé du trafic dont le processus n'a pas pu être identifié.
pub const UNKNOWN_APP: &str = "Système / non identifié";

#[derive(Debug)]
struct Decoded {
    proto: u8,
    src: std::net::IpAddr,
    dst: std::net::IpAddr,
    sport: u16,
    dport: u16,
}

fn decode_ip(b: &[u8]) -> Option<Decoded> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let version = b.first()? >> 4;
    let (proto, src, dst, l4) = match version {
        4 => {
            if b.len() < 20 {
                return None;
            }
            let ihl = ((b[0] & 0x0f) as usize) * 4;
            let src = IpAddr::V4(Ipv4Addr::new(b[12], b[13], b[14], b[15]));
            let dst = IpAddr::V4(Ipv4Addr::new(b[16], b[17], b[18], b[19]));
            (b[9], src, dst, b.get(ihl..).unwrap_or(&[]))
        }
        6 => {
            if b.len() < 40 {
                return None;
            }
            let mut s = [0u8; 16];
            let mut d = [0u8; 16];
            s.copy_from_slice(&b[8..24]);
            d.copy_from_slice(&b[24..40]);
            (b[6], IpAddr::V6(Ipv6Addr::from(s)), IpAddr::V6(Ipv6Addr::from(d)), &b[40..])
        }
        _ => return None,
    };
    let (sport, dport) = if (proto == 6 || proto == 17) && l4.len() >= 4 {
        (
            u16::from_be_bytes([l4[0], l4[1]]),
            u16::from_be_bytes([l4[2], l4[3]]),
        )
    } else {
        (0, 0)
    };
    Some(Decoded { proto, src, dst, sport, dport })
}

/// Retire l'en-tête de couche 2 selon le type de lien pcap.
fn strip_link(linktype: i32, data: &[u8]) -> Option<&[u8]> {
    match linktype {
        1 => {
            // Ethernet, avec prise en charge d'une étiquette VLAN 802.1Q
            let mut off = 12;
            let mut et = u16::from_be_bytes([*data.get(off)?, *data.get(off + 1)?]);
            if et == 0x8100 {
                off += 4;
                et = u16::from_be_bytes([*data.get(off)?, *data.get(off + 1)?]);
            }
            if et == 0x0800 || et == 0x86DD {
                data.get(off + 2..)
            } else {
                None
            }
        }
        0 | 108 => data.get(4..), // BSD loopback / utun (macOS)
        12 | 14 | 101 | 228 | 229 => Some(data), // IP brut
        113 => data.get(16..),    // Linux cooked v1
        276 => data.get(20..),    // Linux cooked v2
        _ => None,
    }
}

fn proto_name(p: u8) -> &'static str {
    match p {
        6 => "TCP",
        17 => "UDP",
        1 | 58 => "ICMP",
        _ => "Autre",
    }
}

#[cfg(feature = "capture")]
struct SockEntry {
    key: (u8, u16),
    remote: Option<(std::net::IpAddr, u16)>,
    pid: u32,
}

#[cfg(feature = "capture")]
fn socket_table() -> Vec<SockEntry> {
    use netstat2::*;
    let mut out = Vec::new();
    let af = AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6;
    let pf = ProtocolFlags::TCP | ProtocolFlags::UDP;
    if let Ok(socks) = get_sockets_info(af, pf) {
        for s in socks {
            let Some(pid) = s.associated_pids.first().copied() else { continue };
            match s.protocol_socket_info {
                ProtocolSocketInfo::Tcp(t) => {
                    let remote = (t.remote_port != 0 && !t.remote_addr.is_unspecified())
                        .then_some((t.remote_addr, t.remote_port));
                    out.push(SockEntry { key: (6, t.local_port), remote, pid });
                }
                ProtocolSocketInfo::Udp(u) => {
                    out.push(SockEntry { key: (17, u.local_port), remote: None, pid });
                }
            }
        }
    }
    out
}

#[cfg(windows)]
fn ensure_npcap() -> Result<(), String> {
    use windows_sys::Win32::System::LibraryLoader::{LoadLibraryW, SetDllDirectoryW};
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let dir = format!("{root}\\System32\\Npcap");
    let wide: Vec<u16> = dir.encode_utf16().chain(std::iter::once(0)).collect();
    let dll: Vec<u16> = "wpcap.dll".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        SetDllDirectoryW(wide.as_ptr());
        if LoadLibraryW(dll.as_ptr()).is_null() {
            return Err("Npcap n'est pas installé (https://npcap.com). La vue par application est désactivée.".into());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn ensure_npcap() -> Result<(), String> {
    Ok(())
}

#[cfg(feature = "capture")]
fn find_device(iface: &str) -> Option<pcap::Device> {
    let devs = pcap::Device::list().ok()?;
    // Sous Windows, le nom pcap est \Device\NPF_{GUID} et netdev donne {GUID}.
    devs.into_iter()
        .find(|d| d.name == iface || d.name.ends_with(iface) || d.name.contains(iface))
}

#[cfg(feature = "capture")]
pub fn spawn(shared: Arc<CaptureShared>) {
    std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || loop {
            if let Err(e) = ensure_npcap() {
                shared.set_status(false, e, None);
                std::thread::sleep(std::time::Duration::from_secs(60));
                continue;
            }
            let iface = match netdev::get_default_interface() {
                Ok(i) => i,
                Err(_) => {
                    shared.set_status(false, "Aucune interface réseau active", None);
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    continue;
                }
            };
            if let Err(e) = run_capture(&shared, &iface) {
                shared.set_status(false, e, Some(iface.name.clone()));
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        })
        .expect("thread capture");
}

fn credit(shared: &CaptureShared, app: &str, delta: Bytes) {
    let mut m = shared.per_app.lock().unwrap();
    if let Some(b) = m.get_mut(app) {
        b.add(delta);
    } else {
        m.insert(app.to_string(), delta);
    }
}

type Key = (u8, u16);
type Remote = (std::net::IpAddr, u16);

/// Rapproche chaque paquet du processus propriétaire du socket.
///
/// Le nom du processus est résolu dès que le socket apparaît (un processus
/// bref peut s'être terminé avant le relevé suivant). Les octets d'un port
/// encore absent de la table sont mis en attente, puis attribués dès que le
/// socket apparaît ; à défaut, au processus qui parlait récemment au même
/// serveur distant ; à défaut, à « Système / non identifié ».
#[cfg(feature = "capture")]
struct Attributor {
    sys: sysinfo::System,
    names: HashMap<u32, String>,
    sockets: HashMap<Key, (String, std::time::Instant)>,
    remotes: HashMap<Remote, (String, std::time::Instant)>,
    limbo: HashMap<Key, (Bytes, std::time::Instant, Remote)>,
    last_merge: std::time::Instant,
}

#[cfg(feature = "capture")]
impl Attributor {
    const KEEP: std::time::Duration = std::time::Duration::from_secs(30);
    const LIMBO: std::time::Duration = std::time::Duration::from_secs(3);
    const MIN_REFRESH: std::time::Duration = std::time::Duration::from_millis(100);

    fn new() -> Self {
        let mut a = Self {
            sys: sysinfo::System::new(),
            names: HashMap::new(),
            sockets: HashMap::new(),
            remotes: HashMap::new(),
            limbo: HashMap::new(),
            last_merge: std::time::Instant::now(),
        };
        a.refresh_table();
        a
    }

    fn refresh_table(&mut self) {
        let t = std::time::Instant::now();
        let table = socket_table();
        let missing: Vec<sysinfo::Pid> = table
            .iter()
            .filter(|e| !self.names.contains_key(&e.pid))
            .map(|e| sysinfo::Pid::from_u32(e.pid))
            .collect();
        if !missing.is_empty() {
            self.sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&missing), true);
            for pid in missing {
                if let Some(p) = self.sys.process(pid) {
                    self.names.insert(pid.as_u32(), p.name().to_string_lossy().to_string());
                }
            }
        }
        for e in table {
            if let Some(n) = self.names.get(&e.pid) {
                self.sockets.insert(e.key, (n.clone(), t));
                if let Some(r) = e.remote {
                    self.remotes.insert(r, (n.clone(), t));
                }
            }
        }
        self.sockets.retain(|_, (_, seen)| seen.elapsed() < Self::KEEP);
        self.remotes.retain(|_, (_, seen)| seen.elapsed() < Self::KEEP);
        if self.names.len() > 4000 {
            self.names.clear(); // les PID sont recyclés par le système
        }
        self.last_merge = t;
    }

    /// Relit la table des sockets et attribue les octets en attente.
    fn merge(&mut self, shared: &CaptureShared) {
        self.refresh_table();
        let (sockets, remotes) = (&self.sockets, &self.remotes);
        self.limbo.retain(|k, (b, since, remote)| {
            if let Some((name, _)) = sockets.get(k) {
                credit(shared, name, *b);
                false
            } else if since.elapsed() > Self::LIMBO {
                let app = remotes.get(remote).map(|(n, _)| n.as_str()).unwrap_or(UNKNOWN_APP);
                credit(shared, app, *b);
                false
            } else {
                true
            }
        });
    }

    fn packet(&mut self, shared: &CaptureShared, key: Key, remote: Remote, delta: Bytes) {
        if !self.sockets.contains_key(&key) && self.last_merge.elapsed() > Self::MIN_REFRESH {
            self.merge(shared);
        }
        match self.sockets.get(&key) {
            Some((name, _)) => {
                credit(shared, name, delta);
                self.remotes.insert(remote, (name.clone(), std::time::Instant::now()));
            }
            None => self
                .limbo
                .entry(key)
                .or_insert((Bytes::default(), std::time::Instant::now(), remote))
                .0
                .add(delta),
        }
    }
}

#[cfg(feature = "capture")]
fn run_capture(shared: &CaptureShared, iface: &netdev::Interface) -> Result<(), String> {
    use std::collections::HashSet;
    use std::net::IpAddr;
    use std::time::{Duration, Instant};

    let dev = find_device(&iface.name)
        .ok_or_else(|| format!("Interface {} introuvable pour la capture", iface.name))?;
    let mut cap = pcap::Capture::from_device(dev)
        .map_err(|e| e.to_string())?
        .promisc(false)
        .snaplen(128)
        .buffer_size(16 * 1024 * 1024)
        .timeout(500)
        .immediate_mode(true)
        .open()
        .map_err(|e| {
            let msg = e.to_string();
            if msg.to_lowercase().contains("permission") || msg.contains("Operation not permitted") {
                "Droits insuffisants pour capturer les paquets. Lancez LanPulse en administrateur \
                 (ou voir le README pour accorder la capacité de capture)."
                    .to_string()
            } else {
                format!("Capture impossible : {msg}")
            }
        })?;
    let linktype = cap.get_datalink().0;

    let local: HashSet<IpAddr> = iface
        .ipv4
        .iter()
        .map(|n| IpAddr::V4(n.addr()))
        .chain(iface.ipv6.iter().map(|n| IpAddr::V6(n.addr())))
        .collect();

    shared.set_status(true, "Capture active", Some(iface.name.clone()));

    let mut attr = Attributor::new();
    let mut last_iface_check = Instant::now();

    loop {
        if attr.last_merge.elapsed() > Duration::from_secs(2) {
            attr.merge(shared);
        }
        if last_iface_check.elapsed() > Duration::from_secs(5) {
            last_iface_check = Instant::now();
            if let Ok(st) = cap.stats() {
                let msg = if st.dropped > 0 {
                    format!("Capture active ({} paquets non analysés, trafic très élevé)", st.dropped)
                } else {
                    "Capture active".to_string()
                };
                shared.set_status(true, msg, Some(iface.name.clone()));
            }
            match netdev::get_default_interface() {
                Ok(i) if i.name == iface.name && i.ipv4 == iface.ipv4 => {}
                _ => return Ok(()), // changement de réseau : on relance la capture
            }
        }
        let pkt = match cap.next_packet() {
            Ok(p) => p,
            Err(pcap::Error::TimeoutExpired) => continue,
            Err(e) => return Err(format!("Capture interrompue : {e}")),
        };
        let len = pkt.header.len as u64;
        let Some(ip) = strip_link(linktype, pkt.data) else { continue };
        let Some(d) = decode_ip(ip) else { continue };

        let (outgoing, lport) = if local.contains(&d.src) {
            (true, d.sport)
        } else {
            (false, d.dport)
        };
        let delta = if outgoing { Bytes { rx: 0, tx: len } } else { Bytes { rx: len, tx: 0 } };
        if d.proto == 6 || d.proto == 17 {
            let remote = if outgoing { (d.dst, d.dport) } else { (d.src, d.sport) };
            attr.packet(shared, (d.proto, lport), remote, delta);
        } else {
            credit(shared, UNKNOWN_APP, delta);
        }
        shared
            .per_proto
            .lock()
            .unwrap()
            .entry(proto_name(d.proto).to_string())
            .or_default()
            .add(delta);
    }
}

#[cfg(not(feature = "capture"))]
pub fn spawn(shared: Arc<CaptureShared>) {
    shared.set_status(false, "Compilé sans la capture de paquets", None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_ipv4_tcp() {
        // En-tête Ethernet + IPv4 + TCP minimal
        let mut f = vec![0u8; 14];
        f[12] = 0x08;
        f[13] = 0x00;
        let mut ip = vec![0u8; 20];
        ip[0] = 0x45;
        ip[9] = 6;
        ip[12..16].copy_from_slice(&[192, 168, 1, 10]);
        ip[16..20].copy_from_slice(&[8, 8, 8, 8]);
        f.extend(ip);
        f.extend([0x1F, 0x90, 0x01, 0xBB]); // 8080 -> 443
        let l3 = strip_link(1, &f).unwrap();
        let d = decode_ip(l3).unwrap();
        assert_eq!(d.proto, 6);
        assert_eq!(d.sport, 8080);
        assert_eq!(d.dport, 443);
        assert_eq!(d.src.to_string(), "192.168.1.10");
    }

    #[test]
    fn decode_ipv6_udp() {
        let mut ip = vec![0u8; 40];
        ip[0] = 0x60;
        ip[6] = 17;
        ip[23] = 1;
        ip[39] = 2;
        ip.extend([0x00, 0x35, 0xC0, 0x00]);
        let d = decode_ip(&ip).unwrap();
        assert_eq!(d.proto, 17);
        assert_eq!(d.sport, 53);
        assert_eq!(proto_name(d.proto), "UDP");
    }
}
