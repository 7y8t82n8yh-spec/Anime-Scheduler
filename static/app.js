"use strict";

const $ = (sel) => document.querySelector(sel);
const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

const STATUS_LABELS = {
  watching: "Watching",
  planning: "Plan to watch",
  paused: "Paused",
  completed: "Completed",
  dropped: "Dropped",
};
const ACTIVE = ["watching", "planning", "paused"];

let anime = [];
let tab = localStorage.getItem("tab") || "active";

async function api(method, path, body) {
  const res = await fetch(path, {
    method,
    headers: body !== undefined ? { "Content-Type": "application/json" } : {},
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (res.status === 204) return null;
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
  return data;
}

let toastTimer;
function toast(msg, isError = false) {
  const t = $("#toast");
  t.textContent = msg;
  t.className = "toast" + (isError ? " error" : "");
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), isError ? 7000 : 3500);
}

// ---------- formatting ----------

function fmtDate(iso, withTime = true) {
  const d = new Date(iso);
  const opts = { weekday: "short", month: "short", day: "numeric" };
  if (d.getFullYear() !== new Date().getFullYear()) opts.year = "numeric";
  if (withTime) Object.assign(opts, { hour: "2-digit", minute: "2-digit" });
  return d.toLocaleString(undefined, opts);
}

