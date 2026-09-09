//! lease4 / lease6 の行を JSON へ写すモデル。
//!
//! 列は Kea のバージョンで増減するので、`#[derive(FromRow)]` ではなく
//! 「あれば読む、無ければ null」で組み立てる。

use crate::schema::Schema;
use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use sqlx::{MySql, Row, mysql::MySqlRow};
use std::net::Ipv6Addr;

/// MAC アドレス等のバイナリ列の見せ方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HwaddrFormat {
    /// `00005E005300` — Kea の lease4Dump と同じ大文字 16 進。v0.1 と同じ。
    #[default]
    Hex,
    /// `00:00:5e:00:53:00` — 一般的な MAC 表記。
    Colon,
}

impl HwaddrFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "hex" => Some(HwaddrFormat::Hex),
            "colon" | "mac" => Some(HwaddrFormat::Colon),
            _ => None,
        }
    }

    /// DB から取れた大文字 16 進文字列を指定の形式に整える。
    fn apply(self, hex: Option<String>) -> Option<String> {
        let hex = hex?;
        match self {
            HwaddrFormat::Hex => Some(hex),
            HwaddrFormat::Colon => Some(
                hex.to_ascii_lowercase()
                    .as_bytes()
                    .chunks(2)
                    .map(|pair| String::from_utf8_lossy(pair).into_owned())
                    .collect::<Vec<_>>()
                    .join(":"),
            ),
        }
    }
}

/// IPv4 リース 1 件。
///
/// 先頭 7 項目とその並びは v0.1 の応答と同じ。以降は今回追加した項目で、
/// 接続先のスキーマに列が無い場合は null になる。
#[derive(Debug, Serialize)]
pub struct Lease4 {
    pub address: Option<String>,
    pub hwaddr: Option<String>,
    pub client_id: Option<String>,
    pub valid_lifetime: Option<u32>,
    pub expire: Option<DateTime<Utc>>,
    pub subnet_id: Option<u32>,
    pub hostname: Option<String>,
    /// client last transmission time。Kea は保持していないので
    /// `expire - valid_lifetime` から復元した値。
    pub cltt: Option<DateTime<Utc>>,
    pub state: Option<u32>,
    pub state_name: Option<String>,
    pub pool_id: Option<u32>,
    pub fqdn_fwd: Option<bool>,
    pub fqdn_rev: Option<bool>,
    pub user_context: Option<serde_json::Value>,
    pub relay_id: Option<String>,
    pub remote_id: Option<String>,
}

impl Lease4 {
    pub fn from_row(row: &MySqlRow, schema: &Schema, hwaddr_format: HwaddrFormat) -> Self {
        let valid_lifetime: Option<u32> = optional(row, "valid_lifetime");
        let expire: Option<DateTime<Utc>> = optional(row, "expire");
        let state: Option<u32> = optional(row, "state");

        Self {
            address: optional(row, "address"),
            hwaddr: hwaddr_format.apply(optional(row, "hwaddr")),
            client_id: optional(row, "client_id"),
            valid_lifetime,
            expire,
            subnet_id: optional(row, "subnet_id"),
            hostname: optional(row, "hostname"),
            cltt: cltt(expire, valid_lifetime),
            state,
            state_name: state
                .and_then(|state| schema.state_name(state))
                .map(str::to_string),
            pool_id: optional(row, "pool_id"),
            fqdn_fwd: optional(row, "fqdn_fwd"),
            fqdn_rev: optional(row, "fqdn_rev"),
            user_context: optional::<String>(row, "user_context").map(user_context),
            relay_id: hwaddr_format.apply(optional(row, "relay_id")),
            remote_id: hwaddr_format.apply(optional(row, "remote_id")),
        }
    }
}

/// IPv6 リース 1 件。
#[derive(Debug, Serialize)]
pub struct Lease6 {
    pub address: Option<String>,
    pub duid: Option<String>,
    pub valid_lifetime: Option<u32>,
    pub expire: Option<DateTime<Utc>>,
    pub cltt: Option<DateTime<Utc>>,
    pub subnet_id: Option<u32>,
    pub pref_lifetime: Option<u32>,
    pub lease_type: Option<i8>,
    pub lease_type_name: Option<String>,
    pub iaid: Option<u32>,
    pub prefix_len: Option<u8>,
    pub hostname: Option<String>,
    pub hwaddr: Option<String>,
    pub hwtype: Option<u16>,
    pub hwaddr_source: Option<u32>,
    pub hwaddr_source_name: Option<String>,
    pub state: Option<u32>,
    pub state_name: Option<String>,
    pub pool_id: Option<u32>,
    pub fqdn_fwd: Option<bool>,
    pub fqdn_rev: Option<bool>,
    pub user_context: Option<serde_json::Value>,
}

