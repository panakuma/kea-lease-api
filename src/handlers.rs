//! リース情報 API のハンドラ。

use crate::error::{ApiError, ApiResult};
use crate::lease::{HwaddrFormat, Lease4, Lease6};
use crate::query::{Family, LeaseQuery, Plan, arguments};
use crate::schema::Schema;
use crate::state::AppState;
use axum::{
    Json,
    extract::{Path, Query, State},
};
use std::net::{Ipv4Addr, Ipv6Addr};

/// lease4 の SELECT リスト。列があるものだけを並べる。
pub fn lease4_select_list(schema: &Schema) -> String {
    // HEX() は引数が NULL なら NULL を返すので、NULL 判定は不要。
    let mut columns = vec![
        "INET_NTOA(lease4.address) AS address".to_string(),
        "HEX(lease4.hwaddr) AS hwaddr".to_string(),
        "HEX(lease4.client_id) AS client_id".to_string(),
        "lease4.valid_lifetime".to_string(),
        "lease4.expire".to_string(),
        "lease4.subnet_id".to_string(),
        "lease4.hostname".to_string(),
    ];
    for column in ["state", "pool_id", "fqdn_fwd", "fqdn_rev", "user_context"] {
        if schema.lease4_has(column) {
            columns.push(format!("lease4.{column}"));
        }
    }
    for column in ["relay_id", "remote_id"] {
        if schema.lease4_has(column) {
            columns.push(format!("HEX(lease4.{column}) AS {column}"));
        }
    }
    columns.join(", ")
}

/// lease6 の SELECT リスト。address は VARCHAR なので変換しない。
pub fn lease6_select_list(schema: &Schema) -> String {
    let mut columns = vec![
        "lease6.address".to_string(),
        "HEX(lease6.duid) AS duid".to_string(),
        "lease6.valid_lifetime".to_string(),
        "lease6.expire".to_string(),
        "lease6.subnet_id".to_string(),
        "lease6.pref_lifetime".to_string(),
        "lease6.lease_type".to_string(),
        "lease6.iaid".to_string(),
        "lease6.prefix_len".to_string(),
        "lease6.hostname".to_string(),
    ];
    for column in [
        "state",
        "pool_id",
        "fqdn_fwd",
        "fqdn_rev",
        "user_context",
        "hwtype",
        "hwaddr_source",
    ] {
        if schema.lease6_has(column) {
            columns.push(format!("lease6.{column}"));
        }
    }
    if schema.lease6_has("hwaddr") {
        columns.push("HEX(lease6.hwaddr) AS hwaddr".to_string());
    }
    columns.join(", ")
}

/// `GET /` `GET /leases`
pub async fn list_leases4(
    State(state): State<AppState>,
    Query(query): Query<LeaseQuery>,
) -> ApiResult<Json<Vec<Lease4>>> {
    state.require_lease4()?;
    let plan = Plan::build(&query, Family::V4, &state.schema, state.max_limit)?;
    let (sql, binds) = plan.select(&lease4_select_list(&state.schema), "lease4");

    let rows = sqlx::query_with(&sql, arguments(&binds)?)
        .fetch_all(&state.pool)
        .await?;

    Ok(Json(
        rows.iter()
            .map(|row| Lease4::from_row(row, &state.schema, plan.hwaddr_format))
            .collect(),
    ))
}

/// `GET /leases/count`
///
/// v0.1 と同じく裸の数値を返す。
pub async fn count_leases4(
    State(state): State<AppState>,
    Query(query): Query<LeaseQuery>,
) -> ApiResult<Json<i64>> {
    state.require_lease4()?;
    let plan = Plan::build(&query, Family::V4, &state.schema, state.max_limit)?;
    let (sql, binds) = plan.count("lease4");

    let count: i64 = sqlx::query_scalar_with(&sql, arguments(&binds)?)
        .fetch_one(&state.pool)
        .await?;

    Ok(Json(count))
}

