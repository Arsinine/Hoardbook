use axum::{http::StatusCode, response::{IntoResponse, Response}};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("payload too large")]
    TooLarge,

    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Internal(anyhow::anyhow!(e))
    }
}

impl From<hb_core::HbError> for AppError {
    fn from(e: hb_core::HbError) -> Self {
        AppError::BadRequest(e.to_string())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, body) = match &self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload too large".to_string(),
            ),
            AppError::Internal(e) => {
                tracing::error!("internal error: {e:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
        };
        (status, body).into_response()
    }
}
