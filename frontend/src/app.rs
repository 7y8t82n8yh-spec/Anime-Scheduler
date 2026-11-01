//! The Leptos UI.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use anime_shared::*;
use chrono::{DateTime, Utc};
use gloo_timers::callback::{Interval, Timeout};
use leptos::{ev::SubmitEvent, prelude::*, task::spawn_local};

use crate::{api, browser};

// ---------------------------------------------------------------------------
// Shared UI state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    UpNext,
    Status(WatchStatus),
    All,
}

impl Tab {
    fn matches(self, s: WatchStatus) -> bool {
        match self {
            Tab::UpNext => s.is_active(),
            Tab::Status(t) => t == s,
            Tab::All => true,
        }
    }
    fn key(self) -> &'static str {
        match self {
            Tab::UpNext => "up_next",
            Tab::Status(s) => s.as_str(),
            Tab::All => "all",
        }
    }
    fn from_key(k: &str) -> Self {
        match k {
            "all" => Tab::All,
            k => WatchStatus::parse(k)
                .map(Tab::Status)
                .unwrap_or(Tab::UpNext),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Tab::UpNext => "Up next",
            Tab::Status(s) => s.label(),
            Tab::All => "All",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Notifications,
    Mal,
}

#[derive(Clone)]
struct Toast {
    id: u64,
    message: String,
    error: bool,
}

/// App-wide signals, provided via context.
#[derive(Clone, Copy)]
struct Ctx {
    anime: RwSignal<Vec<Anime>>,
    now: RwSignal<DateTime<Utc>>,
    toast: RwSignal<Option<Toast>>,
    notifications: RwSignal<Option<NotificationsResp>>,
    panel: RwSignal<Option<Panel>>,
    open_logs: RwSignal<HashSet<i64>>,
}

impl Ctx {
    fn toast(&self, message: impl Into<String>, error: bool) {
        // Ever-increasing, so an old toast's timer can never clear a newer toast.
        static NEXT_TOAST_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_TOAST_ID.fetch_add(1, Ordering::Relaxed);
        self.toast.set(Some(Toast {
            id,
            message: message.into(),
            error,
        }));
        let toast = self.toast;
        Timeout::new(if error { 7000 } else { 3500 }, move || {
            if toast.with_untracked(|t| t.as_ref().is_some_and(|t| t.id == id)) {
                toast.set(None);
            }
        })
        .forget();
    }

    fn error(&self, e: String) {
        self.toast(e, true);
    }

    /// Replace (or insert) an anime after a successful update.
    fn apply(&self, resp: UpdateResp) {
        let a = resp.anime.anime;
        self.anime.update(
            |list| match list.iter_mut().find(|x| x.anilist_id == a.anilist_id) {
                Some(slot) => *slot = a,
                None => list.push(a),
            },
        );
        if let Some(e) = resp.mal_error {
            self.error(format!("Saved locally, but MyAnimeList sync failed: {e}"));
        }
    }

    async fn reload_anime(self) {
        match api::list_anime().await {
            Ok(list) => self.anime.set(list.into_iter().map(|v| v.anime).collect()),
            Err(e) => self.error(e),
        }
    }

    /// Fetch notifications; pop desktop notifications for ones we haven't alerted yet.
    async fn reload_notifications(self) {
        let Ok(data) = api::notifications().await else {
            return;
        };
        browser::set_title(&if data.unread > 0 {
            format!("({}) Anime Scheduler", data.unread)
        } else {
            "Anime Scheduler".into()
        });

        const KEY: &str = "lastAlertedId";
        let prev = browser::storage_get(KEY).and_then(|v| v.parse::<i64>().ok());
        let (alerts, high_water) = pending_alerts(&data.notifications, prev);
        for n in &alerts {
            browser::show_notification(n);
        }
        browser::storage_set(KEY, &high_water.to_string());
        let new_arrived = prev.is_some_and(|p| high_water > p);
        self.notifications.set(Some(data));
        if new_arrived {
            self.reload_anime().await;
        }
    }

    /// Send a partial update (progress and/or status) for one anime.
    fn patch(self, id: i64, req: UpdateReq) {
        self.update_with(api::update_anime(id, req));
    }

    /// Run an API call that returns an updated anime.
    fn update_with<F>(self, fut: F)
    where
        F: std::future::Future<Output = api::ApiResult<UpdateResp>> + 'static,
    {
        self.update_with_message(fut, None);
    }

    /// Like [`Ctx::update_with`], showing `success` once the server confirms.
    fn update_with_message<F>(self, fut: F, success: Option<String>)
    where
        F: std::future::Future<Output = api::ApiResult<UpdateResp>> + 'static,
    {
        spawn_local(async move {
            match fut.await {
                Ok(r) => {
                    let mal_failed = r.mal_error.is_some();
                    self.apply(r);
                    // Don't hide a MAL sync error toast behind the success message.
                    if let (Some(msg), false) = (success, mal_failed) {
                        self.toast(msg, false);
                    }
                }
                Err(e) => self.error(e),
            }
        });
    }
}

fn media_meta(parts: &[Option<String>]) -> String {
    parts
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn pretty_status(s: &Option<String>) -> Option<String> {
    s.as_ref().map(|s| s.replace('_', " ").to_lowercase())
}

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

#[component]
pub fn App() -> impl IntoView {
    let ctx = Ctx {
        anime: RwSignal::new(Vec::new()),
        now: RwSignal::new(Utc::now()),
        toast: RwSignal::new(None),
        notifications: RwSignal::new(None),
        panel: RwSignal::new(None),
        open_logs: RwSignal::new(HashSet::new()),
    };
    provide_context(ctx);

    if browser::take_mal_connected_flag() {
        ctx.toast("MyAnimeList connected", false);
        ctx.panel.set(Some(Panel::Mal));
    }

    spawn_local(ctx.reload_anime());
    spawn_local(ctx.reload_notifications());
    Interval::new(60_000, move || spawn_local(ctx.reload_notifications())).forget();
    // Keep countdowns fresh; reload once an announced episode should have aired.
    Interval::new(30_000, move || {
        let now = Utc::now();
        ctx.now.set(now);
        // Only active shows (the server refreshes those right after air time), and not
        // long-overdue ones (AniList stopped updating them), to avoid a reload loop.
        let overdue = ctx.anime.with_untracked(|list| {
            list.iter().filter(|a| a.status.is_active()).any(|a| {
                a.airing.next_airing_at.is_some_and(|t| {
                    t < now - chrono::Duration::minutes(3) && t > now - chrono::Duration::hours(1)
                })
            })
        });
        if overdue {
            spawn_local(ctx.reload_anime());
        }
    })
    .forget();

    view! {
        <Header/>
        <main>
            <Search/>
            <AnimeList/>
        </main>
        <NotificationsPanel/>
        <MalPanel/>
        <ToastView/>
    }
}

#[component]
fn Header() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let refreshing = RwSignal::new(false);
    let refresh = move |_| {
        refreshing.set(true);
        spawn_local(async move {
            match api::refresh().await {
                Ok(r) => {
                    ctx.reload_anime().await;
                    ctx.reload_notifications().await;
                    ctx.toast(
                        match r.new_notifications {
                            0 => "Up to date".to_string(),
                            n => format!("{n} new episode alert(s)"),
                        },
                        false,
                    );
                }
                Err(e) => ctx.error(e),
            }
            refreshing.set(false);
        });
    };
    let toggle = move |p: Panel| {
        ctx.panel
            .update(|cur| *cur = if *cur == Some(p) { None } else { Some(p) })
    };
    let unread = move || {
        ctx.notifications
            .with(|n| n.as_ref().map_or(0, |n| n.unread))
    };

    view! {
        <header class="top">
            <h1>"📺 Anime Scheduler"</h1>
            <div class="top-actions">
                <button on:click=refresh disabled=refreshing title="Check for new episodes now">
                    "⟳ Refresh"
                </button>
                <button on:click=move |_| toggle(Panel::Mal) title="MyAnimeList">"MAL"</button>
                <button class="bell" on:click=move |_| toggle(Panel::Notifications) title="Notifications">
                    "🔔"
                    <Show when=move || { unread() > 0 }>
                        <span class="badge">{unread}</span>
                    </Show>
                </button>
            </div>
        </header>
    }
}

#[component]
fn ToastView() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    move || {
        ctx.toast.get().map(|t| {
            view! { <div class=if t.error { "toast error" } else { "toast" }>{t.message}</div> }
        })
    }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[component]
fn Search() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let query = RwSignal::new(String::new());
    let results = RwSignal::new(None::<Vec<SearchResult>>);
    let searching = RwSignal::new(false);

    let on_submit = move |ev: SubmitEvent| {
        ev.prevent_default();
        let q = query.get_untracked().trim().to_string();
        if q.is_empty() {
            results.set(None);
            return;
        }
        searching.set(true);
        spawn_local(async move {
            match api::search(&q).await {
                Ok(r) => results.set(Some(r)),
                Err(e) => ctx.error(e),
            }
            searching.set(false);
        });
    };

    let add = move |id: i64, status: WatchStatus| {
        spawn_local(async move {
            match api::add_anime(id, status).await {
                Ok(r) => {
                    ctx.toast(format!("Added {}", r.anime.anime.title), false);
                    ctx.apply(r);
                    results.update(|rs| {
                        if let Some(r) = rs.iter_mut().flatten().find(|r| r.anilist_id == id) {
                            r.in_list = true;
                        }
                    });
                }
                Err(e) => ctx.error(e),
            }
        });
    };

    view! {
        <section class="add">
            <form on:submit=on_submit>
                <input type="search" placeholder="Search AniList to add an anime…" autocomplete="off" bind:value=query/>
                <button type="submit" disabled=searching>"Search"</button>
            </form>
            <div class="search-results">
                {move || searching.get().then(|| view! { <div class="sub">"Searching…"</div> })}
                {move || {
                    results.get().map(|rs| {
                        if rs.is_empty() {
                            return view! { <div class="sub">"No results."</div> }.into_any();
                        }
                        rs.into_iter()
                            .map(|r| {
                                let id = r.anilist_id;
                                let meta = media_meta(&[
                                    r.format.clone(),
                                    r.season.clone(),
                                    r.episodes.map(|e| format!("{e} eps")),
                                    pretty_status(&r.media_status),
                                ]);
                                let english = r.title_english.clone().filter(|e| *e != r.title);
                                view! {
                                    <div class="search-item">
                                        {r.cover.clone().map(|c| view! { <img src=c alt=""/> })}
                                        <div class="meta">
                                            <b>{r.title.clone()}</b>
                                            {english.map(|e| view! { <span class="sub">" (" {e} ")"</span> })}
                                            <div class="sub">{meta}</div>
                                        </div>
                                        <div class="actions">
                                            {if r.in_list {
                                                view! { <span class="sub">"In your list ✓"</span> }.into_any()
                                            } else {
                                                view! {
                                                    <button on:click=move |_| add(id, WatchStatus::Planning)>"+ Plan to watch"</button>
                                                    <button on:click=move |_| add(id, WatchStatus::Watching)>"+ Watching"</button>
                                                }
                                                    .into_any()
                                            }}
                                        </div>
                                    </div>
                                }
                            })
                            .collect_view()
                            .into_any()
                    })
                }}
            </div>
        </section>
    }
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

const TABS: [Tab; 7] = [
    Tab::UpNext,
    Tab::Status(WatchStatus::Watching),
    Tab::Status(WatchStatus::Planning),
    Tab::Status(WatchStatus::Paused),
    Tab::Status(WatchStatus::Completed),
    Tab::Status(WatchStatus::Dropped),
    Tab::All,
];

#[component]
fn AnimeList() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let tab = RwSignal::new(
        browser::storage_get("tab")
            .map(|k| Tab::from_key(&k))
            .unwrap_or(Tab::UpNext),
    );

    let visible = Memo::new(move |_| {
        let now = ctx.now.get();
        let t = tab.get();
        let mut list: Vec<Anime> = ctx
            .anime
            .get()
            .into_iter()
            .filter(|a| t.matches(a.status))
            .collect();
        list.sort_by(|x, y| {
            x.availability(now)
                .sort_key()
                .cmp(&y.availability(now).sort_key())
                .then_with(|| x.title.cmp(&y.title))
        });
        list
    });

    view! {
        <nav class="tabs">
            {TABS
                .into_iter()
                .map(|t| {
                    let count = move || ctx.anime.with(|l| l.iter().filter(|a| t.matches(a.status)).count());
                    view! {
                        <button
                            class=move || if tab.get() == t { "active" } else { "" }
                            on:click=move |_| {
                                tab.set(t);
                                browser::storage_set("tab", t.key());
                            }
                        >
                            {t.label()}
                            <span class="count">{count}</span>
                        </button>
                    }
                })
                .collect_view()}
        </nav>
        <div class="list">
            <Show when=move || ctx.anime.with(|l| l.is_empty())>
                <div class="empty">"Your list is empty — search above to add an anime, or import from MyAnimeList."</div>
            </Show>
            <Show when=move || ctx.anime.with(|l| !l.is_empty()) && visible.with(Vec::is_empty)>
                <div class="empty">"Nothing here."</div>
            </Show>
            // Keyed by what a card displays, so it re-renders only when that changes.
            <For
                each=move || visible.get()
                key=Anime::display_hash
                children=move |a| view! { <Card anime=a/> }
            />
        </div>
    }
}

