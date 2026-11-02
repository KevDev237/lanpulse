"use strict";

// ---------------------------------------------------------------------------
// Accès au backend Tauri (ou données de démonstration dans un navigateur)
// ---------------------------------------------------------------------------
const TAURI = window.__TAURI__;
const invoke = TAURI ? TAURI.core.invoke : (cmd, args) => Demo.invoke(cmd, args);
const listen = TAURI ? TAURI.event.listen : (ev, cb) => Demo.listen(ev, cb);

// ---------------------------------------------------------------------------
// Utilitaires
// ---------------------------------------------------------------------------
const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

function fmtBytes(n) {
  const u = ["o", "Ko", "Mo", "Go", "To"];
  let i = 0, v = Number(n) || 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return (i === 0 ? v.toFixed(0) : v < 10 ? v.toFixed(2) : v < 100 ? v.toFixed(1) : v.toFixed(0)) + " " + u[i];
}
const fmtRate = (n) => fmtBytes(n) + "/s";

function fmtAgo(ts) {
  const d = Math.max(0, Date.now() / 1000 - ts);
  if (d < 20) return "à l'instant";
  if (d < 3600) return `il y a ${Math.round(d / 60)} min`;
  if (d < 86400) return `il y a ${Math.round(d / 3600)} h`;
  return new Date(ts * 1000).toLocaleDateString("fr-FR", { day: "numeric", month: "short" });
}
function fmtTime(ts) {
  return new Date(ts * 1000).toLocaleString("fr-FR", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
}
function toast(msg) {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.remove("hidden");
  clearTimeout(toast._t);
  toast._t = setTimeout(() => t.classList.add("hidden"), 2600);
}
async function call(cmd, args) {
  try { return await invoke(cmd, args); }
  catch (e) { toast(String(e)); throw e; }
}
const osShort = (os) => ({ Windows: "WIN", macOS: "MAC", Linux: "LNX" }[os] || "?");
const PERIOD_LABEL = { session: "Dernière connexion", day: "Aujourd'hui", week: "Cette semaine", month: "Ce mois" };

function cssVar(name) { return getComputedStyle(document.documentElement).getPropertyValue(name).trim(); }

// ---------------------------------------------------------------------------
// État de l'interface
// ---------------------------------------------------------------------------
const ui = {
  view: "live",
  snap: null,
  appsPeriod: "day",
  appsScope: "current",
  peersPeriod: "session",
  personsPeriod: "day",
  selectedNet: null,
  networks: [],
};

// ---------------------------------------------------------------------------
// Graphiques (canvas)
// ---------------------------------------------------------------------------
function prepCanvas(c) {
  const dpr = window.devicePixelRatio || 1;
  const w = c.clientWidth, h = c.clientHeight;
  if (c.width !== w * dpr || c.height !== h * dpr) { c.width = w * dpr; c.height = h * dpr; }
  const ctx = c.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  return { ctx, w, h };
}
function niceMax(v) {
  if (v <= 0) return 1024;
  const p = Math.pow(1024, Math.floor(Math.log(v) / Math.log(1024)));
  const n = v / p;
  const step = n <= 1 ? 1 : n <= 2 ? 2 : n <= 5 ? 5 : n <= 10 ? 10 : n <= 20 ? 20 : n <= 50 ? 50 : n <= 100 ? 100 : n <= 200 ? 200 : n <= 500 ? 500 : 1024;
  return step * p;
}
function axes(ctx, w, h, pad, max, fmt) {
  ctx.font = "11px system-ui, sans-serif";
  ctx.fillStyle = cssVar("--muted");
  ctx.strokeStyle = cssVar("--line");
  ctx.lineWidth = 1;
  ctx.textAlign = "right";
  ctx.textBaseline = "middle";
  for (let i = 0; i <= 4; i++) {
    const y = pad.t + (h - pad.t - pad.b) * (1 - i / 4);
    ctx.beginPath(); ctx.moveTo(pad.l, Math.round(y) + .5); ctx.lineTo(w - pad.r, Math.round(y) + .5); ctx.stroke();
    ctx.fillText(fmt(max * i / 4), pad.l - 8, y);
  }
}
function drawLive(history) {
  const c = $("#live-chart");
  if (!c.clientWidth) return;
  const { ctx, w, h } = prepCanvas(c);
  const pad = { l: 70, r: 10, t: 10, b: 22 };
  const pts = history.slice(-300);
  const max = niceMax(Math.max(1, ...pts.map((p) => Math.max(p[1], p[2]))));
  axes(ctx, w, h, pad, max, fmtRate);
  const n = 300, iw = w - pad.l - pad.r, ih = h - pad.t - pad.b;
  const X = (i) => pad.l + iw * (i + (n - pts.length)) / (n - 1);
  const Y = (v) => pad.t + ih * (1 - v / max);
  ctx.fillStyle = cssVar("--muted"); ctx.textAlign = "center"; ctx.textBaseline = "top";
  ["-5 min", "-4 min", "-3 min", "-2 min", "-1 min", "maintenant"].forEach((t, i) => {
    ctx.textAlign = i === 0 ? "left" : i === 5 ? "right" : "center";
    ctx.fillText(t, pad.l + iw * i / 5, h - pad.b + 6);
  });
  for (const [idx, color] of [[1, cssVar("--rx")], [2, cssVar("--tx")]]) {
    if (!pts.length) break;
    ctx.beginPath();
    pts.forEach((p, i) => (i ? ctx.lineTo(X(i), Y(p[idx])) : ctx.moveTo(X(i), Y(p[idx]))));
    ctx.strokeStyle = color; ctx.lineWidth = 2; ctx.lineJoin = "round"; ctx.stroke();
    ctx.lineTo(X(pts.length - 1), Y(0)); ctx.lineTo(X(0), Y(0)); ctx.closePath();
    ctx.globalAlpha = .10; ctx.fillStyle = color; ctx.fill(); ctx.globalAlpha = 1;
  }
}
function drawHistory(rows, days = 14) {
  const c = $("#hist-chart");
  if (!c.clientWidth) return;
  const { ctx, w, h } = prepCanvas(c);
  const pad = { l: 64, r: 8, t: 10, b: 24 };
  const byDay = Object.fromEntries(rows.map((r) => [r.day, r]));
  const list = [];
  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(); d.setDate(d.getDate() - i);
    const key = d.toLocaleDateString("sv-SE");
    list.push({ key, label: d.toLocaleDateString("fr-FR", { day: "numeric" }), ...(byDay[key] || { rx: 0, tx: 0 }) });
  }
  const max = niceMax(Math.max(1, ...list.map((r) => Math.max(r.rx, r.tx))));
  axes(ctx, w, h, pad, max, fmtBytes);
  const iw = w - pad.l - pad.r, ih = h - pad.t - pad.b, gw = iw / days, bw = Math.max(3, Math.min(14, gw * .32));
  ctx.textAlign = "center"; ctx.textBaseline = "top"; ctx.fillStyle = cssVar("--muted");
  list.forEach((r, i) => {
    const cx = pad.l + gw * i + gw / 2;
    for (const [v, off, col] of [[r.rx, -bw - 1, cssVar("--rx")], [r.tx, 1, cssVar("--tx")]]) {
      const bh = ih * v / max;
      ctx.fillStyle = col;
      ctx.beginPath(); ctx.roundRect(cx + off, pad.t + ih - bh, bw, bh, [3, 3, 0, 0]); ctx.fill();
    }
    ctx.fillStyle = cssVar("--muted");
    ctx.fillText(r.label, cx, h - pad.b + 6);
  });
}

