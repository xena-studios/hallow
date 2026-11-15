//! Every failure leaves as an [`ApiError`] body.

use axum::{
    Json,
    extract::rejection::{JsonRejection, QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use hallow_proto::{ApiError, ErrorCode};

use crate::store::StoreError;

#[derive(Debug)]
pub struct Failure {
    status: StatusCode,
    code: ErrorCode,
    message: String,
}

impl Failure {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let status = match code {
            ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
            ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::Conflict => StatusCode::CONFLICT,
            ErrorCode::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        };
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ApiError {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

impl From<StoreError> for Failure {
    fn from(err: StoreError) -> Self {
        tracing::error!(%err, "store error");
        Self::new(ErrorCode::Unavailable, "live state is unavailable; retry")
    }
}

impl From<hallow_db::sqlx::Error> for Failure {
    fn from(err: hallow_db::sqlx::Error) -> Self {
        tracing::error!(%err, "database error");
        Self::new(ErrorCode::Unavailable, "the database is unavailable; retry")
    }
}

impl From<JsonRejection> for Failure {
    fn from(rejection: JsonRejection) -> Self {
        Self::bad_request(rejection.body_text())
    }
}

impl From<QueryRejection> for Failure {
    fn from(rejection: QueryRejection) -> Self {
        Self::bad_request(rejection.body_text())
    }
}
