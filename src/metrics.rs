use axum::{
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
};
use std::fmt::Write;

use crate::AppState;

const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

pub(crate) async fn metrics(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    // Derive both metrics from one query so they describe the same snapshot.
    let counts = sqlx::query_as::<_, (u32, i64)>(
        "SELECT subnet_id, COUNT(*) FROM lease4 GROUP BY subnet_id ORDER BY subnet_id",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|error| {
        tracing::error!(%error, "Failed to collect lease metrics");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Failed to collect lease metrics\n",
        )
    })?;

    Ok((
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        render_metrics(&counts),
    ))
}

fn render_metrics(counts: &[(u32, i64)]) -> String {
    let total: i64 = counts.iter().map(|(_, count)| count).sum();
    let mut output = String::from(
        "# HELP kea_lease4_records Number of IPv4 lease records in the database, including expired leases.\n\
         # TYPE kea_lease4_records gauge\n",
    );
    writeln!(output, "kea_lease4_records {total}").expect("writing to a String cannot fail");
    output.push_str(
        "# HELP kea_lease4_subnet_records Number of IPv4 lease records per subnet, including expired leases.\n\
         # TYPE kea_lease4_subnet_records gauge\n",
    );
    for (subnet_id, count) in counts {
        writeln!(
            output,
            "kea_lease4_subnet_records{{subnet_id=\"{subnet_id}\"}} {count}"
        )
        .expect("writing to a String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, response::IntoResponse};
    use sqlx::mysql::MySqlPoolOptions;

    #[test]
    fn counts_all_subnets_and_uses_gauges() {
        let output = render_metrics(&[(1, 2), (u32::MAX, 3)]);
        assert!(output.contains("# TYPE kea_lease4_records gauge\nkea_lease4_records 5\n"));
        assert!(output.contains("# TYPE kea_lease4_subnet_records gauge\n"));
        assert!(output.contains("kea_lease4_subnet_records{subnet_id=\"1\"} 2\n"));
        assert!(output.ends_with("kea_lease4_subnet_records{subnet_id=\"4294967295\"} 3\n"));
    }

    #[test]
    fn empty_database_has_zero_total_and_no_subnet_samples() {
        let output = render_metrics(&[]);
        assert!(output.contains("\nkea_lease4_records 0\n"));
        assert!(!output.contains("subnet_id="));
        assert!(output.ends_with('\n'));
    }

    #[tokio::test]
    async fn database_failure_returns_503_without_metrics_or_database_details() {
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://localhost/unused")
            .unwrap();
        pool.close().await;
        let response = metrics(State(AppState { pool })).await.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(body.as_ref(), b"Failed to collect lease metrics\n");
    }
}