// ---------------------------------------------------------------------------
// Rendu : lignes avec barres
// ---------------------------------------------------------------------------
function barRows(el, items, { rate = false, emptyMsg = "Aucune donnée" } = {}) {
  if (!items.length) { el.innerHTML = `<div class="empty">${esc(emptyMsg)}</div>`; return; }
  const max = Math.max(...items.map((i) => i.rx + i.tx), 1);
  const f = rate ? fmtRate : fmtBytes;
  el.innerHTML = items.map((i) => {
    const r = (i.rx / max) * 100, t = (i.tx / max) * 100;
    return `<div class="row"><span class="name" title="${esc(i.name ?? i.app)}">${esc(i.name ?? i.app)}</span>
      <div class="bar"><i class="r" style="width:${r}%"></i><i class="t" style="width:${t}%"></i></div>
      <span class="num">↓ ${f(i.rx)} · ↑ ${f(i.tx)}</span></div>`;
  }).join("");
}

function totalCard(label, b, extra = "") {
  return `<div class="total"><div class="lbl">${esc(label)}</div>
    <div class="val">${fmtBytes(b.rx + b.tx)}</div>
    <div class="split"><span class="r">↓ ${fmtBytes(b.rx)}</span><span class="t">↑ ${fmtBytes(b.tx)}</span></div>${extra}</div>`;
}

