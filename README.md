# Anime Scheduler

A small self-hosted Rust web app for keeping track of the anime you want to watch.

- **Up next list**: every show in your list with what you can watch *right now*
  ("Episodes 4–6 available"). When you're caught up, it shows when the next episode airs,
  with the date, time and a countdown. Shows with no announced date say so.
- **New-episode notifications**: a background poller checks AniList (every 30 min, and
  right after each known air time). New episodes go to the 🔔 panel, and to desktop
  notifications if you allow them in the browser.
- **Progress tracking**: one click on "✓ Watched ep N", or set progress directly, or log a
  specific episode with the date/time you watched it. Each show keeps a watch history.
- **MyAnimeList integration** (optional): import your MAL list, and log in with MAL so
  every progress or status change is synced back to your MAL account.

Airing data comes from [AniList](https://anilist.co)'s public GraphQL API (no key needed),
because MAL's API has no per-episode air schedule. Shows are matched between the two by MAL id.

## Running

```sh
cargo run --release
# open http://127.0.0.1:3000
```

Data is stored in `anime_data.json` in the working directory.

### Configuration (environment variables)

| Variable | Default | Purpose |
|---|---|---|
| `BIND` | `127.0.0.1:3000` | Address to listen on |
| `PUBLIC_URL` | `http://$BIND` | URL you open in the browser (used for the MAL OAuth redirect) |
| `DATA_FILE` | `anime_data.json` | Where your list is saved |
| `POLL_MINUTES` | `30` | Max time between airing checks |
| `MAL_CLIENT_ID` | – | Turns on MyAnimeList import/sync |
| `MAL_CLIENT_SECRET` | – | Only for MAL clients of type "web" |
| `RUST_LOG` | `info` | Log level |

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

```sh
cargo test    # unit tests + end-to-end test against mock AniList/MAL servers
```