/// `GET /leases/{address}`
///
/// state や有効期限では絞らない。名指しされた 1 件をそのまま返す。
pub async fn get_lease4(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<AddressQuery>,
) -> ApiResult<Json<Lease4>> {
    state.require_lease4()?;
    let parsed: Ipv4Addr = address.parse().map_err(|_| {
        ApiError::bad_request(format!("IPv4 アドレスとして解釈できません: {address}"))
    })?;
    let hwaddr_format = query.hwaddr_format()?;

    let sql = format!(
        "SELECT {} FROM lease4 WHERE lease4.address = ?",
        lease4_select_list(&state.schema)
    );
    let mut args = sqlx::mysql::MySqlArguments::default();
    sqlx::Arguments::add(&mut args, u32::from(parsed))
        .map_err(|error| ApiError::Database(sqlx::Error::Encode(error)))?;

    let row = sqlx::query_with(&sql, args)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("リースが見つかりません: {parsed}")))?;

    Ok(Json(Lease4::from_row(&row, &state.schema, hwaddr_format)))
}

/// `GET /leases6`
pub async fn list_leases6(
    State(state): State<AppState>,
    Query(query): Query<LeaseQuery>,
) -> ApiResult<Json<Vec<Lease6>>> {
    state.require_lease6()?;
    let plan = Plan::build(&query, Family::V6, &state.schema, state.max_limit)?;
    let (sql, binds) = plan.select(&lease6_select_list(&state.schema), "lease6");

    let rows = sqlx::query_with(&sql, arguments(&binds)?)
        .fetch_all(&state.pool)
        .await?;

    Ok(Json(
        rows.iter()
            .map(|row| Lease6::from_row(row, &state.schema, plan.hwaddr_format))
            .collect(),
    ))
}

/// `GET /leases6/count`
pub async fn count_leases6(
    State(state): State<AppState>,
    Query(query): Query<LeaseQuery>,
) -> ApiResult<Json<i64>> {
    state.require_lease6()?;
    let plan = Plan::build(&query, Family::V6, &state.schema, state.max_limit)?;
    let (sql, binds) = plan.count("lease6");

    let count: i64 = sqlx::query_scalar_with(&sql, arguments(&binds)?)
        .fetch_one(&state.pool)
        .await?;

    Ok(Json(count))
}

/// `GET /leases6/{address}`
pub async fn get_lease6(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<AddressQuery>,
) -> ApiResult<Json<Lease6>> {
    state.require_lease6()?;
    let parsed: Ipv6Addr = address.parse().map_err(|_| {
        ApiError::bad_request(format!("IPv6 アドレスとして解釈できません: {address}"))
    })?;
    let hwaddr_format = query.hwaddr_format()?;

    let mut args = sqlx::mysql::MySqlArguments::default();
    let sql = if state.schema.lease6_address_is_binary() {
        // スキーマ 19.0 以降は BINARY(16)。16 バイトを直接束縛すれば
        // 主キーがそのまま効き、表記ゆれ (2001:0db8:: と 2001:db8::) も吸収できる。
        sqlx::Arguments::add(&mut args, parsed.octets().to_vec())
            .map_err(|error| ApiError::Database(sqlx::Error::Encode(error)))?;
        format!(
            "SELECT {} FROM lease6 WHERE lease6.address = ?",
            lease6_select_list(&state.schema)
        )
    } else {
        // 18 以前はテキスト。正規化した表記と入力そのままの両方で突き合わせる。
        for value in [parsed.to_string(), address.clone()] {
            sqlx::Arguments::add(&mut args, value)
                .map_err(|error| ApiError::Database(sqlx::Error::Encode(error)))?;
        }
        format!(
            "SELECT {} FROM lease6 WHERE lease6.address IN (?, ?)",
            lease6_select_list(&state.schema)
        )
    };

    let row = sqlx::query_with(&sql, args)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("リースが見つかりません: {parsed}")))?;

    Ok(Json(Lease6::from_row(&row, &state.schema, hwaddr_format)))
}

/// 1 件取得時に受け付けるパラメータ。
#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AddressQuery {
    pub hwaddr_format: Option<String>,
}

impl AddressQuery {
    fn hwaddr_format(&self) -> ApiResult<HwaddrFormat> {
        match self.hwaddr_format.as_deref() {
            None => Ok(HwaddrFormat::default()),
            Some(value) => HwaddrFormat::parse(value).ok_or_else(|| {
                ApiError::bad_request(format!(
                    "hwaddr_format に指定できるのは hex か colon です (指定値: {value})"
                ))
            }),
        }
    }
}