// ---------------------------------------------------------------------------
// Vue temps réel
// ---------------------------------------------------------------------------
function renderLive(s) {
  ui.snap = s;
  const n = s.network;
  $("#net-name").textContent = n ? n.name : "Aucun réseau";
  $("#net-kind").textContent = n ? (n.kind === "wifi" ? "Wi-Fi" : n.kind === "ethernet" ? "Filaire" : "Réseau") : "Hors ligne";
  $("#net-meta").textContent = n ? [n.interface, n.local_ipv4 && "IP " + n.local_ipv4, n.gateway_ip && "passerelle " + n.gateway_ip].filter(Boolean).join(" · ") : "";
  $("#rate-rx").textContent = fmtRate(s.rate_rx);
  $("#rate-tx").textContent = fmtRate(s.rate_tx);

  $("#foot-dot").classList.toggle("on", !!n);
  $("#foot-net").textContent = n ? n.name : "Hors ligne";
  $("#foot-device").textContent = `${s.device_name} · ${s.os}`;
  const np = $("#nav-peers");
  np.textContent = s.peers_online;
  np.classList.toggle("hidden", !s.peers_online);

  const banner = $("#capture-banner");
  if (s.capture.active) banner.classList.add("hidden");
  else {
    banner.classList.remove("hidden");
    banner.innerHTML = `Vue par application indisponible : ${esc(s.capture.message || "capture inactive")} <a data-go="settings">Comment l'activer</a>`;
  }

  if (ui.view === "live") {
    const t = s.totals;
    const since = t.session.since ? `<div class="muted small" style="margin-top:6px">depuis ${fmtTime(t.session.since)}</div>` : "";
    const net = ui.networks.find((x) => n && x.id === n.id);
    const q = (used, mb) => {
      if (!mb) return "";
      const pct = Math.min(100, (used / (mb * 1048576)) * 100);
      return `<div class="quota-bar ${pct >= 90 ? "hot" : ""}"><i style="width:${pct}%"></i></div><div class="muted small">${pct.toFixed(0)} % du quota (${mb} Mo)</div>`;
    };
    $("#totals").innerHTML =
      totalCard("Connexion en cours", t.session, since) +
      totalCard("Aujourd'hui", t.day, q(t.day.rx + t.day.tx, net?.daily_quota_mb)) +
      totalCard("Cette semaine", t.week) +
      totalCard("Ce mois", t.month, q(t.month.rx + t.month.tx, net?.monthly_quota_mb));
    drawLive(s.history);
    barRows($("#live-apps"), s.apps.slice(0, 10), { rate: true, emptyMsg: s.capture.active ? "Pas de trafic en ce moment" : "Capture inactive" });
    barRows($("#live-protos"), s.protocols, { rate: true, emptyMsg: s.capture.active ? "Pas de trafic en ce moment" : "Capture inactive" });
  }
}

