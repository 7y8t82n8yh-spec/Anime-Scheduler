//! Thin wrappers over browser APIs: local time formatting, localStorage, desktop notifications.

use anime_shared::Notification;
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, Timelike, Utc};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};

fn window() -> web_sys::Window {
    web_sys::window().expect("no window")
}

// ---------- time ----------

fn set(obj: &js_sys::Object, k: &str, v: &str) {
    let _ = js_sys::Reflect::set(obj, &k.into(), &v.into());
}

/// Format an instant in the viewer's locale and time zone, e.g. "Sat, Nov 7, 06:30 PM".
pub fn fmt_datetime(dt: DateTime<Utc>, with_time: bool) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(dt.timestamp_millis() as f64));
    let opts = js_sys::Object::new();
    set(&opts, "weekday", "short");
    set(&opts, "month", "short");
    set(&opts, "day", "numeric");
    if dt.with_timezone(&Local).year() != Local::now().year() {
        set(&opts, "year", "numeric");
    }
    if with_time {
        set(&opts, "hour", "2-digit");
        set(&opts, "minute", "2-digit");
    }
    date.to_locale_string("default", &opts).into()
}

/// Format a calendar date (no time zone shifting).
pub fn fmt_date(d: NaiveDate) -> String {
    let noon = d.and_hms_opt(12, 0, 0).expect("valid time");
    match local_to_utc(noon) {
        Some(instant) => fmt_datetime(instant, false),
        None => d.to_string(),
    }
}

/// Current local time in `<input type="datetime-local">` format.
pub fn datetime_local_now() -> String {
    Local::now().format("%Y-%m-%dT%H:%M").to_string()
}

/// Parse an `<input type="datetime-local">` value as local time.
pub fn parse_datetime_local(v: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M").ok()?;
    local_to_utc(naive)
}

/// Interpret a wall-clock time in the browser's time zone.
///
/// Uses JavaScript's `Date` (chrono's `Local` can't see DST transitions on wasm32).
/// Per the ECMAScript spec, a time skipped by a DST jump resolves to the later
/// offset and a repeated time to the earlier one.
fn local_to_utc(t: NaiveDateTime) -> Option<DateTime<Utc>> {
    let date = js_sys::Date::new_with_year_month_day_hr_min(
        u32::try_from(t.year()).ok()?,
        i32::try_from(t.month0()).ok()?,
        i32::try_from(t.day()).ok()?,
        i32::try_from(t.hour()).ok()?,
        i32::try_from(t.minute()).ok()?,
    );
    let millis = date.get_time();
    if millis.is_nan() {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)] // whole milliseconds within i64 range
    DateTime::from_timestamp_millis(millis as i64)
}

// ---------- storage / page ----------

pub fn storage_get(key: &str) -> Option<String> {
    window().local_storage().ok()??.get_item(key).ok()?
}

pub fn storage_set(key: &str, value: &str) {
    if let Ok(Some(s)) = window().local_storage() {
        let _ = s.set_item(key, value);
    }
}

pub fn confirm(msg: &str) -> bool {
    window().confirm_with_message(msg).unwrap_or(false)
}

pub fn set_title(title: &str) {
    if let Some(doc) = window().document() {
        doc.set_title(title);
    }
}

/// Returns true (and cleans the URL) when we just came back from the MAL login.
pub fn take_mal_connected_flag() -> bool {
    let w = window();
    let found = w
        .location()
        .search()
        .map(|s| s.contains("mal=connected"))
        .unwrap_or(false);
    if found {
        if let Ok(h) = w.history() {
            let _ = h.replace_state_with_url(&JsValue::NULL, "", Some("/"));
        }
    }
    found
}

// ---------- desktop notifications ----------

fn notifications_supported() -> bool {
    js_sys::Reflect::has(&window(), &"Notification".into()).unwrap_or(false)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Unsupported,
    Default,
    Granted,
    Denied,
}

pub fn notification_permission() -> Permission {
    if !notifications_supported() {
        return Permission::Unsupported;
    }
    match web_sys::Notification::permission() {
        web_sys::NotificationPermission::Granted => Permission::Granted,
        web_sys::NotificationPermission::Denied => Permission::Denied,
        _ => Permission::Default,
    }
}

pub async fn request_notification_permission() {
    if !notifications_supported() {
        return;
    }
    if let Ok(promise) = web_sys::Notification::request_permission() {
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
}

/// Show a desktop notification; clicking it opens the stream (or focuses the app).
pub fn show_notification(n: &Notification) {
    if notification_permission() != Permission::Granted {
        return;
    }
    let opts = web_sys::NotificationOptions::new();
    opts.set_body(&n.message);
    opts.set_tag(&format!("anime-{}", n.id));
    if let Some(icon) = &n.cover {
        opts.set_icon(icon);
    }
    let Ok(note) = web_sys::Notification::new_with_options(&n.title, &opts) else {
        return;
    };
    let watch_url = n.watch.as_ref().map(|w| w.url.clone());
    let on_click = Closure::once_into_js(move || {
        let w = window();
        let _ = match watch_url {
            Some(url) => w.open_with_url_and_target(&url, "_blank").map(drop),
            None => w.focus(),
        };
    });
    if let Ok(on_click) = on_click.dyn_into::<js_sys::Function>() {
        note.set_onclick(Some(&on_click));
    }
}
