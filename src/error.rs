//! API のエラー型。
//!
//! v0.1 ではクエリ失敗時に `.expect()` していたためハンドラごとパニックして
//! いた。ここで型を用意して、すべて JSON のエラー応答に落とす。

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug)]
pub enum ApiError {
    /// クエリパラメータやパス変数が不正。
    BadRequest(String),
    /// 指定されたリースが存在しない。
    NotFound(String),
    /// データベースアクセスに失敗した。
    Database(sqlx::Error),
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError::BadRequest(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        ApiError::NotFound(message.into())
    }

    fn parts(&self) -> (StatusCode, &'static str, String) {
        match self {
            ApiError::BadRequest(message) => {
                (StatusCode::BAD_REQUEST, "bad_request", message.clone())
            }
            ApiError::NotFound(message) => (StatusCode::NOT_FOUND, "not_found", message.clone()),
            // DB エラーの中身はスキーマや接続情報を含みうるので、クライアントには
            // 返さずログにだけ出す。
            ApiError::Database(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "database_error",
                "データベースへの問い合わせに失敗しました".to_string(),
            ),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        ApiError::Database(error)
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, message) = self.parts();
        if let ApiError::Database(source) = &self {
            tracing::error!("database error: {source}");
        }
        (status, Json(ErrorBody { error, message })).into_response()
    }
}

/// ハンドラの戻り値に使う別名。
pub type ApiResult<T> = Result<T, ApiError>;