/// (css class, text) describing what can be watched.
fn availability_line(a: &Anime, now: DateTime<Utc>) -> (&'static str, String) {
    match a.availability(now) {
        Availability::Available { from, count: 1, .. } => {
            ("available", format!("▶ Episode {from} is available"))
        }
        Availability::Available { from, to, count } => (
            "available",
            format!("▶ Episodes {from}–{to} available ({count})"),
        ),
        Availability::Upcoming {
            episode,
            airing_at,
            in_seconds,
        } => (
            "upcoming",
            format!(
                "⏳ Episode {episode} airs {} · in {}",
                browser::fmt_datetime(airing_at, true),
                fmt_countdown(in_seconds)
            ),
        ),
        Availability::AllWatched => ("done", "✓ All episodes watched".into()),
        Availability::NoDate { hiatus: true, .. } => {
            ("nodate", "⏸ On hiatus — no next date announced".into())
        }
        Availability::NoDate {
            start_date: Some(d),
            ..
        } => {
            let verb = if d > now.date_naive() {
                "Starts"
            } else {
                "Started"
            };
            (
                "nodate",
                format!(
                    "📅 {verb} {} — exact episode date not announced",
                    browser::fmt_date(d)
                ),
            )
        }
        Availability::NoDate { .. } => ("nodate", "📅 No air date announced yet".into()),
    }
}