function fmtCountdown(airingAt) {
  let s = Math.max(0, Math.floor((new Date(airingAt) - Date.now()) / 1000));
  const d = Math.floor(s / 86400);
  s -= d * 86400;
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

function availabilityHtml(a) {
  const av = a.availability;
  switch (av.kind) {
    case "available":
      return av.count === 1
        ? `<div class="avail available">▶ Episode ${av.from} is available</div>`
        : `<div class="avail available">▶ Episodes ${av.from}–${av.to} available (${av.count})</div>`;
    case "upcoming":
      return `<div class="avail upcoming">⏳ Episode ${av.episode} airs ${esc(fmtDate(av.airing_at))} · in ${fmtCountdown(av.airing_at)}</div>`;
    case "all_watched":
      return `<div class="avail done">✓ All episodes watched</div>`;
    default:
      if (av.hiatus) return `<div class="avail nodate">⏸ On hiatus — no next date announced</div>`;
      if (av.start_date) {
        const future = new Date(av.start_date) > new Date();
        return `<div class="avail nodate">📅 ${future ? "Starts" : "Started"} ${esc(fmtDate(av.start_date, false))} — exact episode date not announced</div>`;
      }
      return `<div class="avail nodate">📅 No air date announced yet</div>`;
  }
}

function sortKey(a) {
  const av = a.availability;
  if (av.kind === "available") return [0, -av.count];
  if (av.kind === "upcoming") return [1, new Date(av.airing_at).getTime()];
  if (av.kind === "no_date") return [2, av.start_date ? new Date(av.start_date).getTime() : Infinity];
  return [3, 0];
}

// ---------- list rendering ----------

function filtered() {
  let list = anime;
  if (tab === "active") list = anime.filter((a) => ACTIVE.includes(a.status));
  else if (tab !== "all") list = anime.filter((a) => a.status === tab);
  return [...list].sort((x, y) => {
    const [a1, a2] = sortKey(x);
    const [b1, b2] = sortKey(y);
    return a1 - b1 || a2 - b2 || x.title.localeCompare(y.title);
  });
}

function renderTabs() {
  for (const b of document.querySelectorAll("#tabs button")) {
    const t = b.dataset.tab;
    const n =
      t === "all" ? anime.length : t === "active" ? anime.filter((a) => ACTIVE.includes(a.status)).length : anime.filter((a) => a.status === t).length;
    b.classList.toggle("active", t === tab);
    const label = b.dataset.label || (b.dataset.label = b.textContent);
    b.innerHTML = `${esc(label)}<span class="count">${n}</span>`;
  }
}

function card(a) {
  const total = a.airing.total_episodes;
  const aired = a.airing.latest_aired;
  const denom = total || Math.max(aired, a.watched, 1);
  const watchedPct = Math.min(100, (a.watched / denom) * 100);
  const airedPct = Math.min(100, (aired / denom) * 100);
  const sub = [a.airing.format, a.airing.season, total ? `${total} eps` : null, a.airing.media_status?.replaceAll("_", " ").toLowerCase()]
    .filter(Boolean)
    .join(" · ");
  const next = a.watched + 1;
  const canWatchNext = !total || a.watched < total;
  const statusOpts = Object.entries(STATUS_LABELS)
    .map(([v, l]) => `<option value="${v}" ${v === a.status ? "selected" : ""}>${l}</option>`)
    .join("");
  const log = [...a.watch_log]
    .reverse()
    .map((w) => `<li>Episode ${w.episode} — ${esc(fmtDate(w.watched_at))}</li>`)
    .join("");
  const nowLocal = new Date(Date.now() - new Date().getTimezoneOffset() * 60000).toISOString().slice(0, 16);

  return `
  <article class="card" data-id="${a.anilist_id}">
    ${a.cover ? `<img class="cover" src="${esc(a.cover)}" alt="" loading="lazy">` : ""}
    <div class="body">
      <h3><a href="${esc(a.site_url || "#")}" target="_blank" rel="noopener">${esc(a.title)}</a></h3>
      <div class="sub">${esc(sub)}${a.title_english && a.title_english !== a.title ? ` · ${esc(a.title_english)}` : ""}</div>
      ${availabilityHtml(a)}
      <div class="sub">Watched <b>${a.watched}</b>${total ? ` / ${total}` : ""} · aired ${aired}${total ? ` / ${total}` : ""}</div>
      <div class="progress" title="dark: watched, light: aired"><div class="aired" style="width:${airedPct}%"></div><div style="width:${watchedPct}%"></div></div>
      <div class="controls">
        ${canWatchNext ? `<button class="primary" data-act="next">✓ Watched ep ${next}</button>` : ""}
        <button data-act="undo" ${a.watched === 0 ? "disabled" : ""} title="Un-watch last episode">−1</button>
        <label class="sub">Progress <input type="number" min="0" ${total ? `max="${total}"` : ""} value="${a.watched}" data-act="progress"></label>
        <select data-act="status">${statusOpts}</select>
        <button class="danger" data-act="remove" title="Remove from list">🗑</button>
      </div>
      <details class="log">
        <summary>Watch history (${a.watch_log.length})</summary>
        <form data-act="log">
          <span class="sub">Watched episode</span>
          <input type="number" name="episode" min="1" ${total ? `max="${total}"` : ""} value="${canWatchNext ? next : a.watched}" required>
          <span class="sub">on</span>
          <input type="datetime-local" name="watched_at" value="${nowLocal}" required>
          <button type="submit">Save</button>
        </form>
        <ul>${log || "<li>Nothing logged yet</li>"}</ul>
      </details>
    </div>
  </article>`;
}

function render() {
  renderTabs();
  const list = filtered();
  const openLogs = new Set([...document.querySelectorAll("details.log[open]")].map((d) => d.closest(".card").dataset.id));
  $("#list").innerHTML = list.length
    ? list.map(card).join("")
    : `<div class="empty">${anime.length ? "Nothing here." : "Your list is empty — search above to add an anime, or import from MyAnimeList."}</div>`;
  for (const id of openLogs) $(`.card[data-id="${id}"] details.log`)?.setAttribute("open", "");
}

async function loadAnime() {
  try {
    anime = await api("GET", "/api/anime");
    render();
  } catch (e) {
    toast(e.message, true);
  }
}

function applyUpdate(resp) {
  const i = anime.findIndex((a) => a.anilist_id === resp.anime.anilist_id);
  if (i >= 0) anime[i] = resp.anime;
  else anime.push(resp.anime);
  render();
  if (resp.mal_error) toast(`Saved locally, but MyAnimeList sync failed: ${resp.mal_error}`, true);
}

$("#list").addEventListener("click", async (ev) => {
  const btn = ev.target.closest("button[data-act]");
  if (!btn) return;
  const id = Number(btn.closest(".card").dataset.id);
  const a = anime.find((x) => x.anilist_id === id);
  try {
    if (btn.dataset.act === "next") {
      applyUpdate(await api("POST", `/api/anime/${id}/watch`, {}));
    } else if (btn.dataset.act === "undo") {
      applyUpdate(await api("PATCH", `/api/anime/${id}`, { watched: Math.max(0, a.watched - 1) }));
    } else if (btn.dataset.act === "remove") {
      if (!confirm(`Remove "${a.title}" from your list?`)) return;
      await api("DELETE", `/api/anime/${id}`);
      anime = anime.filter((x) => x.anilist_id !== id);
      render();
    }
  } catch (e) {
    toast(e.message, true);
  }
});

$("#list").addEventListener("change", async (ev) => {
  const el = ev.target.closest("[data-act]");
  if (!el || el.tagName === "FORM" || !el.closest(".controls")) return;
  const id = Number(el.closest(".card").dataset.id);
  try {
    if (el.dataset.act === "status") applyUpdate(await api("PATCH", `/api/anime/${id}`, { status: el.value }));
    if (el.dataset.act === "progress") applyUpdate(await api("PATCH", `/api/anime/${id}`, { watched: Math.max(0, Number(el.value) || 0) }));
  } catch (e) {
    toast(e.message, true);
  }
});

$("#list").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const form = ev.target;
  const id = Number(form.closest(".card").dataset.id);
  const fd = new FormData(form);
  try {
    applyUpdate(
      await api("POST", `/api/anime/${id}/watch`, {
        episode: Number(fd.get("episode")),
        watched_at: new Date(fd.get("watched_at")).toISOString(),
      }),
    );
    toast("Logged");
  } catch (e) {
    toast(e.message, true);
  }
});

