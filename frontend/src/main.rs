//! Anime Scheduler browser UI, written in Rust with Leptos and compiled to WebAssembly.

mod api;
mod app;
mod browser;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}
