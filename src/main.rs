use anyhow::Context;
use axum::{
    body::Body,
    extract::{State, Query, ConnectInfo},
    middleware::Next,
    http::Request,
    response::IntoResponse,
    routing::get,
    Json, 
    Router,
};
use chrono::{DateTime, Utc, Local};
use serde::{Deserialize, Serialize};
use sqlx::{mysql::MySqlPoolOptions, FromRow, MySql, Pool};
use std::net::SocketAddr;
use std::time::Instant;


#[derive(Clone)]
struct AppState {
    pool: Pool<MySql>,
}


#[derive(Deserialize, Clone)]
struct Config {
    general: GeneralConfig,
    database: DatabaseConfig,
}

#[derive(Deserialize, Clone)]
struct GeneralConfig {
    bind_addr: String,
    bind_port: u16,
}

#[derive(Deserialize, Clone)]
struct DatabaseConfig {
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
}

#[derive(Deserialize)]
struct CountParams {
    subnet_id: Option<u32>,
}

#[derive(Serialize, FromRow)]
struct Lease4 {
    address: Option<String>,
    hwaddr: Option<String>,
    client_id: Option<String>,
    valid_lifetime: Option<u32>,
    expire: Option<DateTime<Utc>>,
    subnet_id: Option<u32>,
    hostname: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {

    tracing_subscriber::fmt()
        .with_target(false)
        .with_line_number(false)
        .with_file(false)
        .with_env_filter("info")  // 固定INFO
        .init();

    let config_str = std::fs::read_to_string("config.toml")
        .context("Failed to read config.toml")?;
    let config: Config = toml::from_str(&config_str)
        .context("Failed to parse config.toml")?;

    let database_url = format!(
        "mysql://{user}:{password}@{host}:{port}/{database}",
        user = config.database.user,
        password = config.database.password,
        host = config.database.host,
        port = config.database.port,
        database = config.database.database
    );

    let pool = MySqlPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .context("Failed to connect to database")?;

    let state = AppState { pool };

    let app = Router::new()
        .route("/", get(list_leases))
        .route("/leases", get(list_leases))
        .route("/leases/count", get(count_leases))
        .with_state(state)
        .layer(axum::middleware::from_fn(log_real_ip));

    let bind_setting = config.general.bind_addr + ":" + &config.general.bind_port.to_string();
    println!("try binding on {}", bind_setting);

    let listener = tokio::net::TcpListener::bind(bind_setting.clone())
        .await
        .context("Failed to bind")?;
    
    println!("listening on {}", bind_setting);
    println!("Using DB: {}", database_url.replace(&config.database.password, "*****"));

    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .context("Server failed")?;

    Ok(())
}

async fn list_leases(
        State(state): State<AppState>,
    ) -> Json<Vec<Lease4>> {
    let rows = sqlx::query_as!(
        Lease4,
        r#"
        SELECT 
            INET_NTOA(address) as address,
            HEX(hwaddr) as hwaddr,
            CASE WHEN client_id IS NULL THEN NULL ELSE HEX(client_id) END as client_id,
            valid_lifetime,
            expire,
            subnet_id,
            hostname
        FROM lease4 
        ORDER BY address
        "#
    )
    .fetch_all(&state.pool)
    .await
    .expect("query failed");

    Json(rows)
}

async fn count_leases(
    State(state): State<AppState>,
    Query(params): Query<CountParams>,
) -> Json<i64> {
    let count = if let Some(subnet_id) = params.subnet_id {
        // subnet_id 指定あり
        let row = sqlx::query!(
            r#"SELECT COUNT(*) as "count!" FROM lease4 WHERE subnet_id = ?"#,
            subnet_id as u32
        )
        .fetch_one(&state.pool)
        .await
        .expect("count query failed");
        row.count
    } else {
        // subnet_id 指定なし
        let row = sqlx::query!(
            r#"SELECT COUNT(*) as "count!" FROM lease4"#,
        )
        .fetch_one(&state.pool)
        .await
        .expect("count query failed");
        row.count
    };

    Json(count)
}

async fn log_real_ip(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
    next: Next,
) -> impl IntoResponse {
    let start = Instant::now();
    let query = req.uri().query().unwrap_or("");

    let client_ip: String;

    if addr.ip().to_string().starts_with("::ffff:"){
        client_ip = addr.ip().to_string().strip_prefix("::ffff:").unwrap().to_string();
    }else{
        client_ip = addr.ip().to_string();
    }
    
    tracing::info!(
        "[REQ] time=\"{}\" client_ip=\"{}\" path=\"{}\" query=\"{}\"",
        Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z"),
        client_ip,
        req.uri().path(),
        query
    );

    let res = next.run(req).await;
    let latency = start.elapsed();
    tracing::info!("[RES] status={} latency={:.2?}", res.status(), latency);
    
    res
}