$("#tabs").addEventListener("click", (ev) => {
  const b = ev.target.closest("button[data-tab]");
  if (!b) return;
  tab = b.dataset.tab;
  localStorage.setItem("tab", tab);
  render();
});

// ---------- search ----------

$("#search-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const q = $("#search-input").value.trim();
  const box = $("#search-results");
  if (!q) return (box.innerHTML = "");
  box.innerHTML = `<div class="sub">Searching…</div>`;
  try {
    const results = await api("GET", `/api/search?q=${encodeURIComponent(q)}`);
    box.innerHTML =
      results
        .map(
          (r) => `
      <div class="search-item" data-id="${r.anilist_id}">
        ${r.cover ? `<img src="${esc(r.cover)}" alt="">` : ""}
        <div class="meta">
          <b>${esc(r.title)}</b>${r.title_english && r.title_english !== r.title ? ` <span class="sub">(${esc(r.title_english)})</span>` : ""}
          <div class="sub">${esc([r.format, r.season, r.episodes ? `${r.episodes} eps` : null, r.media_status?.replaceAll("_", " ").toLowerCase()].filter(Boolean).join(" · "))}</div>
        </div>
        <div class="actions">
          ${
            r.in_list
              ? `<span class="sub">In your list</span>`
              : `<button data-add="planning">+ Plan to watch</button><button data-add="watching">+ Watching</button>`
          }
        </div>
      </div>`,
        )
        .join("") || `<div class="sub">No results.</div>`;
  } catch (e) {
    box.innerHTML = "";
    toast(e.message, true);
  }
});

$("#search-results").addEventListener("click", async (ev) => {
  const b = ev.target.closest("button[data-add]");
  if (!b) return;
  const item = b.closest(".search-item");
  b.disabled = true;
  try {
    const resp = await api("POST", "/api/anime", { anilist_id: Number(item.dataset.id), status: b.dataset.add });
    applyUpdate(resp);
    item.querySelector(".actions").innerHTML = `<span class="sub">Added ✓</span>`;
  } catch (e) {
    b.disabled = false;
    toast(e.message, true);
  }
});

// ---------- refresh ----------

$("#refresh-btn").addEventListener("click", async (ev) => {
  const b = ev.currentTarget;
  b.disabled = true;
  try {
    const r = await api("POST", "/api/refresh");
    await loadAnime();
    await loadNotifications();
    toast(r.new_notifications ? `${r.new_notifications} new episode alert(s)` : "Up to date");
  } catch (e) {
    toast(e.message, true);
  } finally {
    b.disabled = false;
  }
});

// ---------- notifications ----------

function lastAlerted() {
  const v = localStorage.getItem("lastAlertedId");
  return v === null ? null : Number(v);
}

async function loadNotifications() {
  let data;
  try {
    data = await api("GET", "/api/notifications");
  } catch {
    return;
  }
  const count = $("#bell-count");
  count.textContent = data.unread;
  count.hidden = data.unread === 0;
  document.title = data.unread ? `(${data.unread}) Anime Scheduler` : "Anime Scheduler";

  $("#notif-list").innerHTML =
    data.notifications
      .map(
        (n) => `
      <li class="${n.read ? "" : "unread"}">
        ${n.cover ? `<img src="${esc(n.cover)}" alt="">` : ""}
        <div><b>${esc(n.title)}</b><div>${esc(n.message)}</div><div class="sub">${esc(fmtDate(n.created_at))}</div></div>
      </li>`,
      )
      .join("") || `<li class="sub">No notifications yet. You'll be told here when a new episode drops.</li>`;

  // Browser (desktop) notifications for anything new since we last looked.
  const maxId = data.notifications.reduce((m, n) => Math.max(m, n.id), -1);
  const prev = lastAlerted();
  if (prev !== null && "Notification" in window && Notification.permission === "granted") {
    for (const n of data.notifications.filter((n) => !n.read && n.id > prev).reverse()) {
      const note = new Notification(n.title, { body: n.message, icon: n.cover || undefined, tag: `anime-${n.id}` });
      note.onclick = () => window.focus();
    }
  }
  if (prev === null || maxId > prev) localStorage.setItem("lastAlertedId", String(maxId));

  // New episodes may have arrived: keep the list in sync.
  if (prev !== null && maxId > prev) loadAnime();
}

