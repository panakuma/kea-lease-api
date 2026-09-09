//! ハンドラ間で共有する状態。

use crate::error::{ApiError, ApiResult};
use crate::schema::Schema;
use sqlx::{MySql, Pool};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: Pool<MySql>,
    pub schema: Arc<Schema>,
    pub max_limit: u32,
    pub trust_proxy_header: bool,
}

impl AppState {
    /// lease4 が無い DB (Kea の DHCPv6 専用構成など) を指していた場合に
    /// 500 ではなく 404 を返す。
    pub fn require_lease4(&self) -> ApiResult<()> {
        if self.schema.has_lease4() {
            Ok(())
        } else {
            Err(ApiError::not_found(
                "接続先のデータベースに lease4 テーブルがありません",
            ))
        }
    }

    pub fn require_lease6(&self) -> ApiResult<()> {
        if self.schema.has_lease6() {
            Ok(())
        } else {
            Err(ApiError::not_found(
                "接続先のデータベースに lease6 テーブルがありません",
            ))
        }
    }
}
