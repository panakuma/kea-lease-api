//! 集計系のエンドポイント (`/healthz`, `/stats`, `/metrics`)。
//!
//! いずれも `SELECT subnet_id, state, COUNT(*) ... GROUP BY` 1 本で済むので、
//! 全リースを引いてから数えるより桁違いに軽い。

use crate::error::ApiResult;
use crate::state::AppState;
use axum::{
    Json,
    extract::State,
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sqlx::{MySql, Pool, Row};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// GROUP BY の 1 行。
#[derive(Debug, Clone)]
struct StatRow {
    subnet_id: Option<u32>,
    state: u32,
    total: i64,
    /// state を問わず、まだ expire を過ぎていない件数。
    unexpired: i64,
}

/// subnet_id / state 別の件数を数える。
async fn collect(
    pool: &Pool<MySql>,
    table: &str,
    has_state: bool,
) -> Result<Vec<StatRow>, sqlx::Error> {
    // state 列が無い古いスキーマでは 0 (default) 相当とみなす。
    // GROUP BY に整数リテラルを書くと選択リストの序数と解釈されてしまうので、
    // 集約キー自体を出し分ける。state が 1 種類しかない以上、
    // subnet_id だけでまとめても結果は同じ。
    let state_expr = if has_state { "state" } else { "0" };
    let group_by = if has_state {
        "subnet_id, state"
    } else {
        "subnet_id"
    };
    let sql = format!(
        "SELECT subnet_id AS subnet_id,
                {state_expr} AS state,
                COUNT(*) AS total,
                COUNT(CASE WHEN expire > NOW() THEN 1 END) AS unexpired
         FROM {table}
         GROUP BY {group_by}"
    );

    let rows = sqlx::query(&sql).fetch_all(pool).await?;
    rows.into_iter()
        .map(|row| {
            Ok(StatRow {
                subnet_id: row.try_get("subnet_id")?,
                state: row.try_get("state").unwrap_or(0),
                total: row.try_get("total")?,
                unexpired: row.try_get("unexpired")?,
            })
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct FamilyStats {
    /// 全行数 (失効済みや declined も含む)。
    total: i64,
    /// state=default かつ expire 前の件数。監視で見たいのは普通これ。
    active: i64,
    by_state: BTreeMap<String, i64>,
    by_subnet: Vec<SubnetStats>,
}

#[derive(Debug, Serialize)]
pub struct SubnetStats {
    subnet_id: Option<u32>,
    total: i64,
    active: i64,
    by_state: BTreeMap<String, i64>,
}

#[derive(Debug, Serialize)]
pub struct StatsResponse {
    schema_version: Option<String>,
    lease4: Option<FamilyStats>,
    lease6: Option<FamilyStats>,
}

/// state 値を表示名に落とす。未知の値でも捨てずに `state-7` として残す。
fn state_label(state: u32, app: &AppState) -> String {
    app.schema
        .state_name(state)
        .map(str::to_string)
        .unwrap_or_else(|| format!("state-{state}"))
}

fn summarize(rows: &[StatRow], app: &AppState) -> FamilyStats {
    let mut by_state: BTreeMap<String, i64> = BTreeMap::new();
    let mut per_subnet: BTreeMap<Option<u32>, (i64, i64, BTreeMap<String, i64>)> = BTreeMap::new();
    let mut total = 0;
    let mut active = 0;

    for row in rows {
        let label = state_label(row.state, app);
        // 「有効」= default (0) かつ expire 前。
        let row_active = if row.state == 0 { row.unexpired } else { 0 };

        total += row.total;
        active += row_active;
        *by_state.entry(label.clone()).or_default() += row.total;

        let entry = per_subnet.entry(row.subnet_id).or_default();
        entry.0 += row.total;
        entry.1 += row_active;
        *entry.2.entry(label).or_default() += row.total;
    }

    FamilyStats {
        total,
        active,
        by_state,
        by_subnet: per_subnet
            .into_iter()
            .map(|(subnet_id, (total, active, by_state))| SubnetStats {
                subnet_id,
                total,
                active,
                by_state,
            })
            .collect(),
    }
}

/// `GET /stats`
pub async fn stats(State(app): State<AppState>) -> ApiResult<Json<StatsResponse>> {
    let lease4 = if app.schema.has_lease4() {
        let rows = collect(&app.pool, "lease4", app.schema.lease4_has("state")).await?;
        Some(summarize(&rows, &app))
    } else {
        None
    };
    let lease6 = if app.schema.has_lease6() {
        let rows = collect(&app.pool, "lease6", app.schema.lease6_has("state")).await?;
        Some(summarize(&rows, &app))
    } else {
        None
    };

    Ok(Json(StatsResponse {
        schema_version: crate::schema::schema_version(&app.pool).await,
        lease4,
        lease6,
    }))
}

/// `GET /metrics` — Prometheus のテキスト形式。
pub async fn metrics(State(app): State<AppState>) -> ApiResult<Response> {
    let mut body = String::new();

    for (family, table, has_state, present) in [
        (
            "lease4",
            "lease4",
            app.schema.lease4_has("state"),
            app.schema.has_lease4(),
        ),
        (
            "lease6",
            "lease6",
            app.schema.lease6_has("state"),
            app.schema.has_lease6(),
        ),
    ] {
        if !present {
            continue;
        }
        let rows = collect(&app.pool, table, has_state).await?;

        let _ = writeln!(
            body,
            "# HELP kea_{family}_leases Number of rows in the Kea {table} table."
        );
        let _ = writeln!(body, "# TYPE kea_{family}_leases gauge");
        for row in &rows {
            let _ = writeln!(
                body,
                "kea_{family}_leases{{subnet_id=\"{}\",state=\"{}\"}} {}",
                row.subnet_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                escape_label(&state_label(row.state, &app)),
                row.total
            );
        }

        let _ = writeln!(
            body,
            "# HELP kea_{family}_active_leases Leases in the default state that have not expired yet."
        );
        let _ = writeln!(body, "# TYPE kea_{family}_active_leases gauge");
        let mut active_by_subnet: BTreeMap<Option<u32>, i64> = BTreeMap::new();
        for row in &rows {
            let entry = active_by_subnet.entry(row.subnet_id).or_default();
            if row.state == 0 {
                *entry += row.unexpired;
            }
        }
        for (subnet_id, active) in active_by_subnet {
            let _ = writeln!(
                body,
                "kea_{family}_active_leases{{subnet_id=\"{}\"}} {}",
                subnet_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                active
            );
        }
    }

    let mut response = body.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    Ok(response)
}

/// Prometheus のラベル値のエスケープ。
fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: &'static str,
    database: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_version: Option<String>,
}

/// `GET /healthz` — DB へ実際に 1 発投げて疎通を確認する。
pub async fn healthz(State(app): State<AppState>) -> Response {
    match sqlx::query("SELECT 1").fetch_one(&app.pool).await {
        Ok(_) => (
            StatusCode::OK,
            Json(HealthResponse {
                status: "ok",
                database: "ok",
                schema_version: crate::schema::schema_version(&app.pool).await,
            }),
        )
            .into_response(),
        Err(error) => {
            tracing::error!("health check failed: {error}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthResponse {
                    status: "unavailable",
                    database: "error",
                    schema_version: None,
                }),
            )
                .into_response()
        }
    }
}
