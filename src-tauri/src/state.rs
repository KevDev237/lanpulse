//! État partagé entre le moniteur, la découverte des pairs et l'interface.

use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};

use crate::capture::{CaptureShared, CaptureStatus};
use crate::db::{Bytes, Db, PeriodTotals};
use crate::peers::Announce;
use crate::wifi::NetworkInfo;

pub const HISTORY_LEN: usize = 300; // 5 minutes à 1 point/s

#[derive(Debug, Clone, Serialize)]
pub struct Rate {
    pub name: String,
    pub rx: f64,
    pub tx: f64,
}

#[derive(Default)]
pub struct Live {
    pub network: Option<NetworkInfo>,
    pub session_id: Option<i64>,
    pub rate_rx: f64,
    pub rate_tx: f64,
    /// (timestamp, débit descendant, débit montant)
    pub history: VecDeque<(i64, f64, f64)>,
    /// Octets accumulés depuis la dernière écriture en base.
    pub pending: Bytes,
    pub pending_apps: HashMap<String, Bytes>,
    /// Débit lissé par application et par protocole.
    pub app_rates: HashMap<String, (f64, f64)>,
    pub proto_rates: HashMap<String, (f64, f64)>,
    /// Alertes de quota déjà envoyées (clé = réseau + période + date).
    pub alerts_sent: HashSet<String>,
}

pub struct PeerLive {
    pub announce: Announce,
    pub last_seen: i64,
    pub ip: String,
}

pub struct AppState {
    pub device_id: String,
    pub device_name: Mutex<String>,
    pub os: String,
    pub db: Mutex<Db>,
    pub live: Mutex<Live>,
    pub capture: Arc<CaptureShared>,
    pub peers: Mutex<HashMap<String, PeerLive>>,
    pub sock: Option<UdpSocket>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub device_id: String,
    pub device_name: String,
    pub os: String,
    pub network: Option<NetworkInfo>,
    pub rate_rx: f64,
    pub rate_tx: f64,
    pub history: Vec<(i64, f64, f64)>,
    pub apps: Vec<Rate>,
    pub protocols: Vec<Rate>,
    pub capture: CaptureStatus,
    pub totals: PeriodTotals,
    pub peers_online: usize,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn os_label() -> String {
    match std::env::consts::OS {
        "macos" => "macOS".into(),
        "windows" => "Windows".into(),
        "linux" => "Linux".into(),
        o => o.into(),
    }
}

fn sorted_rates(m: &HashMap<String, (f64, f64)>, limit: usize) -> Vec<Rate> {
    let mut v: Vec<Rate> = m
        .iter()
        .map(|(k, (rx, tx))| Rate { name: k.clone(), rx: *rx, tx: *tx })
        .filter(|r| r.rx + r.tx >= 1.0)
        .collect();
    v.sort_by(|a, b| (b.rx + b.tx).partial_cmp(&(a.rx + a.tx)).unwrap_or(std::cmp::Ordering::Equal));
    v.truncate(limit);
    v
}

impl AppState {
    /// Totaux du réseau courant, en incluant les octets pas encore écrits.
    pub fn current_totals(&self) -> PeriodTotals {
        let (net_id, pending) = {
            let l = self.live.lock().unwrap();
            (l.network.as_ref().map(|n| n.id.clone()), l.pending)
        };
        let Some(id) = net_id else { return PeriodTotals::default() };
        let mut t = self.db.lock().unwrap().period_totals(&id);
        t.day.add(pending);
        t.week.add(pending);
        t.month.add(pending);
        if t.session.until.is_none() {
            t.session.rx += pending.rx;
            t.session.tx += pending.tx;
        }
        t
    }

    pub fn snapshot(&self) -> Snapshot {
        let totals = self.current_totals();
        let peers_online = {
            let net = self.live.lock().unwrap().network.as_ref().map(|n| n.id.clone());
            let t = now();
            self.peers
                .lock()
                .unwrap()
                .values()
                .filter(|p| t - p.last_seen <= crate::peers::ONLINE_SECS)
                .filter(|p| Some(&p.announce.network_id) == net.as_ref())
                .count()
        };
        let l = self.live.lock().unwrap();
        Snapshot {
            device_id: self.device_id.clone(),
            device_name: self.device_name.lock().unwrap().clone(),
            os: self.os.clone(),
            network: l.network.clone(),
            rate_rx: l.rate_rx,
            rate_tx: l.rate_tx,
            history: l.history.iter().copied().collect(),
            apps: sorted_rates(&l.app_rates, 25),
            protocols: sorted_rates(&l.proto_rates, 8),
            capture: self.capture.status.lock().unwrap().clone(),
            totals,
            peers_online,
        }
    }
}