#[component]
fn Card(anime: Anime) -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let total = anime.airing.total_episodes;
    let meta = media_meta(&[
        anime.airing.format.clone(),
        anime.airing.season.clone(),
        total.map(|t| format!("{t} eps")),
        pretty_status(&anime.airing.media_status),
        anime.title_english.clone().filter(|e| *e != anime.title),
    ]);
    let total_suffix = total.map(|t| format!(" / {t}")).unwrap_or_default();
    let (watched, aired) = (anime.watched, anime.airing.latest_aired);
    let link = anime.site_url.clone().unwrap_or_else(|| "#".into());

    // Re-evaluated as time passes (countdowns).
    let for_avail = anime.clone();
    let availability = move || {
        let (class, text) = availability_line(&for_avail, ctx.now.get());
        view! { <div class=format!("avail {class}")>{text}</div> }
    };

    view! {
        <article class="card">
            {anime.cover.clone().map(|c| view! { <img class="cover" src=c alt="" loading="lazy"/> })}
            <div class="body">
                <h3><a href=link target="_blank" rel="noopener">{anime.title.clone()}</a></h3>
                <div class="sub">{meta}</div>
                {availability}
                <WatchLinks links=anime.streaming.clone()/>
                <div class="sub">
                    "Watched " <b>{watched}</b> {total_suffix.clone()} " · aired " {aired} {total_suffix}
                </div>
                <ProgressBar watched aired total/>
                <Controls anime=anime.clone()/>
                <WatchLog anime/>
            </div>
        </article>
    }
}

