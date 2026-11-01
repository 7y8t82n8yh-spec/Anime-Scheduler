# Anime Scheduler

A small self-hosted web app for keeping track of the anime you want to watch, written
entirely in Rust: an [Axum](https://github.com/tokio-rs/axum) server and a
[Leptos](https://leptos.dev) frontend compiled to **WebAssembly**. No JavaScript is written by hand.

- **Up next list**: every show in your list with what you can watch *right now*
  ("Episodes 4–6 available"). When you're caught up, it shows when the next episode airs,
  with the date, time and a countdown. Shows with no announced date say so.
- **New-episode notifications**: a background poller checks AniList (every 30 min, and
  right after each known air time). New episodes go to the 🔔 panel, and to desktop
  notifications if you allow them in the browser.
- **Progress tracking**: one click on "✓ Watched ep N", or set progress directly, or log a
  specific episode with the date/time you watched it. Each show keeps a watch history.
- **Where to watch**: each show links to the official streaming services that carry it
  (Crunchyroll, Netflix, …, as listed on AniList).
- **Phone notifications** (optional) via [ntfy](https://ntfy.sh): new episodes are pushed
  to your phone even when no browser is open; tapping one opens the stream.
- **MyAnimeList integration** (optional): import your MAL list, and log in with MAL so
  every progress or status change is synced back to your MAL account.

Airing data comes from [AniList](https://anilist.co)'s public GraphQL API (no key needed),
because MAL's API has no per-episode air schedule. Shows are matched between the two by MAL id.

## Project layout

| Crate | What it is |
|---|---|
| `shared/` | Data model, API types and the "what can I watch / when's the next episode" logic. Compiled into both the server and the WASM frontend. |
| `server/` | Axum server: JSON API, AniList/MyAnimeList clients, background poller, serves the built frontend. |
| `frontend/` | Leptos UI, compiled to `wasm32-unknown-unknown` with [Trunk](https://trunkrs.dev). |

## Running

One-time setup (Rust 1.82 or newer):

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
```

Trunk downloads `wasm-bindgen` and `wasm-opt` (which shrinks the `.wasm` to roughly a third)
on first build. Offline, install them yourself with
`cargo install wasm-bindgen-cli --version 0.2.130 --locked` and `cargo install wasm-opt --locked`.

Build the frontend, then run the server (from the repository root):

```sh
(cd frontend && trunk build --release)   # outputs frontend/dist
cargo run --release                      # serves the app at http://127.0.0.1:3000
```

For frontend development with live reload, run the server and `trunk serve` in `frontend/`
side by side, then open http://127.0.0.1:8080 (API calls are proxied to the server).

Data is stored in `anime_data.json` in the working directory. The file contains your
MyAnimeList login tokens, so it is created readable only by your user.

The API rejects state-changing requests that don't carry an `X-Requested-With: anime-scheduler`
header (the frontend sends it), which stops other websites from driving it via your browser.

### Configuration (environment variables)

| Variable | Default | Purpose |
|---|---|---|
| `BIND` | `127.0.0.1:3000` | Address to listen on |
| `PUBLIC_URL` | `http://$BIND` | URL you open in the browser (used for the MAL OAuth redirect) |
| `DATA_FILE` | `anime_data.json` | Where your list is saved |
| `STATIC_DIR` | `frontend/dist` (falls back to the checkout's) | Built WebAssembly frontend to serve |
| `POLL_MINUTES` | `30` | Max time between airing checks |
| `NTFY_TOPIC` | – | Turns on phone notifications via ntfy (pick a long random name) |
| `NTFY_URL` | `https://ntfy.sh` | ntfy server (set this if you self-host) |
| `NTFY_TOKEN` | – | Access token, for a reserved/protected topic |
| `MAL_CLIENT_ID` | – | Turns on MyAnimeList import/sync |
| `MAL_CLIENT_SECRET` | – | Only for MAL clients of type "web" |
| `RUST_LOG` | `info` | Log level |

### Phone notifications with ntfy

1. Install the ntfy app ([Android](https://play.google.com/store/apps/details?id=io.heckel.ntfy),
   [iOS](https://apps.apple.com/app/ntfy/id1625396347)) and subscribe to a topic with a long,
   random name, e.g. `anime-k3v9q2x7wd`. On the public ntfy.sh server anyone who knows the
   name can read it, so don't pick something guessable (or reserve the topic with an account).
2. Start the server with `NTFY_TOPIC=anime-k3v9q2x7wd`.

Each new episode then arrives on your phone with the show's cover, a "Watch on …" button and,
if `PUBLIC_URL` is reachable from your phone, an "Open list" button. The server sends these,
so it works with every browser tab closed as long as the server is running.

### Connecting MyAnimeList

1. Go to <https://myanimelist.net/apiconfig> and create a client (App Type "web" or "other").
2. Set **App Redirect URL** to `http://127.0.0.1:3000/auth/mal/callback` (or `$PUBLIC_URL/auth/mal/callback`).
3. Start the app with `MAL_CLIENT_ID=... cargo run --release` (and `MAL_CLIENT_SECRET` for "web").
4. Click **MAL** in the top bar → **Connect MyAnimeList**, then **Import**.

You can also import any *public* list by username without connecting, but progress then won't sync back.

## API

| Method | Path | |
|---|---|---|
| GET | `/api/anime` | Your list, including `availability` |
| POST | `/api/anime` | `{anilist_id, status}` to add |
| PATCH | `/api/anime/{id}` | `{watched?, status?}` |
| POST | `/api/anime/{id}/watch` | `{episode?, watched_at?}`: log an episode (default: next one, now) |
| DELETE | `/api/anime/{id}` | Remove |
| GET | `/api/search?q=` | Search AniList |
| POST | `/api/refresh` | Check for new episodes now |
| GET/DELETE | `/api/notifications` | List / clear notifications |
| POST | `/api/notifications/read` | `{ids?}`: mark read (all if omitted) |
| GET | `/api/mal` | MAL connection status |
| POST | `/api/mal/import` | `{username?, statuses?}` |

## Development

The workspace forbids `unsafe` code (`[workspace.lints.rust] unsafe_code = "forbid"`).

```sh
cargo test                                                   # shared + server tests (incl. end-to-end against mock AniList/MAL)
cargo clippy -p anime-frontend --target wasm32-unknown-unknown   # check the frontend
```
