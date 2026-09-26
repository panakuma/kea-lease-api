//! リース情報 API のハンドラ。
//!
//! 接続先スキーマ由来の差 (使える列、address の持ち方) は `capability` が
//! 吸収するので、ここでは v4 / v6 を同じ形で扱える。

use crate::capability::{Family, LeaseCapability, arguments};
use crate::error::{ApiError, ApiResult};
use crate::lease::{HwaddrFormat, Lease4, Lease6};
use crate::query::{LeaseQuery, Plan, ValidatedQuery};
use crate::state::AppState;
use axum::{
    Json,
    extract::{Path, State},
};
use sqlx::mysql::MySqlRow;

/// `GET /` `GET /leases`
pub async fn list_leases4(
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<LeaseQuery>,
) -> ApiResult<Json<Vec<Lease4>>> {
    let capability = state.capability(Family::V4)?;
    let plan = Plan::build(&query, capability, state.max_limit)?;
    let rows = fetch_all(&state, &plan, capability).await?;

    Ok(Json(
        rows.iter()
            .map(|row| Lease4::from_row(row, capability, plan.hwaddr_format))
            .collect(),
    ))
}

/// `GET /leases/count`
///
/// v0.1 と同じく裸の数値を返す。
pub async fn count_leases4(
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<LeaseQuery>,
) -> ApiResult<Json<i64>> {
    let capability = state.capability(Family::V4)?;
    let plan = Plan::build(&query, capability, state.max_limit)?;
    Ok(Json(count(&state, &plan, capability).await?))
}

/// `GET /leases/{address}`
///
/// state や有効期限では絞らない。名指しされた 1 件をそのまま返す。
pub async fn get_lease4(
    State(state): State<AppState>,
    Path(address): Path<String>,
    ValidatedQuery(query): ValidatedQuery<AddressQuery>,
) -> ApiResult<Json<Lease4>> {
    let capability = state.capability(Family::V4)?;
    let hwaddr_format = HwaddrFormat::from_param(query.hwaddr_format.as_deref())?;
    let row = fetch_one(&state, capability, &address).await?;
    Ok(Json(Lease4::from_row(&row, capability, hwaddr_format)))
}

/// `GET /leases6`
pub async fn list_leases6(
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<LeaseQuery>,
) -> ApiResult<Json<Vec<Lease6>>> {
    let capability = state.capability(Family::V6)?;
    let plan = Plan::build(&query, capability, state.max_limit)?;
    let rows = fetch_all(&state, &plan, capability).await?;

    Ok(Json(
        rows.iter()
            .map(|row| Lease6::from_row(row, capability, plan.hwaddr_format))
            .collect(),
    ))
}

/// `GET /leases6/count`
pub async fn count_leases6(
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<LeaseQuery>,
) -> ApiResult<Json<i64>> {
    let capability = state.capability(Family::V6)?;
    let plan = Plan::build(&query, capability, state.max_limit)?;
    Ok(Json(count(&state, &plan, capability).await?))
}

/// `GET /leases6/{address}`
pub async fn get_lease6(
    State(state): State<AppState>,
    Path(address): Path<String>,
    ValidatedQuery(query): ValidatedQuery<AddressQuery>,
) -> ApiResult<Json<Lease6>> {
    let capability = state.capability(Family::V6)?;
    let hwaddr_format = HwaddrFormat::from_param(query.hwaddr_format.as_deref())?;
    let row = fetch_one(&state, capability, &address).await?;
    Ok(Json(Lease6::from_row(&row, capability, hwaddr_format)))
}

async fn fetch_all(
    state: &AppState,
    plan: &Plan,
    capability: &LeaseCapability,
) -> ApiResult<Vec<MySqlRow>> {
    let (sql, binds) = plan.select(capability);
    Ok(sqlx::query_with(&sql, arguments(&binds)?)
        .fetch_all(&state.pool)
        .await?)
}

async fn count(state: &AppState, plan: &Plan, capability: &LeaseCapability) -> ApiResult<i64> {
    let (sql, binds) = plan.count(capability);
    Ok(sqlx::query_scalar_with(&sql, arguments(&binds)?)
        .fetch_one(&state.pool)
        .await?)
}

/// アドレスを名指しして 1 件引く。
async fn fetch_one(
    state: &AppState,
    capability: &LeaseCapability,
    address: &str,
) -> ApiResult<MySqlRow> {
    let lookup = capability.address_lookup(address)?;
    let sql = format!(
        "SELECT {} FROM {} WHERE {}",
        capability.select_list(),
        capability.table(),
        lookup.predicate
    );

    sqlx::query_with(&sql, arguments(&lookup.binds)?)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("リースが見つかりません: {}", lookup.canonical)))
}

/// 1 件取得時に受け付けるパラメータ。
#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AddressQuery {
    pub hwaddr_format: Option<String>,
}