/// "▶ Watch on Crunchyroll" links to official streaming services.
#[component]
fn WatchLinks(links: Vec<StreamLink>) -> impl IntoView {
    (!links.is_empty()).then(|| {
        let links = links
            .into_iter()
            .map(|l| view! { <a class="watch" href=l.url target="_blank" rel="noopener">{format!("▶ Watch on {}", l.site)}</a> })
            .collect_view();
        view! { <div class="watch-links">{links}</div> }
    })
}

#[component]
fn ProgressBar(watched: u32, aired: u32, total: Option<u32>) -> impl IntoView {
    let denominator = f64::from(total.unwrap_or(aired.max(watched)).max(1));
    let percent = |n: u32| (f64::from(n) / denominator * 100.0).min(100.0);
    view! {
        <div class="progress" title="dark: watched, light: aired">
            <div class="aired" style=format!("width:{}%", percent(aired))></div>
            <div style=format!("width:{}%", percent(watched))></div>
        </div>
    }
}

/// Watched / un-watch buttons, progress box, status menu and remove button.
#[component]
fn Controls(anime: Anime) -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let Anime {
        anilist_id: id,
        watched,
        status,
        title,
        ..
    } = anime;
    let total = anime.airing.total_episodes;
    let next = watched + 1;
    let can_watch_next = total.is_none_or(|t| watched < t);

    let watch_next = move |_| ctx.update_with(api::log_watch(id, WatchReq::default()));
    let undo = move |_| {
        ctx.patch(
            id,
            UpdateReq {
                watched: Some(watched.saturating_sub(1)),
                ..Default::default()
            },
        )
    };
    let set_progress = move |ev| {
        let input = event_target::<web_sys::HtmlInputElement>(&ev);
        let typed = input.value();
        // Show the saved value until the server confirms a change (the card then
        // re-renders), so an invalid or clamped entry never lingers in the box.
        input.set_value(&watched.to_string());
        match typed.trim().parse::<u32>() {
            Ok(v) if v != watched => ctx.patch(
                id,
                UpdateReq {
                    watched: Some(v),
                    ..Default::default()
                },
            ),
            Ok(_) => {}
            Err(_) => ctx.error("Progress must be a whole number".into()),
        }
    };
    let set_status = move |ev| {
        if let Some(s) = WatchStatus::parse(&event_target_value(&ev)) {
            ctx.patch(
                id,
                UpdateReq {
                    status: Some(s),
                    ..Default::default()
                },
            );
        }
    };
    let remove = move |_| {
        if !browser::confirm(&format!("Remove \"{title}\" from your list?")) {
            return;
        }
        spawn_local(async move {
            match api::remove_anime(id).await {
                Ok(()) => ctx.anime.update(|l| l.retain(|a| a.anilist_id != id)),
                Err(e) => ctx.error(e),
            }
        });
    };

    view! {
        <div class="controls">
            {can_watch_next.then(|| view! {
                <button class="primary" on:click=watch_next>{format!("✓ Watched ep {next}")}</button>
            })}
            <button on:click=undo disabled=watched == 0 title="Un-watch last episode">"−1"</button>
            <label class="sub">
                "Progress "
                <input type="number" min="0" max=total.map(|t| t.to_string()) prop:value=watched.to_string() on:change=set_progress/>
            </label>
            <select on:change=set_status>
                {WatchStatus::ALL
                    .into_iter()
                    .map(|s| view! { <option value=s.as_str() selected=s == status>{s.label()}</option> })
                    .collect_view()}
            </select>
            <button class="danger" on:click=remove title="Remove from list">"🗑"</button>
        </div>
    }
}

