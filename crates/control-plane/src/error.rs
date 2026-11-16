//! Every failure leaves as an [`ApiError`] body.

use axum::{
    Json,
    extract::rejection::{JsonRejection, QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use hallow_proto::{ApiError, ErrorCode, fleet::FieldErrors};

use crate::store::StoreError;

#[derive(Debug)]
pub struct Failure {
    status: StatusCode,
    code: ErrorCode,
    message: String,
    fields: FieldErrors,
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
            fields: FieldErrors::new(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    /// A `403`: the token is good but can't do this. The code stays
    /// `unauthorized`, which every client already knows.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            ..Self::new(ErrorCode::Unauthorized, message)
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    /// A `400` naming what is wrong with each field of a form.
    pub fn invalid_fields(message: impl Into<String>, fields: FieldErrors) -> Self {
        Self {
            fields,
            ..Self::bad_request(message)
        }
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ApiError {
                code: self.code,
                message: self.message,
                fields: self.fields,
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
