//! 接続先の Kea スキーマを起動時に調べておく。
//!
//! lease4 / lease6 の列は Kea のバージョンによって増えてきた。
//! (state は 3.0、user_context は 7.0、relay_id / remote_id は 16、
//!  pool_id は 18 で追加。released / registered という state 値も後年の追加。)
//! 存在しない列を SELECT すると即エラーになるので、information_schema を
//! 見て使える列だけを組み立てる。
//!
//! state 名や lease type 名も Kea 自身が DB に持っている
//! (lease_state / lease6_types / lease_hwaddr_source テーブル) ので、
//! ハードコードせずそこから読む。将来 Kea が値を増やしても追従できる。

use sqlx::{MySql, Pool, Row};
use std::collections::{BTreeMap, HashMap};

/// 接続先スキーマから読み取った情報。
#[derive(Debug, Clone)]
pub struct Schema {
    /// 列名 -> DATA_TYPE ("int", "binary", "varchar" など)。
    lease4_columns: HashMap<String, String>,
    lease6_columns: HashMap<String, String>,
    /// state 値 -> 名前 (0 = "default", 2 = "expired-reclaimed" など)。
    pub lease_states: BTreeMap<u32, String>,
    /// lease6.lease_type 値 -> 名前 (0 = "IA_NA" など)。
    pub lease6_types: BTreeMap<i8, String>,
    /// lease6.hwaddr_source 値 -> 名前。
    pub hwaddr_sources: BTreeMap<u32, String>,
}

impl Schema {
    pub async fn detect(pool: &Pool<MySql>) -> Result<Self, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT TABLE_NAME AS table_name,
                   COLUMN_NAME AS column_name,
                   DATA_TYPE AS data_type
            FROM information_schema.COLUMNS
            WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME IN ('lease4', 'lease6')
            "#,
        )
        .fetch_all(pool)
        .await?;

        let mut lease4_columns = HashMap::new();
        let mut lease6_columns = HashMap::new();
        for row in rows {
            let table: String = row.try_get("table_name")?;
            let column: String = row.try_get("column_name")?;
            let data_type: String = row.try_get("data_type").unwrap_or_default();
            match table.as_str() {
                "lease4" => {
                    lease4_columns.insert(column, data_type.to_ascii_lowercase());
                }
                "lease6" => {
                    lease6_columns.insert(column, data_type.to_ascii_lowercase());
                }
                _ => {}
            }
        }

        Ok(Self {
            lease4_columns,
            lease6_columns,
            lease_states: load_lease_states(pool).await,
            lease6_types: load_lease6_types(pool).await,
            hwaddr_sources: load_hwaddr_sources(pool).await,
        })
    }

    pub fn has_lease4(&self) -> bool {
        !self.lease4_columns.is_empty()
    }

    pub fn has_lease6(&self) -> bool {
        !self.lease6_columns.is_empty()
    }

    pub fn lease4_has(&self, column: &str) -> bool {
        self.lease4_columns.contains_key(column)
    }

    pub fn lease6_has(&self, column: &str) -> bool {
        self.lease6_columns.contains_key(column)
    }

    /// lease6.address がバイナリ列かどうか。
    ///
    /// Kea スキーマ 19.0 で `VARCHAR(39)` のテキスト表現から
    /// `BINARY(16)` (INET6_ATON と同じ 16 バイト) に変わった。
    /// どちらのスキーマでも読めるよう、型を見て扱いを分ける。
    pub fn lease6_address_is_binary(&self) -> bool {
        self.lease6_columns
            .get("address")
            .is_some_and(|data_type| matches!(data_type.as_str(), "binary" | "varbinary" | "blob"))
    }

    /// state の名前を引く。未知の値なら None。
    pub fn state_name(&self, state: u32) -> Option<&str> {
        self.lease_states.get(&state).map(String::as_str)
    }

    /// lease6 の lease type 名を引く。
    pub fn lease6_type_name(&self, lease_type: i8) -> Option<&str> {
        self.lease6_types.get(&lease_type).map(String::as_str)
    }

    /// hwaddr_source の名前を引く。
    pub fn hwaddr_source_name(&self, source: u32) -> Option<&str> {
        self.hwaddr_sources.get(&source).map(String::as_str)
    }

    /// `state=released` のような名前指定を数値へ解決する。
    pub fn state_by_name(&self, name: &str) -> Option<u32> {
        self.lease_states
            .iter()
            .find(|(_, candidate)| candidate.eq_ignore_ascii_case(name))
            .map(|(value, _)| *value)
    }

    /// エラーメッセージ用に、指定できる state 名を並べる。
    pub fn known_state_names(&self) -> String {
        self.lease_states
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
impl Schema {
    /// 列一覧だけを与えて組み立てる。接続先スキーマ違いの分岐を
    /// DB 無しで確かめるためのもの。
    pub fn from_columns(lease4: &[(&str, &str)], lease6: &[(&str, &str)]) -> Self {
        fn columns(entries: &[(&str, &str)]) -> HashMap<String, String> {
            entries
                .iter()
                .map(|(name, data_type)| (name.to_string(), data_type.to_string()))
                .collect()
        }
        Self {
            lease4_columns: columns(lease4),
            lease6_columns: columns(lease6),
            lease_states: BUILTIN_LEASE_STATES
                .iter()
                .map(|(state, name)| (*state, name.to_string()))
                .collect(),
            lease6_types: BTreeMap::new(),
            hwaddr_sources: BTreeMap::new(),
        }
    }
}

/// lease_state が読めなかったときに使う、Kea が定義している既知の値。
/// メトリクスのラベルが環境によって `state-0` になったり `default` に
/// なったりしないよう、名前は常に引けるようにしておく。
const BUILTIN_LEASE_STATES: [(u32, &str); 5] = [
    (0, "default"),
    (1, "declined"),
    (2, "expired-reclaimed"),
    (3, "released"),
    (4, "registered"),
];

/// コードテーブルは無くても致命的ではない (名前が引けなくなるだけ) ので、
/// 読めなければ警告だけ出して組み込みの定義で続行する。
async fn load_lease_states(pool: &Pool<MySql>) -> BTreeMap<u32, String> {
    let loaded: BTreeMap<u32, String> = match sqlx::query("SELECT state, name FROM lease_state")
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| {
                let state: u32 = row.try_get("state").ok()?;
                let name: String = row.try_get("name").ok()?;
                Some((state, name))
            })
            .collect(),
        Err(error) => {
            tracing::warn!("lease_state テーブルを読めませんでした: {error}");
            BTreeMap::new()
        }
    };

    if loaded.is_empty() {
        BUILTIN_LEASE_STATES
            .iter()
            .map(|(state, name)| (*state, name.to_string()))
            .collect()
    } else {
        loaded
    }
}

