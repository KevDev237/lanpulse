mod capture;
mod db;
mod monitor;
mod peers;
mod state;
mod tray;
mod wifi;

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{Manager, State, WindowEvent};

use db::{AppRow, DayRow, NetworkRow, PeriodTotals, Person};
use state::{now, AppState, Snapshot};

type S<'a> = State<'a, Arc<AppState>>;
type R<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[derive(Serialize)]
struct PeerView {
    device_id: String,
    device_name: String,
    os: String,
    network_id: String,
    network_name: String,
    online: bool,
    last_seen: i64,
    ip: Option<String>,
    rate_rx: f64,
    rate_tx: f64,
    totals: PeriodTotals,
    person_id: Option<i64>,
    is_self: bool,
}

/// Les totaux d'un pair hors ligne ont été annoncés à la date `ts` : on remet
/// à zéro les périodes déjà écoulées depuis (jour, semaine, mois).
fn freshen(mut t: PeriodTotals, ts: i64) -> PeriodTotals {
    use chrono::{Datelike, Local, TimeZone};
    let Some(then) = Local.timestamp_opt(ts, 0).single().map(|d| d.date_naive()) else {
        return t;
    };
    let today = Local::now().date_naive();
    if then != today {
        t.day = Default::default();
    }
    if then.iso_week() != today.iso_week() {
        t.week = Default::default();
    }
    if (then.year(), then.month()) != (today.year(), today.month()) {
        t.month = Default::default();
    }
    t
}

#[tauri::command]
fn get_snapshot(state: S) -> Snapshot {
    state.snapshot()
}

#[tauri::command]
fn get_networks(state: S) -> R<Vec<NetworkRow>> {
    monitor::flush(&state);
    state.db.lock().unwrap().networks().map_err(err)
}

#[tauri::command]
fn rename_network(state: S, network_id: String, name: String) -> R<()> {
    state.db.lock().unwrap().rename_network(&network_id, name.trim()).map_err(err)
}

#[tauri::command]
fn set_quota(state: S, network_id: String, daily_mb: Option<u64>, monthly_mb: Option<u64>) -> R<()> {
    state.live.lock().unwrap().alerts_sent.clear();
    state
        .db
        .lock()
        .unwrap()
        .set_quota(&network_id, daily_mb, monthly_mb)
        .map_err(err)
}

#[tauri::command]
fn get_app_usage(state: S, network_id: Option<String>, period: String) -> R<Vec<AppRow>> {
    monitor::flush(&state);
    state.db.lock().unwrap().app_usage(network_id.as_deref(), &period).map_err(err)
}

#[tauri::command]
fn get_history(state: S, network_id: Option<String>, days: i64) -> R<Vec<DayRow>> {
    monitor::flush(&state);
    state
        .db
        .lock()
        .unwrap()
        .daily_history(network_id.as_deref(), days.clamp(1, 366))
        .map_err(err)
}

/// Liste des machines : celle-ci, les pairs en ligne et ceux déjà vus.
#[tauri::command]
fn get_peers(state: S) -> R<Vec<PeerView>> {
    let t = now();
    let assignments: HashMap<String, i64> = state.db.lock().unwrap().device_persons().map_err(err)?;
    let snap = state.snapshot();
    let mut out = vec![PeerView {
        device_id: state.device_id.clone(),
        device_name: snap.device_name.clone(),
        os: state.os.clone(),
        network_id: snap.network.as_ref().map(|n| n.id.clone()).unwrap_or_default(),
        network_name: snap.network.as_ref().map(|n| n.name.clone()).unwrap_or_default(),
        online: true,
        last_seen: t,
        ip: snap.network.as_ref().and_then(|n| n.local_ipv4.clone()),
        rate_rx: snap.rate_rx,
        rate_tx: snap.rate_tx,
        totals: snap.totals.clone(),
        person_id: assignments.get(&state.device_id).copied(),
        is_self: true,
    }];

    let live = state.peers.lock().unwrap();
    let stored = state.db.lock().unwrap().peers().map_err(err)?;
    for row in stored {
        let (a, last_seen, ip) = match live.get(&row.device_id) {
            Some(p) => (p.announce.clone(), p.last_seen, Some(p.ip.clone())),
            None => match serde_json::from_str::<peers::Announce>(&row.payload) {
                Ok(a) => (a, row.last_seen, None),
                Err(_) => continue,
            },
        };
        let online = t - last_seen <= peers::ONLINE_SECS;
        out.push(PeerView {
            device_id: a.device_id.clone(),
            device_name: a.device_name,
            os: a.os,
            network_id: a.network_id,
            network_name: a.network_name,
            online,
            last_seen,
            ip,
            rate_rx: if online { a.rate_rx } else { 0.0 },
            rate_tx: if online { a.rate_tx } else { 0.0 },
            totals: freshen(a.totals, a.ts),
            person_id: assignments.get(&a.device_id).copied(),
            is_self: false,
        });
    }
    Ok(out)
}