// ---------------------------------------------------------------------------
// Vue applications
// ---------------------------------------------------------------------------
async function renderApps() {
  const net = ui.appsScope === "current" ? ui.snap?.network?.id ?? null : null;
  const rows = await call("get_app_usage", { networkId: net, period: ui.appsPeriod });
  barRows($("#apps-table"), rows, { emptyMsg: "Aucune consommation enregistrée sur cette période" });
}

// ---------------------------------------------------------------------------
// Vue réseaux
// ---------------------------------------------------------------------------
async function loadNetworks() { ui.networks = await call("get_networks"); }

async function renderNetworks() {
  await loadNetworks();
  const cur = ui.snap?.network?.id;
  if (!ui.selectedNet) ui.selectedNet = cur || ui.networks[0]?.id || null;
  const el = $("#networks-list");
  if (!ui.networks.length) { el.innerHTML = `<div class="card empty">Aucun réseau mémorisé pour l'instant</div>`; }
  else el.innerHTML = ui.networks.map((n) => {
    const t = n.totals;
    return `<div class="card net ${n.id === ui.selectedNet ? "selected" : ""}" data-id="${esc(n.id)}">
      <div class="net-top">
        <div><div class="net-title">${esc(n.name)} ${n.id === cur ? '<span class="tag">connecté</span>' : ""}</div>
        <div class="muted small">${n.kind === "wifi" ? "Wi-Fi" : n.kind === "ethernet" ? "Filaire" : "Réseau"} · ${n.sessions} connexion${n.sessions > 1 ? "s" : ""} · vu ${fmtAgo(n.last_seen)}</div></div>
        <button class="link-btn" data-rename="${esc(n.id)}">Renommer</button>
      </div>
      <div class="mini">
        <div>Dernière connexion<b>${fmtBytes(t.session.rx + t.session.tx)}</b></div>
        <div>Jour<b>${fmtBytes(t.day.rx + t.day.tx)}</b></div>
        <div>Semaine<b>${fmtBytes(t.week.rx + t.week.tx)}</b></div>
        <div>Mois<b>${fmtBytes(t.month.rx + t.month.tx)}</b></div>
      </div>
      <form class="quota-form" data-quota="${esc(n.id)}">Quota
        <input type="number" min="0" name="d" placeholder="aucun" value="${n.daily_quota_mb ?? ""}"> Mo/jour
        <input type="number" min="0" name="m" placeholder="aucun" value="${n.monthly_quota_mb ?? ""}"> Mo/mois
        <button class="ghost" type="submit">Appliquer</button>
      </form>
    </div>`;
  }).join("");
  const sel = ui.networks.find((n) => n.id === ui.selectedNet);
  $("#hist-title").textContent = sel ? `${sel.name} : 14 derniers jours` : "Consommation des 14 derniers jours";
  drawHistory(sel ? await call("get_history", { networkId: sel.id, days: 14 }) : []);
}