async fn load_lease6_types(pool: &Pool<MySql>) -> BTreeMap<i8, String> {
    match sqlx::query("SELECT lease_type, name FROM lease6_types")
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| {
                let lease_type: i8 = row.try_get("lease_type").ok()?;
                let name: String = row.try_get("name").ok()?;
                Some((lease_type, name))
            })
            .collect(),
        Err(error) => {
            tracing::warn!("lease6_types テーブルを読めませんでした: {error}");
            BTreeMap::new()
        }
    }
}

async fn load_hwaddr_sources(pool: &Pool<MySql>) -> BTreeMap<u32, String> {
    match sqlx::query("SELECT hwaddr_source, name FROM lease_hwaddr_source")
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| {
                let source: u32 = row.try_get("hwaddr_source").ok()?;
                let name: String = row.try_get("name").ok()?;
                Some((source, name))
            })
            .collect(),
        Err(error) => {
            tracing::warn!("lease_hwaddr_source テーブルを読めませんでした: {error}");
            BTreeMap::new()
        }
    }
}

/// Kea のスキーマバージョン (schema_version テーブル)。
pub async fn schema_version(pool: &Pool<MySql>) -> Option<String> {
    let row = sqlx::query("SELECT version, minor FROM schema_version")
        .fetch_one(pool)
        .await
        .ok()?;
    let major: i32 = row.try_get("version").ok()?;
    let minor: i32 = row.try_get("minor").ok()?;
    Some(format!("{major}.{minor}"))
}
