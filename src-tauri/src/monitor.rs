//! Boucle principale (1 tick/s) : mesure du débit, suivi du réseau courant,
//! écriture en base, quotas, annonces aux pairs et mise à jour de l'interface.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sysinfo::Networks;
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;

use crate::db::{today, Bytes};
use crate::peers::{self, Announce};
use crate::state::{now, AppState, HISTORY_LEN};
use crate::wifi;

const EMA: f64 = 0.5;

fn smooth(map: &mut HashMap<String, (f64, f64)>, sample: &HashMap<String, Bytes>, dt: f64) {
    for v in map.values_mut() {
        v.0 *= 1.0 - EMA;
        v.1 *= 1.0 - EMA;
    }
    for (k, b) in sample {
        let e = map.entry(k.clone()).or_insert((0.0, 0.0));
        e.0 += EMA * b.rx as f64 / dt;
        e.1 += EMA * b.tx as f64 / dt;
    }
    map.retain(|_, v| v.0 + v.1 >= 0.5);
}

/// Écrit en base les octets en attente du réseau courant.
pub fn flush(state: &AppState) {
    let (net_id, sid, pending, apps) = {
        let mut l = state.live.lock().unwrap();
        let Some(n) = l.network.as_ref() else { return };
        let id = n.id.clone();
        let sid = l.session_id;
        let p = std::mem::take(&mut l.pending);
        let a = std::mem::take(&mut l.pending_apps);
        (id, sid, p, a)
    };
    if let Err(e) = state.db.lock().unwrap().flush(&net_id, sid, pending, &apps, now()) {
        eprintln!("[lanpulse] écriture base : {e}");
    }
}

/// Gère un éventuel changement de réseau. Retourne vrai si le réseau a changé.
fn update_network(state: &AppState) -> bool {
    let detected = wifi::detect();
    let current_id = state.live.lock().unwrap().network.as_ref().map(|n| n.id.clone());
    let t = now();
    match (&detected, current_id) {
        (Some(n), Some(old)) if n.id == old => {
            let _ = state.db.lock().unwrap().upsert_network(n, t);
            state.live.lock().unwrap().network = Some(n.clone());
            false
        }
        _ => {
            // Fin de la session précédente
            flush(state);
            let old_sid = state.live.lock().unwrap().session_id.take();
            if let Some(sid) = old_sid {
                let _ = state.db.lock().unwrap().close_session(sid, t);
            }
            let mut sid = None;
            if let Some(n) = &detected {
                let db = state.db.lock().unwrap();
                let _ = db.upsert_network(n, t);
                sid = db.open_session(&n.id, t).ok();
            }
            let mut l = state.live.lock().unwrap();
            l.network = detected;
            l.session_id = sid;
            l.app_rates.clear();
            l.proto_rates.clear();
            true
        }
    }
}

fn check_quotas(app: &AppHandle, state: &AppState) {
    let Some((id, name)) = state
        .live
        .lock()
        .unwrap()
        .network
        .as_ref()
        .map(|n| (n.id.clone(), n.name.clone()))
    else {
        return;
    };
    let (daily, monthly) = state.db.lock().unwrap().quotas(&id);
    let totals = state.current_totals();
    let day = today();
    let checks = [
        ("jour", daily, totals.day.rx + totals.day.tx, day.clone()),
        ("mois", monthly, totals.month.rx + totals.month.tx, day[..7].to_string()),
    ];
    for (label, quota, used, key_date) in checks {
        let Some(q) = quota else { continue };
        let limit = q * 1024 * 1024;
        for (pct, tag) in [(80u64, "80"), (100u64, "100")] {
            if used * 100 >= limit * pct {
                let key = format!("{id}|{label}|{tag}|{key_date}");
                let mut l = state.live.lock().unwrap();
                if l.alerts_sent.insert(key) {
                    drop(l);
                    let body = if pct >= 100 {
                        format!("Quota du {label} atteint sur « {name} » ({q} Mo).")
                    } else {
                        format!("80 % du quota du {label} consommés sur « {name} » ({q} Mo).")
                    };
                    let _ = app.notification().builder().title("LanPulse").body(body).show();
                }
            }
        }
    }
}