/// Collapsible watch history with a form to log an episode at a chosen time.
#[component]
fn WatchLog(anime: Anime) -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let id = anime.anilist_id;
    let total = anime.airing.total_episodes;
    let suggested = if total.is_none_or(|t| anime.watched < t) {
        anime.watched + 1
    } else {
        anime.watched
    };
    let log_ep = RwSignal::new(suggested.to_string());
    let log_at = RwSignal::new(browser::datetime_local_now());

    let submit = move |ev: SubmitEvent| {
        ev.prevent_default();
        let (Ok(episode), Some(at)) = (
            log_ep.get_untracked().parse::<u32>(),
            browser::parse_datetime_local(&log_at.get_untracked()),
        ) else {
            ctx.error("Enter an episode number and a valid date".into());
            return;
        };
        let req = WatchReq {
            episode: Some(episode),
            watched_at: Some(at),
        };
        ctx.update_with_message(
            api::log_watch(id, req),
            Some(format!("Logged episode {episode}")),
        );
    };
    // Remember open/closed across re-renders of this card.
    let open = ctx.open_logs.with_untracked(|s| s.contains(&id));
    let toggle = move |_| {
        ctx.open_logs.update(|s| {
            if !s.remove(&id) {
                s.insert(id);
            }
        });
    };
    let entries = anime.watch_log.len();
    let history = if anime.watch_log.is_empty() {
        view! { <li>"Nothing logged yet"</li> }.into_any()
    } else {
        anime
            .watch_log
            .into_iter()
            .rev()
            .map(|w| view! { <li>{format!("Episode {} — {}", w.episode, browser::fmt_datetime(w.watched_at, true))}</li> })
            .collect_view()
            .into_any()
    };

    view! {
        <details class="log" open=open>
            <summary on:click=toggle>{format!("Watch history ({entries})")}</summary>
            <form on:submit=submit>
                <span class="sub">"Watched episode"</span>
                <input type="number" min="1" max=total.map(|t| t.to_string()) required bind:value=log_ep/>
                <span class="sub">"on"</span>
                <input type="datetime-local" required bind:value=log_at/>
                <button type="submit">"Save"</button>
            </form>
            <ul>{history}</ul>
        </details>
    }
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