// ---------------------------------------------------------------------------
// Vue machines (pairs)
// ---------------------------------------------------------------------------
let personsCache = [];
function personSelect(p) {
  return `<select data-assign="${esc(p.device_id)}"><option value="">Non regroupé</option>${personsCache
    .map((x) => `<option value="${x.id}" ${p.person_id === x.id ? "selected" : ""}>${esc(x.name)}</option>`).join("")}</select>`;
}
function peerCard(p, period) {
  const b = p.totals[period] || { rx: 0, tx: 0 };
  const sinceTxt = period === "session" && p.totals.session.since ? `depuis ${fmtTime(p.totals.session.since)}` : PERIOD_LABEL[period];
  return `<div class="card peer ${p.is_self ? "self" : ""} ${p.online ? "" : "offline"}">
    <div class="peer-head"><div class="os">${osShort(p.os)}</div>
      <div style="min-width:0"><div class="peer-name">${esc(p.device_name)}${p.is_self ? " (cette machine)" : ""}</div>
      <div class="peer-meta"><span class="dot ${p.online ? "on" : ""}" style="display:inline-block;width:7px;height:7px;margin-right:4px"></span>${p.online ? "en ligne" : "vu " + fmtAgo(p.last_seen)}${p.ip ? " · " + esc(p.ip) : ""}</div></div></div>
    <div class="peer-big">${fmtBytes(b.rx + b.tx)}</div>
    <div class="muted small">${esc(sinceTxt)} · ↓ ${fmtBytes(b.rx)} · ↑ ${fmtBytes(b.tx)}</div>
    ${p.online ? `<div class="small" style="margin-top:6px">Débit : ↓ ${fmtRate(p.rate_rx)} · ↑ ${fmtRate(p.rate_tx)}</div>` : ""}
    <div class="peer-foot">${personSelect(p)}${p.is_self ? "" : `<button class="link-btn" data-forget="${esc(p.device_id)}">Oublier</button>`}</div>
  </div>`;
}
async function renderPeers() {
  const [peers, persons] = await Promise.all([call("get_peers"), call("get_persons")]);
  personsCache = persons;
  const cur = ui.snap?.network?.id;
  const here = peers.filter((p) => p.is_self || p.network_id === cur);
  const other = peers.filter((p) => !p.is_self && p.network_id !== cur);
  const online = here.filter((p) => !p.is_self && p.online).length;
  $("#peers-sub").textContent = ui.snap?.network
    ? `${online} autre${online > 1 ? "s" : ""} machine${online > 1 ? "s" : ""} en ligne sur « ${ui.snap.network.name} » avec LanPulse`
    : "Aucun réseau";
  $("#peers-grid").innerHTML = here.map((p) => peerCard(p, ui.peersPeriod)).join("") +
    (here.length === 1 ? `<div class="card empty">Aucune autre machine avec LanPulse détectée sur ce réseau.<br>Installez l'application sur un autre appareil connecté au même Wi-Fi.</div>` : "");
  $("#peers-other-wrap").classList.toggle("hidden", !other.length);
  $("#peers-other").innerHTML = other.map((p) =>
    peerCard(p, ui.peersPeriod).replace('<div class="peer-big">', `<div class="muted small" style="margin-top:8px">Réseau : ${esc(p.network_name)}</div><div class="peer-big">`)).join("");
}

// ---------------------------------------------------------------------------
// Vue personnes
// ---------------------------------------------------------------------------
async function renderPersons() {
  const [peers, persons] = await Promise.all([call("get_peers"), call("get_persons")]);
  personsCache = persons;
  const cur = ui.snap?.network?.id;
  const period = ui.personsPeriod;
  const el = $("#persons-list");
  el.innerHTML = persons.map((x) => {
    const devs = peers.filter((p) => p.person_id === x.id);
    const sum = devs.filter((p) => p.network_id === cur).reduce((a, p) => {
      const b = p.totals[period] || { rx: 0, tx: 0 };
      return { rx: a.rx + b.rx, tx: a.tx + b.tx };
    }, { rx: 0, tx: 0 });
    const live = devs.filter((p) => p.online && p.network_id === cur).reduce((a, p) => a + p.rate_rx + p.rate_tx, 0);
    return `<div class="card person">
      <div class="net-top"><h3>${esc(x.name)}</h3>
        <div><button class="link-btn" data-rename-person="${x.id}">Renommer</button> · <button class="link-btn" data-del-person="${x.id}">Supprimer</button></div></div>
      <div class="peer-big">${fmtBytes(sum.rx + sum.tx)}</div>
      <div class="muted small">${PERIOD_LABEL[period]} · ↓ ${fmtBytes(sum.rx)} · ↑ ${fmtBytes(sum.tx)}${live ? " · débit actuel " + fmtRate(live) : ""}</div>
      <div class="chips">${devs.length ? devs.map((d) => `<span class="chip"><span class="dot ${d.online ? "on" : ""}"></span>${esc(d.device_name)}<button title="Retirer" data-unassign="${esc(d.device_id)}">×</button></span>`).join("") : '<span class="muted small">Aucun appareil. Choisissez-les ci-dessous ou depuis « Machines ».</span>'}</div>
    </div>`;
  }).join("") || `<div class="card empty">Créez une personne pour regrouper ses appareils (ordinateur portable, PC de bureau…).</div>`;

  const un = peers.filter((p) => !p.person_id);
  $("#unassigned").innerHTML = un.length ? un.map((p) => `<div class="row" style="grid-template-columns:1fr auto auto">
      <span class="name">${esc(p.device_name)}${p.is_self ? " (cette machine)" : ""} <span class="muted small">· ${esc(p.os)} · ${p.online ? "en ligne" : "vu " + fmtAgo(p.last_seen)}</span></span>
      <span class="num">${fmtBytes((p.totals[period]?.rx || 0) + (p.totals[period]?.tx || 0))}</span>${personSelect(p)}</div>`).join("")
    : `<div class="empty">Tous les appareils sont regroupés.</div>`;
}