#[tauri::command]
fn forget_peer(state: S, device_id: String) -> R<()> {
    state.peers.lock().unwrap().remove(&device_id);
    state.db.lock().unwrap().forget_peer(&device_id).map_err(err)
}

#[tauri::command]
fn get_persons(state: S) -> R<Vec<Person>> {
    state.db.lock().unwrap().persons().map_err(err)
}

#[tauri::command]
fn create_person(state: S, name: String) -> R<i64> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Le nom est vide".into());
    }
    state.db.lock().unwrap().create_person(name).map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            "Cette personne existe déjà".to_string()
        } else {
            e.to_string()
        }
    })
}

#[tauri::command]
fn rename_person(state: S, id: i64, name: String) -> R<()> {
    state.db.lock().unwrap().rename_person(id, name.trim()).map_err(err)
}

#[tauri::command]
fn delete_person(state: S, id: i64) -> R<()> {
    state.db.lock().unwrap().delete_person(id).map_err(err)
}

#[tauri::command]
fn assign_device(state: S, device_id: String, person_id: Option<i64>) -> R<()> {
    state.db.lock().unwrap().assign_device(&device_id, person_id).map_err(err)
}

#[tauri::command]
fn set_device_name(state: S, name: String) -> R<()> {
    let name = name.trim().chars().take(64).collect::<String>();
    if name.is_empty() {
        return Err("Le nom est vide".into());
    }
    state.db.lock().unwrap().set_setting("device_name", &name).map_err(err)?;
    *state.device_name.lock().unwrap() = name;
    Ok(())
}

fn build_state(app: &tauri::AppHandle) -> Result<Arc<AppState>, Box<dyn std::error::Error>> {
    let dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&dir)?;
    let db = db::Db::open(&dir.join("lanpulse.sqlite"))?;

    let device_id = match db.get_setting("device_id") {
        Some(id) => id,
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            db.set_setting("device_id", &id)?;
            id
        }
    };
    let device_name = db
        .get_setting("device_name")
        .unwrap_or_else(|| gethostname::gethostname().to_string_lossy().to_string());

    let sock = match peers::bind() {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("[lanpulse] découverte des pairs désactivée : {e}");
            None
        }
    };

    Ok(Arc::new(AppState {
        device_id,
        device_name: Mutex::new(device_name),
        os: state::os_label(),
        db: Mutex::new(db),
        live: Mutex::new(Default::default()),
        capture: Arc::new(Default::default()),
        peers: Mutex::new(HashMap::new()),
        sock,
    }))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let state = build_state(&handle)?;
            app.manage(state.clone());
            // Un échec de l'icône (bureau sans zone de notification, droits)
            // ne doit pas empêcher l'application de démarrer.
            if let Err(e) = tray::create(&handle) {
                eprintln!("[lanpulse] icône de barre indisponible : {e}");
            }
            capture::spawn(state.capture.clone());
            monitor::spawn_receiver(state.clone());
            monitor::spawn(handle, state);
            Ok(())
        })
        .on_window_event(|window, event| {
            // Fermer la fenêtre la cache : l'application reste dans la barre.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_networks,
            rename_network,
            set_quota,
            get_app_usage,
            get_history,
            get_peers,
            forget_peer,
            get_persons,
            create_person,
            rename_person,
            delete_person,
            assign_device,
            set_device_name
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de LanPulse");
}