#[component]
fn NotificationsPanel() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let permission = RwSignal::new(browser::notification_permission());
    let enable = move |_| {
        spawn_local(async move {
            browser::request_notification_permission().await;
            permission.set(browser::notification_permission());
        })
    };
    let read_all = move |_| {
        spawn_local(async move {
            match api::mark_all_read().await {
                Ok(()) => ctx.reload_notifications().await,
                Err(e) => ctx.error(e),
            }
        })
    };
    let clear = move |_| {
        if !browser::confirm("Delete all notifications?") {
            return;
        }
        spawn_local(async move {
            match api::clear_notifications().await {
                Ok(()) => ctx.reload_notifications().await,
                Err(e) => ctx.error(e),
            }
        })
    };

    view! {
        <Show when=move || ctx.panel.get() == Some(Panel::Notifications)>
            <aside class="panel">
                <div class="panel-head">
                    <h2>"Notifications"</h2>
                    <div>
                        <Show when=move || permission.get() == browser::Permission::Default>
                            <button on:click=enable>"Enable desktop alerts"</button>
                        </Show>
                        <button on:click=read_all>"Mark all read"</button>
                        <button on:click=clear>"Clear"</button>
                        <button on:click=move |_| ctx.panel.set(None)>"✕"</button>
                    </div>
                </div>
                <ul class="notif-list">
                    {move || {
                        let list = ctx.notifications.get().map(|n| n.notifications).unwrap_or_default();
                        if list.is_empty() {
                            return view! {
                                <li class="sub">"No notifications yet. You'll be told here when a new episode drops."</li>
                            }
                                .into_any();
                        }
                        list.into_iter()
                            .map(|n| view! {
                                <li class=if n.read { "" } else { "unread" }>
                                    {n.cover.clone().map(|c| view! { <img src=c alt=""/> })}
                                    <div>
                                        <b>{n.title.clone()}</b>
                                        <div>{n.message.clone()}</div>
                                        <div class="sub">{browser::fmt_datetime(n.created_at, true)}</div>
                                        {n.watch.clone().map(|w| view! { <WatchLinks links=vec![w]/> })}
                                    </div>
                                </li>
                            })
                            .collect_view()
                            .into_any()
                    }}
                </ul>
            </aside>
        </Show>
    }
}