// ---------------------------------------------------------------------------
// Vue réglages
// ---------------------------------------------------------------------------
function renderSettings() {
  const s = ui.snap;
  if (!s) return;
  if (document.activeElement !== $("#device-name")) $("#device-name").value = s.device_name;
  $("#device-id").textContent = s.device_id;
  $("#capture-detail").innerHTML = s.capture.active
    ? `<span class="tag">active</span> sur l'interface <code>${esc(s.capture.interface)}</code>`
    : `<span class="tag" style="background:var(--warn-bg);color:var(--warn-ink)">inactive</span> ${esc(s.capture.message)}`;
}

// ---------------------------------------------------------------------------
// Navigation et événements
// ---------------------------------------------------------------------------
const RENDER = { live: () => ui.snap && renderLive(ui.snap), apps: renderApps, networks: renderNetworks, peers: renderPeers, persons: renderPersons, settings: renderSettings };

function go(view) {
  ui.view = view;
  $$("nav button").forEach((b) => b.classList.toggle("active", b.dataset.view === view));
  $$(".view").forEach((v) => v.classList.toggle("active", v.id === "view-" + view));
  RENDER[view]?.();
}

document.addEventListener("click", async (e) => {
  const t = e.target.closest("button, a, .net");
  if (!t) return;
  if (t.matches("nav button")) return go(t.dataset.view);
  if (t.dataset.go) return go(t.dataset.go);

  const seg = t.closest(".seg");
  if (seg && t.tagName === "BUTTON") {
    $$("button", seg).forEach((b) => b.classList.toggle("on", b === t));
    const v = t.dataset.v;
    if (seg.id === "apps-period") ui.appsPeriod = v;
    else if (seg.id === "apps-scope") ui.appsScope = v;
    else if (seg.dataset.target === "peers") ui.peersPeriod = v;
    else if (seg.dataset.target === "persons") ui.personsPeriod = v;
    return RENDER[ui.view]();
  }
  if (t.dataset.rename) {
    e.stopPropagation();
    const n = ui.networks.find((x) => x.id === t.dataset.rename);
    const name = prompt("Nouveau nom du réseau", n?.name || "");
    if (name && name.trim()) { await call("rename_network", { networkId: t.dataset.rename, name }); renderNetworks(); }
    return;
  }
  if (t.classList.contains("net") && !e.target.closest("form")) { ui.selectedNet = t.dataset.id; return renderNetworks(); }
  if (t.dataset.forget) {
    if (confirm("Oublier cette machine ? Elle réapparaîtra si elle s'annonce de nouveau.")) { await call("forget_peer", { deviceId: t.dataset.forget }); renderPeers(); }
    return;
  }
  if (t.dataset.unassign) { await call("assign_device", { deviceId: t.dataset.unassign, personId: null }); return renderPersons(); }
  if (t.dataset.renamePerson) {
    const p = personsCache.find((x) => x.id === +t.dataset.renamePerson);
    const name = prompt("Nouveau nom", p?.name || "");
    if (name && name.trim()) { await call("rename_person", { id: +t.dataset.renamePerson, name }); renderPersons(); }
    return;
  }
  if (t.dataset.delPerson) {
    if (confirm("Supprimer cette personne ? Ses appareils redeviennent non regroupés.")) { await call("delete_person", { id: +t.dataset.delPerson }); renderPersons(); }
    return;
  }
  if (t.id === "save-name") {
    await call("set_device_name", { name: $("#device-name").value });
    toast("Nom enregistré");
  }
});