fn announce(state: &AppState) {
    let Some(sock) = state.sock.as_ref() else { return };
    let (net, rx, tx) = {
        let l = state.live.lock().unwrap();
        (l.network.clone(), l.rate_rx, l.rate_tx)
    };
    let Some(net) = net else { return };
    let a = Announce {
        app: peers::APP_TAG.into(),
        v: 1,
        device_id: state.device_id.clone(),
        device_name: state.device_name.lock().unwrap().clone(),
        os: state.os.clone(),
        network_id: net.id.clone(),
        network_name: net.name.clone(),
        rate_rx: rx,
        rate_tx: tx,
        totals: state.current_totals(),
        ts: now(),
    };
    peers::send(sock, &a, net.broadcast_ipv4.as_deref());
}

pub fn spawn_receiver(state: Arc<AppState>) {
    let Some(sock) = state.sock.as_ref().and_then(|s| s.try_clone().ok()) else { return };
    std::thread::Builder::new()
        .name("pairs".into())
        .spawn(move || {
            let mut buf = vec![0u8; 8192];
            loop {
                let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
                let Some(a) = peers::parse(&buf[..n]) else { continue };
                if a.device_id == state.device_id {
                    continue;
                }
                let t = now();
                let row = crate::db::PeerRow {
                    device_id: a.device_id.clone(),
                    device_name: a.device_name.clone(),
                    os: a.os.clone(),
                    network_id: a.network_id.clone(),
                    last_seen: t,
                    payload: serde_json::to_string(&a).unwrap_or_default(),
                };
                let _ = state.db.lock().unwrap().save_peer(&row);
                state.peers.lock().unwrap().insert(
                    a.device_id.clone(),
                    crate::state::PeerLive { announce: a, last_seen: t, ip: from.ip().to_string() },
                );
            }
        })
        .expect("thread pairs");
}

pub fn spawn(app: AppHandle, state: Arc<AppState>) {
    std::thread::Builder::new()
        .name("moniteur".into())
        .spawn(move || {
            let mut nets = Networks::new_with_refreshed_list();
            let mut last = Instant::now();
            let mut tick: u64 = 0;

            update_network(&state);
            nets.refresh(true);

            loop {
                std::thread::sleep(Duration::from_millis(1000));
                let dt = last.elapsed().as_secs_f64().max(0.2);
                last = Instant::now();
                tick += 1;

                if tick % 5 == 0 && update_network(&state) {
                    nets.refresh(true);
                    let _ = app.emit("network-changed", ());
                    continue;
                }

                // --- Débit de l'interface par défaut ---
                nets.refresh(true);
                let iface = state.live.lock().unwrap().network.as_ref().map(|n| n.interface.clone());
                let mut delta = Bytes::default();
                if let Some(name) = iface.as_deref() {
                    if let Some((_, data)) = nets.iter().find(|(n, _)| n.as_str() == name) {
                        delta = Bytes { rx: data.received(), tx: data.transmitted() };
                    }
                }

                // --- Trafic par application (capture) ---
                let (apps, protos) = state.capture.drain();

                {
                    let mut l = state.live.lock().unwrap();
                    if l.network.is_some() {
                        l.pending.add(delta);
                        for (k, b) in &apps {
                            l.pending_apps.entry(k.clone()).or_default().add(*b);
                        }
                    }
                    l.rate_rx = delta.rx as f64 / dt;
                    l.rate_tx = delta.tx as f64 / dt;
                    let (rx, tx) = (l.rate_rx, l.rate_tx);
                    l.history.push_back((now(), rx, tx));
                    while l.history.len() > HISTORY_LEN {
                        l.history.pop_front();
                    }
                    smooth(&mut l.app_rates, &apps, dt);
                    smooth(&mut l.proto_rates, &protos, dt);
                }

                if tick % 10 == 0 {
                    flush(&state);
                    check_quotas(&app, &state);
                }
                if tick % 5 == 1 {
                    announce(&state);
                }

                let snap = state.snapshot();
                crate::tray::update(&app, &snap);
                let _ = app.emit("tick", &snap);
            }
        })
        .expect("thread moniteur");
}
