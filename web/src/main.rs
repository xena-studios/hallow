//! Hallow's player website: profiles and leaderboards, server-rendered from
//! the control plane's public API (`hallow_proto::public`).
//!
//! | Variable | Default |
//! |---|---|
//! | `HALLOW_PUBLIC_API` | required, such as `http://control-plane:8080` |
//! | `HALLOW_WEB_LISTEN` | `0.0.0.0:8080` |
//! | `HALLOW_WEB_NETWORK_NAME` | `Hallow` |

mod api;
mod fmt;
mod views;

use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Router, serve};
use futures_util::future::join_all;
use hallow_proto::public::valid_board;
use hallow_proto::stats::Window;
use maud::Markup;
use serde::Deserialize;

use crate::api::{Api, ApiError, forwarded_for};
use crate::views::{BOARD_ROWS, PREVIEW_ROWS, Preview, Site};

const CSS: &str = include_str!("style.css");
/// Kinds previewed on the home page.
const HOME_KINDS: usize = 12;

/// The stylesheet's URL, with a hash of its content so it can be cached
/// for a year.
static CSS_HREF: LazyLock<String> = LazyLock::new(|| {
    let hash = CSS.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("/static/style.css?v={:08x}", hash >> 32)
});

struct App {
    api: Api,
    name: String,
}

impl App {
    fn site(&self) -> Site<'_> {
        Site {
            name: &self.name,
            css: &CSS_HREF,
        }
    }

    fn page(&self, status: StatusCode, markup: Markup) -> Response {
        (status, Html(markup.into_string())).into_response()
    }

    fn ok(&self, markup: Markup) -> Response {
        self.page(StatusCode::OK, markup)
    }

    fn not_found(&self, message: &str) -> Response {
        self.page(
            StatusCode::NOT_FOUND,
            views::not_found(self.site(), message),
        )
    }

    /// The page for an API failure other than a 404.
    fn failed(&self, err: ApiError) -> Response {
        match err {
            ApiError::NotFound => self.not_found("That page doesn't exist."),
            ApiError::RateLimited(retry) => {
                let mut response = self.page(
                    StatusCode::TOO_MANY_REQUESTS,
                    views::slow_down(self.site(), retry),
                );
                if let Some(secs) = retry {
                    response
                        .headers_mut()
                        .insert(header::RETRY_AFTER, HeaderValue::from(secs));
                }
                response
            }
            ApiError::Unavailable(why) => {
                tracing::warn!(%why, "public API unavailable");
                self.page(
                    StatusCode::SERVICE_UNAVAILABLE,
                    views::unavailable(self.site()),
                )
            }
        }
    }
}

type Shared = State<Arc<App>>;

/// Who's asking, passed on so the API rate-limits the browser, not us.
struct From(String);