document.addEventListener("change", async (e) => {
  const s = e.target;
  if (s.dataset.assign !== undefined) {
    await call("assign_device", { deviceId: s.dataset.assign, personId: s.value ? +s.value : null });
    toast("Regroupement enregistré");
    RENDER[ui.view]();
  }
});

document.addEventListener("submit", async (e) => {
  e.preventDefault();
  const f = e.target;
  if (f.id === "person-form") {
    const name = $("#person-name").value;
    await call("create_person", { name });
    $("#person-name").value = "";
    return renderPersons();
  }
  if (f.dataset.quota) {
    const v = (x) => (x.value === "" ? null : Math.max(0, parseInt(x.value, 10)));
    await call("set_quota", { networkId: f.dataset.quota, dailyMb: v(f.d), monthlyMb: v(f.m) });
    toast("Quota enregistré");
    loadNetworks();
  }
});

window.addEventListener("resize", () => {
  if (ui.view === "live" && ui.snap) drawLive(ui.snap.history);
  if (ui.view === "networks") renderNetworks();
});

// ---------------------------------------------------------------------------
// Démarrage
// ---------------------------------------------------------------------------
async function start() {
  renderLive(await call("get_snapshot"));
  loadNetworks();
  listen("tick", (ev) => {
    renderLive(ev.payload);
    if (ui.view === "settings") renderSettings();
  });
  listen("network-changed", () => { loadNetworks(); RENDER[ui.view](); });
  // Rafraîchissement périodique des vues alimentées par la base
  setInterval(() => {
    if (document.hidden) return;
    if (ui.view === "peers" && !document.activeElement?.matches("select")) renderPeers();
    if (ui.view === "persons" && !document.activeElement?.matches("select, input")) renderPersons();
    if (ui.view === "apps") renderApps();
  }, 5000);
  setInterval(loadNetworks, 30000);
}
// Lancé après l'évaluation complète du script (le mode démo est défini plus bas).
document.addEventListener("DOMContentLoaded", start);

