//! クエリパラメータの解釈と SQL の組み立て。
//!
//! v0.1 は `sqlx::query_as!` のコンパイル時検証マクロを使っていたため、
//! ビルドに生きた DATABASE_URL か `.sqlx` オフラインキャッシュが必要で、
//! クローンしただけでは `cargo build` が通らなかった。加えて、条件を
//! 動的に組み立てられないので絞り込みも足せない。
//! ここではランタイムクエリに切り替え、値はすべてプレースホルダで
//! バインドする (SQL に文字列を埋め込まない)。

use crate::error::{ApiError, ApiResult};
use crate::lease::HwaddrFormat;
use crate::schema::Schema;
use serde::Deserialize;
use sqlx::Arguments;
use sqlx::mysql::MySqlArguments;

/// 対象テーブル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    V4,
    V6,
}

impl Family {
    pub fn table(self) -> &'static str {
        match self {
            Family::V4 => "lease4",
            Family::V6 => "lease6",
        }
    }

    fn has_column(self, schema: &Schema, column: &str) -> bool {
        match self {
            Family::V4 => schema.lease4_has(column),
            Family::V6 => schema.lease6_has(column),
        }
    }
}

/// `/leases` `/leases6` `/leases/count` が受け取るクエリパラメータ。
///
/// 綴り間違いを黙って無視すると「絞り込んだつもりが全件」という事故に
/// なるので、知らないパラメータはエラーにする。
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LeaseQuery {
    /// state 名 (`default`, `declined`, `expired-reclaimed`, `released`,
    /// `registered`) か数値、または `all`。既定は `default`。
    pub state: Option<String>,
    /// true にすると expire を過ぎたリースも含める。既定は false。
    pub include_expired: Option<bool>,
    pub subnet_id: Option<u32>,
    pub pool_id: Option<u32>,
    /// MAC アドレスの前方一致。`:` `-` `.` は無視するので
    /// `00:00:5e` でも `00005e` でもよい。
    pub hwaddr: Option<String>,
    /// DUID の前方一致 (IPv6 のみ)。
    pub duid: Option<String>,
    /// ホスト名の部分一致。
    pub hostname: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    /// `address` (既定), `expire`, `hostname`, `subnet_id`, `state`
    pub order_by: Option<String>,
    /// true で降順。
    pub desc: Option<bool>,
    /// `hex` (既定) か `colon`。
    pub hwaddr_format: Option<String>,
}

/// バインドする値。型ごとに持たないと sqlx に渡せない。
#[derive(Debug, Clone)]
pub enum Bind {
    U32(u32),
    Str(String),
}

/// 解決済みのクエリ。
#[derive(Debug)]
pub struct Plan {
    where_sql: String,
    binds: Vec<Bind>,
    order_sql: String,
    limit: Option<u32>,
    offset: Option<u32>,
    pub hwaddr_format: HwaddrFormat,
}