function updateEnableButton() {
  $("#notif-enable").hidden = !("Notification" in window) || Notification.permission !== "default";
}

$("#notif-enable").addEventListener("click", async () => {
  await Notification.requestPermission();
  updateEnableButton();
});
$("#notif-read").addEventListener("click", async () => {
  await api("POST", "/api/notifications/read", {});
  loadNotifications();
});
$("#notif-clear").addEventListener("click", async () => {
  if (!confirm("Delete all notifications?")) return;
  await api("DELETE", "/api/notifications");
  loadNotifications();
});

function togglePanel(id) {
  for (const p of document.querySelectorAll(".panel")) if (p.id !== id) p.hidden = true;
  const p = document.getElementById(id);
  p.hidden = !p.hidden;
  return !p.hidden;
}
document.addEventListener("click", (ev) => {
  const c = ev.target.closest("[data-close]");
  if (c) document.getElementById(c.dataset.close).hidden = true;
});
$("#bell-btn").addEventListener("click", () => {
  updateEnableButton();
  togglePanel("notif-panel");
});

// ---------- MyAnimeList ----------

async function renderMal() {
  const body = $("#mal-body");
  let s;
  try {
    s = await api("GET", "/api/mal");
  } catch (e) {
    body.textContent = e.message;
    return;
  }
  if (!s.configured) {
    body.innerHTML = `
      <p>MyAnimeList integration is off. To enable it:</p>
      <ol>
        <li>Create an API client at <a href="https://myanimelist.net/apiconfig" target="_blank" rel="noopener">myanimelist.net/apiconfig</a>.</li>
        <li>Set the App Redirect URL to <code>${esc(s.redirect_uri)}</code>.</li>
        <li>Restart this app with <code>MAL_CLIENT_ID</code> (and <code>MAL_CLIENT_SECRET</code> for "web" apps) set.</li>
      </ol>`;
    return;
  }
  const statusBoxes = Object.entries(STATUS_LABELS)
    .map(([v, l]) => `<label><input type="checkbox" name="status" value="${v}" ${ACTIVE.includes(v) ? "checked" : ""}> ${l}</label>`)
    .join("");
  body.innerHTML = `
    ${
      s.connected
        ? `<p>Connected as <b>${esc(s.username || "your account")}</b>. Progress you log here is synced to MyAnimeList.</p>
           <button id="mal-disconnect">Disconnect</button>`
        : `<p>Connect your account to sync watched episodes back to MyAnimeList.</p>
           <a href="/auth/mal/login"><button class="primary" type="button">Connect MyAnimeList</button></a>`
    }
    <hr>
    <form id="mal-import">
      <b>Import list</b>
      <input name="username" placeholder="${s.connected ? "Username (blank = your account)" : "MyAnimeList username (public list)"}">
      <div>${statusBoxes}</div>
      <button class="primary" type="submit">Import</button>
      <div id="mal-import-result" class="sub"></div>
    </form>`;
  $("#mal-disconnect")?.addEventListener("click", async () => {
    await api("POST", "/api/mal/disconnect");
    renderMal();
  });
  $("#mal-import").addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const fd = new FormData(ev.target);
    const out = $("#mal-import-result");
    const btn = ev.target.querySelector("button[type=submit]");
    btn.disabled = true;
    out.textContent = "Importing… (this can take a moment for big lists)";
    try {
      const r = await api("POST", "/api/mal/import", { username: fd.get("username") || null, statuses: fd.getAll("status") });
      out.innerHTML =
        `Added ${r.added}, updated ${r.updated}.` + (r.unmatched.length ? ` Couldn't find on AniList: ${esc(r.unmatched.join(", "))}` : "");
      loadAnime();
    } catch (e) {
      out.textContent = e.message;
    } finally {
      btn.disabled = false;
    }
  });
}

$("#mal-btn").addEventListener("click", () => {
  if (togglePanel("mal-panel")) renderMal();
});

// ---------- boot ----------

if (new URLSearchParams(location.search).get("mal") === "connected") {
  history.replaceState(null, "", "/");
  toast("MyAnimeList connected");
  togglePanel("mal-panel");
  renderMal();
}

loadAnime();
loadNotifications();
setInterval(loadNotifications, 60_000);
// Keep countdowns fresh, and reload when an upcoming episode should have aired.
setInterval(() => {
  const due = anime.some((a) => a.availability.kind === "upcoming" && new Date(a.availability.airing_at) < Date.now() - 3 * 60_000);
  if (due) loadAnime();
  else if (!document.querySelector(".card :focus")) render();
}, 60_000);