// ---------------------------------------------------------------------------
// Mode démonstration (ouverture de index.html dans un navigateur, sans Tauri)
// ---------------------------------------------------------------------------
var Demo = (() => {
  const now = () => Math.floor(Date.now() / 1000);
  const MB = 1048576;
  const hist = [];
  for (let i = 0; i < 300; i++) hist.push([now() - 300 + i, (120 + 90 * Math.sin(i / 14) + Math.random() * 60) * 1024, (25 + Math.random() * 20) * 1024]);
  const net = { id: "gw-a4b1c2d3e4f5", name: "PAK-DSI", ssid: "PAK-DSI", kind: "wifi", interface: "wlan0", local_ipv4: "192.168.1.24", broadcast_ipv4: "192.168.1.255", gateway_ip: "192.168.1.1", gateway_mac: "a4:b1:c2:d3:e4:f5" };
  const T = (s, d, w, m) => ({ session: { rx: s * MB, tx: s * MB * .12, since: now() - 7400, until: null }, day: { rx: d * MB, tx: d * MB * .1 }, week: { rx: w * MB, tx: w * MB * .1 }, month: { rx: m * MB, tx: m * MB * .1 } });
  const persons = [{ id: 1, name: "Yvan" }, { id: 2, name: "Mme Ndongo" }];
  const assign = { self: 1, p1: 1, p2: 2 };
  const peers = () => [
    { device_id: "self", device_name: "PC-Yvan", os: "Windows", network_id: net.id, network_name: net.name, online: true, last_seen: now(), ip: "192.168.1.24", rate_rx: 180000, rate_tx: 30000, totals: T(412, 860, 4200, 15800), is_self: true },
    { device_id: "p1", device_name: "MacBook-Yvan", os: "macOS", network_id: net.id, network_name: net.name, online: true, last_seen: now(), ip: "192.168.1.31", rate_rx: 52000, rate_tx: 8000, totals: T(95, 210, 1300, 5100), is_self: false },
    { device_id: "p2", device_name: "poste-compta", os: "Linux", network_id: net.id, network_name: net.name, online: true, last_seen: now(), ip: "192.168.1.40", rate_rx: 640000, rate_tx: 120000, totals: T(1320, 2410, 9800, 31000), is_self: false },
    { device_id: "p3", device_name: "Laptop-stagiaire", os: "Windows", network_id: net.id, network_name: net.name, online: false, last_seen: now() - 3600 * 5, ip: null, rate_rx: 0, rate_tx: 0, totals: T(220, 0, 640, 2900), is_self: false },
    { device_id: "p4", device_name: "PC-maison", os: "Linux", network_id: "gw-other", network_name: "Maison-Orange", online: false, last_seen: now() - 86400 * 2, ip: null, rate_rx: 0, rate_tx: 0, totals: T(540, 0, 900, 3200), is_self: false },
  ].map((p) => ({ ...p, person_id: assign[p.device_id] ?? null }));
  const apps = [["chrome", 380, 40], ["Teams", 210, 90], ["OneDrive", 140, 220], ["Code", 60, 8], ["svchost.exe", 45, 12], ["Spotify", 38, 2], ["Système / non identifié", 22, 6]];
  return {
    invoke: async (cmd, a) => {
      switch (cmd) {
        case "get_snapshot": return { device_id: "6b1e9c2a-demo", device_name: "PC-Yvan", os: "Windows", network: net, rate_rx: hist.at(-1)[1], rate_tx: hist.at(-1)[2], history: hist, apps: apps.map(([n, r, t]) => ({ name: n, rx: r * 900, tx: t * 900 })), protocols: [{ name: "TCP", rx: 160000, tx: 26000 }, { name: "UDP", rx: 22000, tx: 5200 }, { name: "ICMP", rx: 120, tx: 120 }], capture: { active: true, message: "Capture active", interface: "wlan0" }, totals: T(412, 860, 4200, 15800), peers_online: 2 };
        case "get_peers": return peers();
        case "get_persons": return persons;
        case "get_networks": return [{ ...net, first_seen: now() - 86400 * 30, last_seen: now(), daily_quota_mb: 2048, monthly_quota_mb: 40960, totals: T(412, 860, 4200, 15800), sessions: 42 }, { id: "gw-other", name: "Maison-Orange", ssid: "Maison-Orange", kind: "wifi", gateway_ip: "192.168.0.1", first_seen: now() - 86400 * 60, last_seen: now() - 86400 * 2, daily_quota_mb: null, monthly_quota_mb: 20480, totals: T(310, 0, 0, 5400), sessions: 18 }];
        case "get_history": return Array.from({ length: 14 }, (_, i) => { const d = new Date(); d.setDate(d.getDate() - i); return { day: d.toLocaleDateString("sv-SE"), rx: (600 + Math.random() * 900) * MB, tx: (60 + Math.random() * 120) * MB }; });
        case "get_app_usage": return apps.map(([n, r, t]) => ({ app: n, rx: r * MB * 3, tx: t * MB * 3 }));
        case "assign_device": assign[a.deviceId] = a.personId; return null;
        case "create_person": persons.push({ id: persons.length + 1, name: a.name }); return persons.length;
        default: return null;
      }
    },
    listen: (ev, cb) => {
      if (ev !== "tick") return;
      setInterval(async () => {
        hist.push([now(), (120 + 90 * Math.sin(Date.now() / 14000) + Math.random() * 60) * 1024, (25 + Math.random() * 20) * 1024]);
        hist.shift();
        cb({ payload: await Demo.invoke("get_snapshot") });
      }, 1000);
    },
  };
})();
