//! ハンドラ間で共有する状態。

use crate::capability::{Family, LeaseCapability};
use crate::error::{ApiError, ApiResult};
use crate::schema::Schema;
use sqlx::{MySql, Pool};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: Pool<MySql>,
    pub schema: Arc<Schema>,
    pub lease4: Arc<LeaseCapability>,
    pub lease6: Arc<LeaseCapability>,
    pub max_limit: u32,
    pub trust_proxy_header: bool,
}

impl AppState {
    pub fn lease(&self, family: Family) -> &LeaseCapability {
        match family {
            Family::V4 => self.lease4.as_ref(),
            Family::V6 => self.lease6.as_ref(),
        }
    }

    /// 対象テーブルが無い DB (Kea の DHCPv6 専用構成など) を指していた場合に
    /// 500 ではなく 404 を返す。
    pub fn capability(&self, family: Family) -> ApiResult<&LeaseCapability> {
        let capability = self.lease(family);
        if capability.present() {
            Ok(capability)
        } else {
            Err(ApiError::not_found(format!(
                "接続先のデータベースに {} テーブルがありません",
                capability.table()
            )))
        }
    }
}
