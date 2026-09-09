//! ISC Kea の MySQL / MariaDB バックエンドからリース情報を読み出して
//! JSON で返す HTTP API サーバ。

mod capability;
mod config;
mod error;
mod handlers;
mod lease;
mod query;
mod schema;
mod state;
mod stats;

use anyhow::Context;
use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, State},
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};
use capability::{Family, LeaseCapability};
use config::Config;
use schema::Schema;
use sqlx::mysql::MySqlPoolOptions;
use state::AppState;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower_http::timeout::TimeoutLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // RUST_LOG があれば従い、無ければ info。v0.1 は info 固定だった。
    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config_path = config::config_path_hint();
    let config = Config::load()?;
    tracing::info!("設定ファイル: {}", config_path.as_ref().display());

    let pool = MySqlPoolOptions::new()
        .max_connections(config.database.max_connections)
        .acquire_timeout(Duration::from_secs(config.database.connect_timeout_secs))
        .connect_with(config.database.connect_options())
        .await
        .with_context(|| {
            format!(
                "データベースに接続できませんでした: {}",
                config.database.display_target()
            )
        })?;
    tracing::info!("接続先データベース: {}", config.database.display_target());

    let schema = Arc::new(
        Schema::detect(&pool)
            .await
            .context("Kea のスキーマを確認できませんでした")?,
    );
    let lease4 = Arc::new(LeaseCapability::detect(Family::V4, schema.clone()));
    let lease6 = Arc::new(LeaseCapability::detect(Family::V6, schema.clone()));
    log_capabilities(&pool, &lease4, &lease6).await;

    let app_state = AppState {
        pool,
        schema,
        lease4,
        lease6,
        max_limit: config.general.max_limit,
        trust_proxy_header: config.general.trust_proxy_header,
    };

    let app = Router::new()
        .route("/", get(handlers::list_leases4))
        .route("/leases", get(handlers::list_leases4))
        .route("/leases/count", get(handlers::count_leases4))
        .route("/leases/{address}", get(handlers::get_lease4))
        .route("/leases6", get(handlers::list_leases6))
        .route("/leases6/count", get(handlers::count_leases6))
        .route("/leases6/{address}", get(handlers::get_lease6))
        .route("/stats", get(stats::stats))
        .route("/metrics", get(stats::metrics))
        .route("/healthz", get(stats::healthz))
        .fallback(not_found)
        .with_state(app_state.clone())
        // 遅い問い合わせで接続を握り続けないよう頭を押さえる。
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(config.general.request_timeout_secs),
        ))
        // ログは最外周に置いて、タイムアウトした応答も記録できるようにする。
        .layer(axum::middleware::from_fn_with_state(app_state, log_request));

    let bind_address = config.bind_address();
    let listener = tokio::net::TcpListener::bind(&bind_address)
        .await
        .with_context(|| format!("待ち受けを開始できませんでした: {bind_address}"))?;
    tracing::info!("listening on {bind_address}");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("サーバが異常終了しました")?;

    tracing::info!("停止しました");
    Ok(())
}

/// 起動時に、接続先スキーマから見えた情報を出しておく。
/// 「新しい項目が全部 null で返る」ときの切り分けが楽になる。
async fn log_capabilities(
    pool: &sqlx::Pool<sqlx::MySql>,
    lease4: &LeaseCapability,
    lease6: &LeaseCapability,
) {
    match schema::schema_version(pool).await {
        Some(version) => tracing::info!("Kea スキーマバージョン: {version}"),
        None => tracing::warn!("schema_version テーブルを読めませんでした"),
    }
    tracing::info!(
        "lease4: {} / lease6: {}",
        if lease4.present() { "あり" } else { "なし" },
        if lease6.present() { "あり" } else { "なし" }
    );

    for capability in [lease4, lease6] {
        if !capability.present() {
            continue;
        }
        // 後年の Kea で追加された列。無くても動くが、該当項目は null になる。
        let notable: &[&str] = match capability.family() {
            Family::V4 => &["state", "pool_id", "relay_id", "remote_id"],
            Family::V6 => &["state", "pool_id"],
        };
        let missing: Vec<&str> = notable
            .iter()
            .copied()
            .filter(|column| !capability.has(column))
            .collect();
        if !missing.is_empty() {
            tracing::warn!(
                "{} に無い列があります (該当項目は null で返します): {}",
                capability.table(),
                missing.join(", ")
            );
        }
    }
}

/// アクセスログ。v0.1 は受信時と応答時で 2 行に分かれていたため、
/// 同時アクセス時にどの行が対になるのか分からなかった。1 行にまとめる。
async fn log_request(
    State(app): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let start = Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let client_ip = client_ip(&app, &addr, &req);

    let response = next.run(req).await;

    tracing::info!(
        "client_ip=\"{client_ip}\" method={method} path=\"{path}\" query=\"{query}\" status={} latency={:.2?}",
        response.status().as_u16(),
        start.elapsed()
    );
    response
}

/// クライアント IP を求める。
///
/// IPv6 ソケットで待つと IPv4 の接続が `::ffff:192.0.2.1` に見えるので、
/// v0.1 と同様にほどく。リバースプロキシ配下では設定で
/// `trust_proxy_header = true` にすると X-Forwarded-For を優先する。
fn client_ip(app: &AppState, addr: &SocketAddr, req: &Request<Body>) -> String {
    if app.trust_proxy_header
        && let Some(forwarded) = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    {
        return forwarded.to_string();
    }

    let ip = addr.ip().to_string();
    ip.strip_prefix("::ffff:").unwrap_or(&ip).to_string()
}

async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": "not_found",
            "message": "そのようなエンドポイントはありません",
            "endpoints": [
                "/leases", "/leases/count", "/leases/{address}",
                "/leases6", "/leases6/count", "/leases6/{address}",
                "/stats", "/metrics", "/healthz"
            ]
        })),
    )
        .into_response()
}

/// SIGINT / SIGTERM を受けたら、処理中のリクエストを捌き切ってから終了する。
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!("SIGTERM を待ち受けられませんでした: {error}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("SIGINT を受信しました"),
        _ = terminate => tracing::info!("SIGTERM を受信しました"),
    }
}
