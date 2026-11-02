//! Stockage local SQLite : réseaux connus, sessions, consommation journalière,
//! consommation par application, pairs vus, personnes et regroupements.

use chrono::{Datelike, Duration, Local, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::wifi::NetworkInfo;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Bytes {
    pub rx: u64,
    pub tx: u64,
}

impl Bytes {
    pub fn add(&mut self, o: Bytes) {
        self.rx += o.rx;
        self.tx += o.tx;
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionTotals {
    pub rx: u64,
    pub tx: u64,
    /// Début de la session (timestamp Unix, secondes).
    pub since: i64,
    /// Fin de la session, absente si la session est en cours.
    pub until: Option<i64>,
}

/// Totaux par période, tels qu'affichés localement et partagés avec les pairs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeriodTotals {
    pub session: SessionTotals,
    pub day: Bytes,
    pub week: Bytes,
    pub month: Bytes,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkRow {
    pub id: String,
    pub name: String,
    pub ssid: Option<String>,
    pub kind: String,
    pub gateway_ip: Option<String>,
    pub first_seen: i64,
    pub last_seen: i64,
    pub daily_quota_mb: Option<u64>,
    pub monthly_quota_mb: Option<u64>,
    pub totals: PeriodTotals,
    pub sessions: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppRow {
    pub app: String,
    pub rx: u64,
    pub tx: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DayRow {
    pub day: String,
    pub rx: u64,
    pub tx: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Person {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRow {
    pub device_id: String,
    pub device_name: String,
    pub os: String,
    pub network_id: String,
    pub last_seen: i64,
    pub payload: String,
}

pub struct Db {
    conn: Connection,
}

pub fn today() -> String {
    Local::now().date_naive().format("%Y-%m-%d").to_string()
}

fn day_str(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// Bornes (incluses) des périodes jour, semaine (lundi) et mois.
fn period_bounds() -> (String, String, String) {
    let d = Local::now().date_naive();
    let week_start = d - Duration::days(d.weekday().num_days_from_monday() as i64);
    let month_start = d.with_day(1).unwrap_or(d);
    (day_str(d), day_str(week_start), day_str(month_start))
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS networks (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, ssid TEXT, kind TEXT NOT NULL,
                gateway_ip TEXT, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL,
                daily_quota_mb INTEGER, monthly_quota_mb INTEGER
            );
            CREATE TABLE IF NOT EXISTS sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT, network_id TEXT NOT NULL,
                start_ts INTEGER NOT NULL, end_ts INTEGER, rx INTEGER NOT NULL DEFAULT 0,
                tx INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_net ON sessions(network_id, start_ts);
            CREATE TABLE IF NOT EXISTS usage_daily (
                network_id TEXT NOT NULL, day TEXT NOT NULL,
                rx INTEGER NOT NULL DEFAULT 0, tx INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (network_id, day)
            );
            CREATE TABLE IF NOT EXISTS app_usage (
                network_id TEXT NOT NULL, day TEXT NOT NULL, app TEXT NOT NULL,
                rx INTEGER NOT NULL DEFAULT 0, tx INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (network_id, day, app)
            );
            CREATE TABLE IF NOT EXISTS peers (
                device_id TEXT PRIMARY KEY, device_name TEXT NOT NULL, os TEXT NOT NULL,
                network_id TEXT NOT NULL, last_seen INTEGER NOT NULL, payload TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS persons (
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE
            );
            CREATE TABLE IF NOT EXISTS device_person (
                device_id TEXT PRIMARY KEY,
                person_id INTEGER NOT NULL REFERENCES persons(id) ON DELETE CASCADE
            );
            "#,
        )?;
        // Une session restée ouverte (arrêt brutal) est clôturée au démarrage.
        conn.execute(
            "UPDATE sessions SET end_ts = start_ts WHERE end_ts IS NULL AND rx = 0 AND tx = 0",
            [],
        )?;
        conn.execute(
            "UPDATE sessions SET end_ts = strftime('%s','now') WHERE end_ts IS NULL",
            [],
        )?;
        Ok(Self { conn })
    }

    // ---------- Réglages ----------

    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ---------- Réseaux et sessions ----------

    pub fn upsert_network(&self, n: &NetworkInfo, now: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO networks(id, name, ssid, kind, gateway_ip, first_seen, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(id) DO UPDATE SET
                name = CASE WHEN excluded.ssid IS NOT NULL THEN excluded.name ELSE networks.name END,
                ssid = COALESCE(excluded.ssid, networks.ssid),
                kind = excluded.kind,
                gateway_ip = COALESCE(excluded.gateway_ip, networks.gateway_ip),
                last_seen = excluded.last_seen",
            params![n.id, n.name, n.ssid, n.kind, n.gateway_ip, now],
        )?;
        Ok(())
    }

    pub fn open_session(&self, network_id: &str, now: i64) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO sessions(network_id, start_ts) VALUES (?1, ?2)",
            params![network_id, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn close_session(&self, session_id: i64, now: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET end_ts = ?2 WHERE id = ?1",
            params![session_id, now],
        )?;
        Ok(())
    }

    /// Enregistre un lot de consommation accumulée (appelé toutes les ~10 s).
    pub fn flush(
        &mut self,
        network_id: &str,
        session_id: Option<i64>,
        total: Bytes,
        apps: &HashMap<String, Bytes>,
        now: i64,
    ) -> rusqlite::Result<()> {
        let day = today();
        let tx_ = self.conn.transaction()?;
        tx_.execute(
            "INSERT INTO usage_daily(network_id, day, rx, tx) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(network_id, day) DO UPDATE SET rx = rx + excluded.rx, tx = tx + excluded.tx",
            params![network_id, day, total.rx as i64, total.tx as i64],
        )?;
        if let Some(sid) = session_id {
            tx_.execute(
                "UPDATE sessions SET rx = rx + ?2, tx = tx + ?3 WHERE id = ?1",
                params![sid, total.rx as i64, total.tx as i64],
            )?;
        }
        for (app, b) in apps {
            if b.rx == 0 && b.tx == 0 {
                continue;
            }
            tx_.execute(
                "INSERT INTO app_usage(network_id, day, app, rx, tx) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(network_id, day, app) DO UPDATE SET rx = rx + excluded.rx, tx = tx + excluded.tx",
                params![network_id, day, app, b.rx as i64, b.tx as i64],
            )?;
        }
        tx_.execute(
            "UPDATE networks SET last_seen = ?2 WHERE id = ?1",
            params![network_id, now],
        )?;
        tx_.commit()
    }

    fn sum_since(&self, network_id: &str, from_day: &str) -> Bytes {
        self.conn
            .query_row(
                "SELECT COALESCE(SUM(rx),0), COALESCE(SUM(tx),0) FROM usage_daily
                 WHERE network_id = ?1 AND day >= ?2",
                params![network_id, from_day],
                |r| {
                    Ok(Bytes {
                        rx: r.get::<_, i64>(0)? as u64,
                        tx: r.get::<_, i64>(1)? as u64,
                    })
                },
            )
            .unwrap_or_default()
    }

    /// Session en cours sur ce réseau, ou à défaut la dernière session connue.
    pub fn last_session(&self, network_id: &str) -> SessionTotals {
        self.conn
            .query_row(
                "SELECT start_ts, end_ts, rx, tx FROM sessions WHERE network_id = ?1
                 ORDER BY (end_ts IS NULL) DESC, start_ts DESC LIMIT 1",
                [network_id],
                |r| {
                    Ok(SessionTotals {
                        since: r.get(0)?,
                        until: r.get(1)?,
                        rx: r.get::<_, i64>(2)? as u64,
                        tx: r.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .unwrap_or_default()
    }

    /// Totaux enregistrés (hors lot non encore écrit, ajouté par l'appelant).
    pub fn period_totals(&self, network_id: &str) -> PeriodTotals {
        let (d, w, m) = period_bounds();
        PeriodTotals {
            session: self.last_session(network_id),
            day: self.sum_since(network_id, &d),
            week: self.sum_since(network_id, &w),
            month: self.sum_since(network_id, &m),
        }
    }

    pub fn networks(&self) -> rusqlite::Result<Vec<NetworkRow>> {
        let mut st = self.conn.prepare(
            "SELECT n.id, n.name, n.ssid, n.kind, n.gateway_ip, n.first_seen, n.last_seen,
                    n.daily_quota_mb, n.monthly_quota_mb,
                    (SELECT COUNT(*) FROM sessions s WHERE s.network_id = n.id)
             FROM networks n ORDER BY n.last_seen DESC",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(NetworkRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    ssid: r.get(2)?,
                    kind: r.get(3)?,
                    gateway_ip: r.get(4)?,
                    first_seen: r.get(5)?,
                    last_seen: r.get(6)?,
                    daily_quota_mb: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                    monthly_quota_mb: r.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                    totals: PeriodTotals::default(),
                    sessions: r.get::<_, i64>(9)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .map(|mut n| {
                n.totals = self.period_totals(&n.id);
                n
            })
            .collect())
    }

    pub fn quotas(&self, network_id: &str) -> (Option<u64>, Option<u64>) {
        self.conn
            .query_row(
                "SELECT daily_quota_mb, monthly_quota_mb FROM networks WHERE id = ?1",
                [network_id],
                |r| {
                    Ok((
                        r.get::<_, Option<i64>>(0)?.map(|v| v as u64),
                        r.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                    ))
                },
            )
            .unwrap_or((None, None))
    }

    pub fn set_quota(
        &self,
        network_id: &str,
        daily_mb: Option<u64>,
        monthly_mb: Option<u64>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE networks SET daily_quota_mb = ?2, monthly_quota_mb = ?3 WHERE id = ?1",
            params![
                network_id,
                daily_mb.map(|v| v as i64),
                monthly_mb.map(|v| v as i64)
            ],
        )?;
        Ok(())
    }

    pub fn rename_network(&self, network_id: &str, name: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE networks SET name = ?2 WHERE id = ?1",
            params![network_id, name],
        )?;
        Ok(())
    }

    /// Consommation par application sur une période ("day", "week", "month").
    pub fn app_usage(&self, network_id: Option<&str>, period: &str) -> rusqlite::Result<Vec<AppRow>> {
        let (d, w, m) = period_bounds();
        let from = match period {
            "week" => w,
            "month" => m,
            _ => d,
        };
        let sql = "SELECT app, SUM(rx), SUM(tx) FROM app_usage
                   WHERE day >= ?1 AND (?2 IS NULL OR network_id = ?2)
                   GROUP BY app ORDER BY SUM(rx) + SUM(tx) DESC LIMIT 50";
        let mut st = self.conn.prepare(sql)?;
        let rows = st
            .query_map(params![from, network_id], |r| {
                Ok(AppRow {
                    app: r.get(0)?,
                    rx: r.get::<_, i64>(1)? as u64,
                    tx: r.get::<_, i64>(2)? as u64,
                })
            })?
            .collect();
        rows
    }

    /// Consommation journalière sur les `days` derniers jours.
    pub fn daily_history(&self, network_id: Option<&str>, days: i64) -> rusqlite::Result<Vec<DayRow>> {
        let from = day_str(Local::now().date_naive() - Duration::days(days - 1));
        let mut st = self.conn.prepare(
            "SELECT day, SUM(rx), SUM(tx) FROM usage_daily
             WHERE day >= ?1 AND (?2 IS NULL OR network_id = ?2)
             GROUP BY day ORDER BY day",
        )?;
        let rows = st
            .query_map(params![from, network_id], |r| {
                Ok(DayRow {
                    day: r.get(0)?,
                    rx: r.get::<_, i64>(1)? as u64,
                    tx: r.get::<_, i64>(2)? as u64,
                })
            })?
            .collect();
        rows
    }

    // ---------- Pairs ----------

    pub fn save_peer(&self, p: &PeerRow) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO peers(device_id, device_name, os, network_id, last_seen, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(device_id) DO UPDATE SET device_name = excluded.device_name,
               os = excluded.os, network_id = excluded.network_id,
               last_seen = excluded.last_seen, payload = excluded.payload",
            params![p.device_id, p.device_name, p.os, p.network_id, p.last_seen, p.payload],
        )?;
        Ok(())
    }

    pub fn peers(&self) -> rusqlite::Result<Vec<PeerRow>> {
        let mut st = self.conn.prepare(
            "SELECT device_id, device_name, os, network_id, last_seen, payload
             FROM peers ORDER BY last_seen DESC",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(PeerRow {
                    device_id: r.get(0)?,
                    device_name: r.get(1)?,
                    os: r.get(2)?,
                    network_id: r.get(3)?,
                    last_seen: r.get(4)?,
                    payload: r.get(5)?,
                })
            })?
            .collect();
        rows
    }

    pub fn forget_peer(&self, device_id: &str) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM peers WHERE device_id = ?1", [device_id])?;
        self.conn.execute("DELETE FROM device_person WHERE device_id = ?1", [device_id])?;
        Ok(())
    }

    // ---------- Personnes et regroupement ----------

    pub fn persons(&self) -> rusqlite::Result<Vec<Person>> {
        let mut st = self.conn.prepare("SELECT id, name FROM persons ORDER BY name COLLATE NOCASE")?;
        let rows = st
            .query_map([], |r| Ok(Person { id: r.get(0)?, name: r.get(1)? }))?
            .collect();
        rows
    }

    pub fn create_person(&self, name: &str) -> rusqlite::Result<i64> {
        self.conn.execute("INSERT INTO persons(name) VALUES (?1)", [name])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename_person(&self, id: i64, name: &str) -> rusqlite::Result<()> {
        self.conn.execute("UPDATE persons SET name = ?2 WHERE id = ?1", params![id, name])?;
        Ok(())
    }

    pub fn delete_person(&self, id: i64) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM device_person WHERE person_id = ?1", [id])?;
        self.conn.execute("DELETE FROM persons WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn assign_device(&self, device_id: &str, person_id: Option<i64>) -> rusqlite::Result<()> {
        match person_id {
            Some(pid) => self.conn.execute(
                "INSERT INTO device_person(device_id, person_id) VALUES (?1, ?2)
                 ON CONFLICT(device_id) DO UPDATE SET person_id = excluded.person_id",
                params![device_id, pid],
            )?,
            None => self
                .conn
                .execute("DELETE FROM device_person WHERE device_id = ?1", [device_id])?,
        };
        Ok(())
    }

    pub fn device_persons(&self) -> rusqlite::Result<HashMap<String, i64>> {
        let mut st = self.conn.prepare("SELECT device_id, person_id FROM device_person")?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<HashMap<_, _>>>();
        rows
    }
}