fn from(headers: &HeaderMap, peer: SocketAddr) -> From {
    From(forwarded_for(headers, peer.ip()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

async fn home(
    State(app): Shared,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let From(from) = from(&headers, peer);
    let (summary, kinds) = tokio::join!(app.api.network(&from), app.api.boards(&from));
    // A rate limit anywhere means the whole page; otherwise show what loaded.
    if let Err(ApiError::RateLimited(retry)) = summary {
        return app.failed(ApiError::RateLimited(retry));
    }
    let kinds = match kinds {
        Ok(kinds) => kinds,
        Err(ApiError::NotFound) => Vec::new(),
        Err(err) => return app.failed(err),
    };
    let previews = join_all(kinds.into_iter().take(HOME_KINDS).map(|kind| {
        let app = &app;
        let from = &from;
        async move {
            let board = match views::headline_stat(&kind.stats) {
                Some(stat) => app
                    .api
                    .leaderboard(&kind.kind, stat, Window::All, PREVIEW_ROWS, from)
                    .await
                    .ok(),
                None => None,
            };
            Preview { kind, board }
        }
    }))
    .await;
    app.ok(views::home(app.site(), summary.ok().as_ref(), &previews))
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
}

async fn search(
    State(app): Shared,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Response {
    let From(from) = from(&headers, peer);
    let q = query.q.trim();
    if q.is_empty() {
        return Redirect::to("/").into_response();
    }
    let q: String = q.chars().take(64).collect();
    let matches = match app.api.search(&q, &from).await {
        Ok(matches) => matches,
        Err(ApiError::NotFound) => Vec::new(),
        Err(err) => return app.failed(err),
    };
    if let Some(exact) = matches.iter().find(|m| m.username.eq_ignore_ascii_case(&q)) {
        return Redirect::to(&format!("/player/{}", fmt::segment(&exact.username))).into_response();
    }
    if matches.is_empty() {
        // A UUID, or a name search doesn't find: try it as a profile.
        if let Ok(profile) = app.api.profile(&q, &from).await {
            return Redirect::to(&format!("/player/{}", fmt::segment(&profile.username)))
                .into_response();
        }
    }
    app.ok(views::search(app.site(), &q, &matches))
}

async fn player(
    State(app): Shared,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let From(from) = from(&headers, peer);
    if name.is_empty() || name.len() > 64 {
        return app.not_found("No player by that name.");
    }
    match app.api.profile(&name, &from).await {
        Ok(profile) => app.ok(views::player(app.site(), now_ms(), &profile)),
        Err(ApiError::NotFound) => app.not_found("No player by that name has joined the network."),
        Err(err) => app.failed(err),
    }
}

async fn boards(
    State(app): Shared,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let From(from) = from(&headers, peer);
    match app.api.boards(&from).await {
        Ok(kinds) => app.ok(views::boards(app.site(), &kinds)),
        Err(ApiError::NotFound) => app.ok(views::boards(app.site(), &[])),
        Err(err) => app.failed(err),
    }
}

#[derive(Deserialize)]
struct BoardQuery {
    window: Option<String>,
}

async fn board(
    State(app): Shared,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((kind, stat)): Path<(String, String)>,
    Query(query): Query<BoardQuery>,
) -> Response {
    let From(from) = from(&headers, peer);
    if !valid_board(&kind, &stat) {
        return app.not_found("No such leaderboard.");
    }
    let window = query
        .window
        .as_deref()
        .and_then(Window::parse)
        .unwrap_or_default();
    let (board, kinds) = tokio::join!(
        app.api.leaderboard(&kind, &stat, window, BOARD_ROWS, &from),
        app.api.boards(&from)
    );
    let stats = kinds
        .ok()
        .and_then(|kinds| kinds.into_iter().find(|k| k.kind == kind))
        .map(|k| k.stats)
        .unwrap_or_default();
    match board {
        Ok(board) => app.ok(views::leaderboard(app.site(), &board, &stats)),
        Err(ApiError::NotFound) => app.not_found("No such leaderboard."),
        Err(err) => app.failed(err),
    }
}

async fn style() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        CSS,
    )
}

async fn fallback(State(app): Shared) -> Response {
    app.not_found("That page doesn't exist.")
}

fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/search", get(search))
        .route("/player/{name}", get(player))
        .route("/leaderboards", get(boards))
        .route("/leaderboards/{kind}/{stat}", get(board))
        .route("/static/style.css", get(style))
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .fallback(fallback)
        .with_state(app)
}

/// Ctrl-C, or SIGTERM from `docker stop`.
async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let Ok(base) = std::env::var("HALLOW_PUBLIC_API") else {
        eprintln!("HALLOW_PUBLIC_API is required, such as http://control-plane:8080");
        std::process::exit(2);
    };
    let listen = std::env::var("HALLOW_WEB_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let name = std::env::var("HALLOW_WEB_NETWORK_NAME")
        .ok()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Hallow".into());
    let app = Arc::new(App {
        api: Api::new(&base),
        name,
    });
    let listener = match tokio::net::TcpListener::bind(&listen).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("can't listen on {listen}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(%listen, api = %base, "hallow-web up");
    let service = router(app).into_make_service_with_connect_info::<SocketAddr>();
    if let Err(e) = serve(listener, service)
        .with_graceful_shutdown(shutdown())
        .await
    {
        eprintln!("server: {e}");
        std::process::exit(1);
    }
}