impl Lease6 {
    pub fn from_row(row: &MySqlRow, schema: &Schema, hwaddr_format: HwaddrFormat) -> Self {
        let valid_lifetime: Option<u32> = optional(row, "valid_lifetime");
        let expire: Option<DateTime<Utc>> = optional(row, "expire");
        let state: Option<u32> = optional(row, "state");
        let lease_type: Option<i8> = optional(row, "lease_type");
        let hwaddr_source: Option<u32> = optional(row, "hwaddr_source");

        Self {
            address: lease6_address(row, schema),
            duid: hwaddr_format.apply(optional(row, "duid")),
            valid_lifetime,
            expire,
            cltt: cltt(expire, valid_lifetime),
            subnet_id: optional(row, "subnet_id"),
            pref_lifetime: optional(row, "pref_lifetime"),
            lease_type,
            lease_type_name: lease_type
                .and_then(|lease_type| schema.lease6_type_name(lease_type))
                .map(str::to_string),
            iaid: optional(row, "iaid"),
            prefix_len: optional(row, "prefix_len"),
            hostname: optional(row, "hostname"),
            hwaddr: hwaddr_format.apply(optional(row, "hwaddr")),
            hwtype: optional(row, "hwtype"),
            hwaddr_source,
            hwaddr_source_name: hwaddr_source
                .and_then(|source| schema.hwaddr_source_name(source))
                .map(str::to_string),
            state,
            state_name: state
                .and_then(|state| schema.state_name(state))
                .map(str::to_string),
            pool_id: optional(row, "pool_id"),
            fqdn_fwd: optional(row, "fqdn_fwd"),
            fqdn_rev: optional(row, "fqdn_rev"),
            user_context: optional::<String>(row, "user_context").map(user_context),
        }
    }
}

/// 列があれば読み、無ければ None。
///
/// SELECT リストは接続先スキーマに合わせて組み立てているので
/// `ColumnNotFound` は想定内。デコードに失敗した場合も、1 件のために
/// 応答全体を落とすより null で返したほうが実用的なのでログに残して続行する。
fn optional<'r, T>(row: &'r MySqlRow, name: &str) -> Option<T>
where
    T: sqlx::Decode<'r, MySql> + sqlx::Type<MySql>,
{
    match row.try_get::<Option<T>, _>(name) {
        Ok(value) => value,
        Err(sqlx::Error::ColumnNotFound(_)) => None,
        Err(error) => {
            tracing::warn!("列 {name} を読めませんでした: {error}");
            None
        }
    }
}

/// lease6.address を文字列にする。
///
/// Kea スキーマ 19.0 以降は BINARY(16) の生アドレスなので、DB の
/// INET6_NTOA() に頼らず Rust 側で RFC 5952 の表記へ整える。
/// 18 以前は VARCHAR(39) のテキストなのでそのまま読む。
fn lease6_address(row: &MySqlRow, schema: &Schema) -> Option<String> {
    if schema.lease6_address_is_binary() {
        optional::<Vec<u8>>(row, "address").and_then(format_ipv6)
    } else {
        optional(row, "address")
    }
}

/// BINARY(16) の 16 バイトを IPv6 アドレス表記にする。
fn format_ipv6(bytes: Vec<u8>) -> Option<String> {
    let octets: [u8; 16] = bytes.try_into().ok()?;
    Some(Ipv6Addr::from(octets).to_string())
}

/// Kea は cltt (最終通信時刻) を DB に持たず expire = cltt + valid_lifetime と
/// して保存する。ここでは逆算して返す。
fn cltt(expire: Option<DateTime<Utc>>, valid_lifetime: Option<u32>) -> Option<DateTime<Utc>> {
    let expire = expire?;
    let valid_lifetime = valid_lifetime?;
    expire.checked_sub_signed(TimeDelta::seconds(i64::from(valid_lifetime)))
}

/// user_context は JSON テキスト。読めれば JSON として、駄目なら文字列のまま返す。
fn user_context(raw: String) -> serde_json::Value {
    serde_json::from_str(&raw).unwrap_or(serde_json::Value::String(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colon_format_splits_every_octet() {
        let formatted = HwaddrFormat::Colon.apply(Some("00005E005300".to_string()));
        assert_eq!(formatted.as_deref(), Some("00:00:5e:00:53:00"));
    }

    #[test]
    fn hex_format_is_untouched() {
        let formatted = HwaddrFormat::Hex.apply(Some("00005E005300".to_string()));
        assert_eq!(formatted.as_deref(), Some("00005E005300"));
    }

    #[test]
    fn cltt_is_expire_minus_lifetime() {
        let expire = DateTime::parse_from_rfc3339("2026-03-31T07:49:01Z")
            .unwrap()
            .with_timezone(&Utc);
        let derived = cltt(Some(expire), Some(3600)).unwrap();
        assert_eq!(derived.to_rfc3339(), "2026-03-31T06:49:01+00:00");
    }

    #[test]
    fn cltt_needs_both_values() {
        assert!(cltt(None, Some(3600)).is_none());
        assert!(cltt(Some(Utc::now()), None).is_none());
    }

    #[test]
    fn binary_address_is_formatted_as_ipv6() {
        let mut octets = vec![0u8; 16];
        octets[0] = 0x20;
        octets[1] = 0x01;
        octets[2] = 0x0d;
        octets[3] = 0xb8;
        octets[15] = 0x01;
        assert_eq!(format_ipv6(octets).as_deref(), Some("2001:db8::1"));
    }

    #[test]
    fn binary_address_needs_16_bytes() {
        assert!(format_ipv6(vec![0u8; 4]).is_none());
    }

    #[test]
    fn user_context_falls_back_to_string() {
        assert_eq!(
            user_context("{\"a\":1}".to_string()),
            serde_json::json!({"a": 1})
        );
        assert_eq!(
            user_context("not json".to_string()),
            serde_json::Value::String("not json".to_string())
        );
    }
}
