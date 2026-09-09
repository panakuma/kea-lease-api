//! 設定ファイル (config.toml) の読み込み。
//!
//! 追加した項目はすべて `#[serde(default)]` 付きなので、v0.1 時代の
//! config.toml をそのまま置いても起動する。

use anyhow::Context;
use serde::Deserialize;
use sqlx::mysql::MySqlConnectOptions;
use std::path::{Path, PathBuf};

/// 設定ファイルのパスを差し替える環境変数。
const ENV_CONFIG: &str = "KEA_LEASE_API_CONFIG";
/// DB パスワードを設定ファイルに書かずに渡すための環境変数。
const ENV_DB_PASSWORD: &str = "KEA_LEASE_API_DB_PASSWORD";

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    pub database: DatabaseConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct GeneralConfig {
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
    #[serde(default = "default_bind_port")]
    pub bind_port: u16,
    /// 1 リクエストあたりの上限時間 (秒)。超えると 408 を返す。
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// `limit` クエリパラメータの上限。1 回の応答で返す行数のハードキャップ。
    #[serde(default = "default_max_limit")]
    pub max_limit: u32,
    /// リバースプロキシ配下で X-Forwarded-For をクライアント IP として
    /// ログに出すか。信頼できる経路でのみ true にすること。
    #[serde(default)]
    pub trust_proxy_header: bool,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    pub host: String,
    #[serde(default = "default_db_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub password: String,
    pub database: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
}

fn default_bind_addr() -> String {
    "[::]".to_string()
}
fn default_bind_port() -> u16 {
    3000
}
fn default_request_timeout_secs() -> u64 {
    30
}
fn default_max_limit() -> u32 {
    10_000
}
fn default_db_port() -> u16 {
    3306
}
fn default_max_connections() -> u32 {
    5
}
fn default_connect_timeout_secs() -> u64 {
    10
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            bind_addr: default_bind_addr(),
            bind_port: default_bind_port(),
            request_timeout_secs: default_request_timeout_secs(),
            max_limit: default_max_limit(),
            trust_proxy_header: false,
        }
    }
}

impl Config {
    /// 設定ファイルを探して読み込む。
    ///
    /// 優先順位は 第1引数 > `KEA_LEASE_API_CONFIG` > カレントディレクトリの
    /// `config.toml`。systemd から起動する場合に WorkingDirectory へ依存しなくて
    /// 済むよう、明示指定できるようにしてある。
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::resolve_path();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("設定ファイルを読めませんでした: {}", path.display()))?;
        let mut config: Config = toml::from_str(&text)
            .with_context(|| format!("設定ファイルを解析できませんでした: {}", path.display()))?;

        if let Ok(password) = std::env::var(ENV_DB_PASSWORD) {
            config.database.password = password;
        }
        config.validate()?;
        Ok(config)
    }

    fn resolve_path() -> PathBuf {
        if let Some(arg) = std::env::args().nth(1) {
            return PathBuf::from(arg);
        }
        if let Ok(path) = std::env::var(ENV_CONFIG) {
            return PathBuf::from(path);
        }
        PathBuf::from("config.toml")
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.general.max_limit > 0,
            "general.max_limit は 1 以上である必要があります"
        );
        anyhow::ensure!(
            self.general.request_timeout_secs > 0,
            "general.request_timeout_secs は 1 以上である必要があります"
        );
        anyhow::ensure!(
            self.database.max_connections > 0,
            "database.max_connections は 1 以上である必要があります"
        );
        Ok(())
    }

    /// 待ち受けアドレス。`[::]:3000` のような形にする。
    pub fn bind_address(&self) -> String {
        format!("{}:{}", self.general.bind_addr, self.general.bind_port)
    }
}

impl DatabaseConfig {
    /// 接続オプションを組み立てる。
    ///
    /// URL 文字列を経由しないので、パスワードに `@` や `/` が含まれていても
    /// パーセントエンコードを気にしなくてよい。
    ///
    /// `time_zone` は必ず `+00:00` に固定する。Kea は expire を TIMESTAMP 型に
    /// 格納しており、MySQL は TIMESTAMP をセッションのタイムゾーンへ変換して
    /// 返す。sqlx は取り出した値を無条件に UTC とみなすため、ここを UTC 以外に
    /// するとレスポンスの expire がずれる。
    pub fn connect_options(&self) -> MySqlConnectOptions {
        MySqlConnectOptions::new()
            .host(&self.host)
            .port(self.port)
            .username(&self.user)
            .password(&self.password)
            .database(&self.database)
            .timezone(Some("+00:00".to_string()))
    }

    /// 起動ログ用の、パスワードを含まない接続先表記。
    pub fn display_target(&self) -> String {
        format!(
            "mysql://{}@{}:{}/{}",
            self.user, self.host, self.port, self.database
        )
    }
}

/// 実際に読み込んだ設定ファイルのパス (ログ表示用)。
pub fn config_path_hint() -> impl AsRef<Path> {
    Config::resolve_path()
}