impl Plan {
    /// クエリパラメータを検証して SQL の断片に落とす。
    pub fn build(
        query: &LeaseQuery,
        family: Family,
        schema: &Schema,
        max_limit: u32,
    ) -> ApiResult<Self> {
        let table = family.table();
        let mut conditions: Vec<String> = Vec::new();
        let mut binds: Vec<Bind> = Vec::new();

        // --- state ---------------------------------------------------------
        // state 列が無い古いスキーマでは絞りようがないので、指定が無ければ素通し。
        let has_state = family.has_column(schema, "state");
        match query.state.as_deref() {
            Some(value) if value.eq_ignore_ascii_case("all") => {}
            Some(value) => {
                if !has_state {
                    return Err(ApiError::bad_request(format!(
                        "接続先の {table} テーブルには state 列がありません (Kea スキーマ 3.0 以降が必要です)"
                    )));
                }
                let state = resolve_state(value, schema)?;
                conditions.push(format!("{table}.state = ?"));
                binds.push(Bind::U32(state));
            }
            None => {
                if has_state {
                    conditions.push(format!("{table}.state = ?"));
                    binds.push(Bind::U32(0));
                }
            }
        }

        // --- 有効期限 ------------------------------------------------------
        // NOW() はセッションのタイムゾーンで返り、TIMESTAMP 列の比較も同じ
        // タイムゾーンで行われるので、両辺が揃う。UTC_TIMESTAMP() だと
        // セッションが UTC のときしか正しくない。
        if !query.include_expired.unwrap_or(false) {
            conditions.push(format!("{table}.expire > NOW()"));
        }

        // --- 単純な等値条件 ------------------------------------------------
        if let Some(subnet_id) = query.subnet_id {
            conditions.push(format!("{table}.subnet_id = ?"));
            binds.push(Bind::U32(subnet_id));
        }
        if let Some(pool_id) = query.pool_id {
            if !family.has_column(schema, "pool_id") {
                return Err(ApiError::bad_request(format!(
                    "接続先の {table} テーブルには pool_id 列がありません (Kea スキーマ 18 以降が必要です)"
                )));
            }
            conditions.push(format!("{table}.pool_id = ?"));
            binds.push(Bind::U32(pool_id));
        }

        // --- バイナリ列の前方一致 ------------------------------------------
        if let Some(hwaddr) = query.hwaddr.as_deref() {
            let prefix = normalize_hex(hwaddr, "hwaddr")?;
            conditions.push(format!("HEX({table}.hwaddr) LIKE CONCAT(?, '%')"));
            binds.push(Bind::Str(prefix));
        }
        if let Some(duid) = query.duid.as_deref() {
            if family != Family::V6 {
                return Err(ApiError::bad_request(
                    "duid は IPv6 のリース (/leases6) でのみ指定できます",
                ));
            }
            let prefix = normalize_hex(duid, "duid")?;
            conditions.push(format!("HEX({table}.duid) LIKE CONCAT(?, '%')"));
            binds.push(Bind::Str(prefix));
        }

        // --- ホスト名の部分一致 --------------------------------------------
        if let Some(hostname) = query.hostname.as_deref() {
            conditions.push(format!(
                "{table}.hostname LIKE CONCAT('%', ?, '%') ESCAPE '\\\\'"
            ));
            binds.push(Bind::Str(escape_like(hostname)));
        }

        // --- 並び順 --------------------------------------------------------
        // 別名ではなく実列を指すよう修飾する。IPv4 では address を
        // INET_NTOA() した別名と実列が同名なので、修飾しないと MySQL は
        // 別名 (文字列) を採用してしまい 192.0.2.9 が 192.0.2.10 より
        // 後ろに並ぶ。
        let order_column = match query.order_by.as_deref().unwrap_or("address") {
            "address" => "address",
            "expire" => "expire",
            "hostname" => "hostname",
            "subnet_id" => "subnet_id",
            "state" if has_state => "state",
            other => {
                // state 列が無い環境では state を候補に出さない。
                let mut allowed = vec!["address", "expire", "hostname", "subnet_id"];
                if has_state {
                    allowed.push("state");
                }
                return Err(ApiError::bad_request(format!(
                    "order_by に指定できるのは {} です (指定値: {other})",
                    allowed.join(", ")
                )));
            }
        };
        let direction = if query.desc.unwrap_or(false) {
            "DESC"
        } else {
            "ASC"
        };
        let order_sql = format!(" ORDER BY {table}.{order_column} {direction}");

        // --- 件数制限 ------------------------------------------------------
        if let Some(limit) = query.limit
            && limit == 0
        {
            return Err(ApiError::bad_request("limit は 1 以上で指定してください"));
        }
        let limit = query.limit.map(|limit| limit.min(max_limit));
        // MySQL は OFFSET を単体で書けないので、limit 未指定なら上限を使う。
        let limit = match (limit, query.offset) {
            (None, Some(_)) => Some(max_limit),
            (limit, _) => limit,
        };

        let hwaddr_format = match query.hwaddr_format.as_deref() {
            None => HwaddrFormat::default(),
            Some(value) => HwaddrFormat::parse(value).ok_or_else(|| {
                ApiError::bad_request(format!(
                    "hwaddr_format に指定できるのは hex か colon です (指定値: {value})"
                ))
            })?,
        };

        let where_sql = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };

        Ok(Self {
            where_sql,
            binds,
            order_sql,
            limit,
            offset: query.offset,
            hwaddr_format,
        })
    }

    /// 一覧取得用の SQL とバインド値。
    pub fn select(&self, select_list: &str, table: &str) -> (String, Vec<Bind>) {
        let mut sql = format!("SELECT {select_list} FROM {table}{}", self.where_sql);
        sql.push_str(&self.order_sql);
        let mut binds = self.binds.clone();
        if let Some(limit) = self.limit {
            sql.push_str(" LIMIT ?");
            binds.push(Bind::U32(limit));
            if let Some(offset) = self.offset {
                sql.push_str(" OFFSET ?");
                binds.push(Bind::U32(offset));
            }
        }
        (sql, binds)
    }

    /// 件数取得用の SQL とバインド値。並び順と LIMIT は意味がないので付けない。
    pub fn count(&self, table: &str) -> (String, Vec<Bind>) {
        (
            format!("SELECT COUNT(*) FROM {table}{}", self.where_sql),
            self.binds.clone(),
        )
    }
}

/// バインド値を sqlx の引数列に詰める。
pub fn arguments(binds: &[Bind]) -> ApiResult<MySqlArguments> {
    let mut args = MySqlArguments::default();
    for bind in binds {
        let result = match bind {
            Bind::U32(value) => args.add(*value),
            Bind::Str(value) => args.add(value.as_str()),
        };
        result.map_err(|error| {
            ApiError::Database(sqlx::Error::Encode(
                format!("クエリ引数を組み立てられませんでした: {error}").into(),
            ))
        })?;
    }
    Ok(args)
}

/// `state=released` のような名前、または `state=3` のような数値を解決する。
fn resolve_state(value: &str, schema: &Schema) -> ApiResult<u32> {
    if let Ok(number) = value.parse::<u32>() {
        return Ok(number);
    }
    schema.state_by_name(value).ok_or_else(|| {
        let known = schema.known_state_names();
        ApiError::bad_request(format!(
            "state に指定できるのは all, 数値, または {known} です (指定値: {value})"
        ))
    })
}

/// MAC / DUID の入力を大文字 16 進へ正規化する。
///
/// HEX() の出力に対する前方一致に使うので、奇数桁 (ニブル単位) も許す。
fn normalize_hex(input: &str, field: &str) -> ApiResult<String> {
    let normalized: String = input
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | '.' | ' ' | '_'))
        .collect();

    if normalized.is_empty() {
        return Err(ApiError::bad_request(format!(
            "{field} に 16 進の値を指定してください"
        )));
    }
    if normalized.len() > 260 {
        return Err(ApiError::bad_request(format!("{field} が長すぎます")));
    }
    if !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request(format!(
            "{field} には 16 進の文字だけを指定してください (指定値: {input})"
        )));
    }
    Ok(normalized.to_ascii_uppercase())
}

/// LIKE のメタ文字を無害化する。ESCAPE '\' と組で使う。
fn escape_like(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for c in input.chars() {
        if matches!(c, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_normalization_accepts_common_separators() {
        assert_eq!(
            normalize_hex("00:00:5e:00:53:00", "hwaddr").unwrap(),
            "00005E005300"
        );
        assert_eq!(normalize_hex("00-00-5e", "hwaddr").unwrap(), "00005E");
        assert_eq!(normalize_hex("0000.5e00", "hwaddr").unwrap(), "00005E00");
    }

    #[test]
    fn hex_normalization_rejects_non_hex() {
        assert!(normalize_hex("zz:zz", "hwaddr").is_err());
        assert!(normalize_hex("", "hwaddr").is_err());
        assert!(normalize_hex("::::", "hwaddr").is_err());
    }

    #[test]
    fn like_metacharacters_are_escaped() {
        assert_eq!(escape_like("100%_pc"), "100\\%\\_pc");
        assert_eq!(escape_like("a\\b"), "a\\\\b");
        assert_eq!(escape_like("plain"), "plain");
    }
}