#[component]
fn MalPanel() -> impl IntoView {
    let ctx = expect_context::<Ctx>();
    let status = RwSignal::new(None::<MalStatus>);
    let username = RwSignal::new(String::new());
    let statuses = RwSignal::new(
        WatchStatus::ALL
            .into_iter()
            .filter(|s| s.is_active())
            .collect::<HashSet<_>>(),
    );
    let importing = RwSignal::new(false);
    let result = RwSignal::new(None::<String>);

    let load = move || {
        spawn_local(async move {
            match api::mal_status().await {
                Ok(s) => status.set(Some(s)),
                Err(e) => ctx.error(e),
            }
        })
    };
    Effect::new(move |_| {
        if ctx.panel.get() == Some(Panel::Mal) {
            load();
        }
    });

    let disconnect = move |_| {
        spawn_local(async move {
            match api::mal_disconnect().await {
                Ok(()) => load(),
                Err(e) => ctx.error(e),
            }
        })
    };
    let import = move |ev: SubmitEvent| {
        ev.prevent_default();
        importing.set(true);
        result.set(Some(
            "Importing… (this can take a moment for big lists)".into(),
        ));
        let req = ImportReq {
            username: Some(username.get_untracked()).filter(|u| !u.trim().is_empty()),
            statuses: Some(statuses.get_untracked().into_iter().collect()),
        };
        spawn_local(async move {
            match api::mal_import(req).await {
                Ok(r) => {
                    let mut msg = format!("Added {}, updated {}.", r.added, r.updated);
                    if !r.unmatched.is_empty() {
                        msg += &format!(" Couldn't find on AniList: {}", r.unmatched.join(", "));
                    }
                    result.set(Some(msg));
                    ctx.reload_anime().await;
                }
                Err(e) => result.set(Some(e)),
            }
            importing.set(false);
        });
    };

    let body = move || {
        let Some(s) = status.get() else {
            return view! { <p class="sub">"Loading…"</p> }.into_any();
        };
        if !s.configured {
            return view! {
                <p>"MyAnimeList integration is off. To enable it:"</p>
                <ol>
                    <li>
                        "Create an API client at "
                        <a href="https://myanimelist.net/apiconfig" target="_blank" rel="noopener">"myanimelist.net/apiconfig"</a>"."
                    </li>
                    <li>"Set the App Redirect URL to " <code>{s.redirect_uri.clone()}</code> "."</li>
                    <li>
                        "Restart the server with " <code>"MAL_CLIENT_ID"</code> " (and "
                        <code>"MAL_CLIENT_SECRET"</code> " for \"web\" apps) set."
                    </li>
                </ol>
            }
                .into_any();
        }
        let connected = s.connected;
        let account = if connected {
            view! {
                <p>
                    "Connected as " <b>{s.username.clone().unwrap_or_else(|| "your account".into())}</b>
                    ". Progress you log here is synced to MyAnimeList."
                </p>
                <button on:click=disconnect>"Disconnect"</button>
            }
                .into_any()
        } else {
            view! {
                <p>"Connect your account to sync watched episodes back to MyAnimeList."</p>
                <a href="/auth/mal/login"><button class="primary" type="button">"Connect MyAnimeList"</button></a>
            }
                .into_any()
        };
        view! {
            {account}
            <hr/>
            <form on:submit=import>
                <b>"Import list"</b>
                <input
                    placeholder=if connected { "Username (blank = your account)" } else { "MyAnimeList username (public list)" }
                    bind:value=username
                />
                <div>
                    {WatchStatus::ALL
                        .into_iter()
                        .map(|st| view! {
                            <label>
                                <input
                                    type="checkbox"
                                    prop:checked=move || statuses.with(|s| s.contains(&st))
                                    on:change=move |ev| {
                                        let on = event_target_checked(&ev);
                                        statuses.update(|s| {
                                            if on { s.insert(st); } else { s.remove(&st); }
                                        });
                                    }
                                />
                                " " {st.label()}
                            </label>
                        })
                        .collect_view()}
                </div>
                <button class="primary" type="submit" disabled=importing>"Import"</button>
                <div class="sub">{move || result.get()}</div>
            </form>
        }
            .into_any()
    };

    view! {
        <Show when=move || ctx.panel.get() == Some(Panel::Mal)>
            <aside class="panel">
                <div class="panel-head">
                    <h2>"MyAnimeList"</h2>
                    <button on:click=move |_| ctx.panel.set(None)>"✕"</button>
                </div>
                <div class="mal-body">{body}</div>
            </aside>
        </Show>
    }
}
