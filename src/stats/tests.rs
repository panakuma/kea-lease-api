use super::*;
use crate::handlers::{count_leases4, count_leases6, list_leases4, list_leases6};
use crate::query::{LeaseQuery, ValidatedQuery};
use crate::schema::Schema;
use axum::body::to_bytes;
use chrono::DateTime;
use sqlx::mysql::MySqlPoolOptions;
use std::sync::Arc;

/// 実DBでSQLのNULL処理とAND/ORの優先順位も確認する。
/// 接続先には一時テーブルだけを作り、既存のリーステーブルは変更しない。
#[tokio::test]
#[ignore = "requires KEA_LEASE_API_TEST_DATABASE_URL (MySQL/MariaDB)"]
async fn infinite_leases_are_included_in_lists_counts_stats_and_metrics() {
    let url = std::env::var("KEA_LEASE_API_TEST_DATABASE_URL").unwrap();
    // 一時テーブルと固定時刻をすべての問い合わせで共有する。
    let pool = MySqlPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    sqlx::query("SET time_zone = '+00:00', timestamp = 1800000000")
        .execute(&pool)
        .await
        .unwrap();

    // state列のない旧スキーマと、state列・バイナリIPv6アドレスのあるスキーマ。
    for has_state in [false, true] {
        let mut v4_columns = vec![
            ("address", "int"),
            ("valid_lifetime", "int"),
            ("expire", "timestamp"),
            ("subnet_id", "int"),
        ];
        if has_state {
            v4_columns.push(("state", "int"));
        }
        let mut v6_columns = v4_columns.clone();
        v6_columns[0] = ("address", if has_state { "binary" } else { "varchar" });
        let schema = Arc::new(Schema::from_columns(&v4_columns, &v6_columns));
        let app = AppState {
            pool: pool.clone(),
            lease4: Arc::new(LeaseCapability::detect(Family::V4, schema.clone())),
            lease6: Arc::new(LeaseCapability::detect(Family::V6, schema.clone())),
            schema,
            max_limit: 100,
            trust_proxy_header: false,
        };
        for family in [Family::V4, Family::V6] {
            let table = family.table();
            let address_type = match family {
                Family::V4 => "INT UNSIGNED",
                Family::V6 if has_state => "BINARY(16)",
                Family::V6 => "VARCHAR(39)",
            };
            let state_column = if has_state {
                ", state INT UNSIGNED"
            } else {
                ""
            };
            sqlx::query(&format!(
                "CREATE TEMPORARY TABLE {table} (
                    address {address_type} PRIMARY KEY,
                    valid_lifetime INT UNSIGNED, expire TIMESTAMP NULL,
                    subnet_id INT UNSIGNED{state_column})"
            ))
            .execute(&pool)
            .await
            .unwrap();

            // id=1はKeaが保存する無期限リース: expire = cltt (過去の時刻)。
            let samples = [
                (0xffff_ffff_u32, Some(-3600), 1, 0),
                (3600, Some(3600), 1, 0),
                (3600, Some(-3600), 1, 0),
                (3600, None, 1, 0),
                (0xffff_ffff, Some(-3600), 1, 1),
                (0xffff_ffff, Some(-3600), 1, 2),
                (0xffff_ffff, Some(-3600), 1, 3),
                (0xffff_ffff, Some(-3600), 1, 4),
                (0xffff_ffff, Some(-3600), 2, 0),
                (0xffff_ffff, None, 3, 0),
            ];
            for (index, (lifetime, seconds, subnet, state)) in samples.iter().enumerate() {
                let id = index + 1;
                let address = match family {
                    Family::V4 => format!("INET_ATON('192.0.2.{id}')"),
                    Family::V6 if has_state => format!("INET6_ATON('2001:db8::{id}')"),
                    Family::V6 => format!("'2001:db8::{id}'"),
                };
                let expire = seconds.map_or("NULL".to_string(), |s| {
                    format!("TIMESTAMPADD(SECOND, {s}, NOW())")
                });
                let state_value = if has_state {
                    format!(", {state}")
                } else {
                    String::new()
                };
                sqlx::query(&format!(
                    "INSERT INTO {table} VALUES ({address}, {lifetime}, {expire}, {subnet}{state_value})"
                ))
                .execute(&pool)
                .await
                .unwrap();
            }
        }

        for (state, include_expired, subnet_id, expected) in [
            (None, false, None, if has_state { 4 } else { 8 }),
            (None, false, Some(1), if has_state { 2 } else { 6 }),
            (Some("all"), false, None, 8),
            (Some("all"), true, None, 10),
        ] {
            let query = || {
                ValidatedQuery(LeaseQuery {
                    state: state.map(str::to_string),
                    include_expired: Some(include_expired),
                    subnet_id,
                    ..Default::default()
                })
            };
            let v4 = list_leases4(State(app.clone()), query()).await.unwrap().0;
            let v6 = list_leases6(State(app.clone()), query()).await.unwrap().0;
            assert_eq!(v4.len(), expected);
            assert_eq!(v6.len(), expected);
            let stored_cltt = DateTime::from_timestamp(1799996400, 0);
            assert_eq!(v4[0].address.as_deref(), Some("192.0.2.1"));
            assert_eq!(v6[0].address.as_deref(), Some("2001:db8::1"));
            assert_eq!(v4[0].cltt, stored_cltt);
            assert_eq!(v6[0].cltt, stored_cltt);
            assert_eq!(v4[0].expire, stored_cltt);
            assert_eq!(v6[0].expire, stored_cltt);
            assert_eq!(
                count_leases4(State(app.clone()), query()).await.unwrap().0,
                expected as i64
            );
            assert_eq!(
                count_leases6(State(app.clone()), query()).await.unwrap().0,
                expected as i64
            );
        }

        let summary = stats(State(app.clone())).await.unwrap().0;
        for family in [summary.lease4.unwrap(), summary.lease6.unwrap()] {
            assert_eq!(family.total, 10);
            assert_eq!(family.active, if has_state { 4 } else { 8 });
            assert_eq!(family.by_subnet[0].active, if has_state { 2 } else { 6 });
        }
        let response = metrics(State(app)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 16384).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        for family in ["lease4", "lease6"] {
            let active = if has_state { 2 } else { 6 };
            assert!(body.contains(&format!(
                "kea_{family}_active_leases{{subnet_id=\"1\"}} {active}\n"
            )));
            assert!(body.contains(&format!(
                "kea_{family}_active_leases{{subnet_id=\"2\"}} 1\n"
            )));
            assert!(body.contains(&format!(
                "kea_{family}_active_leases{{subnet_id=\"3\"}} 1\n"
            )));
            sqlx::query(&format!("DROP TEMPORARY TABLE {family}"))
                .execute(&pool)
                .await
                .unwrap();
        }
    }
    pool.close().await;
}
