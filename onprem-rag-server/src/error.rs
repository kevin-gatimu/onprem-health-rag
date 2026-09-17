//! Unified error type that serializes to a JSON body and a sensible HTTP status.

use rocket::http::{ContentType, Status};
use rocket::request::Request;
use rocket::response::{self, Responder, Response};
use rocket::serde::json::json;
use std::io::Cursor;

/// Application-wide error. Every fallible route returns `Result<T, AppError>`.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("not found")]
    NotFound,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("database error: {0}")]
    Database(#[from] mongodb::error::Error),

    #[error("service unavailable: {0}")]
    Unavailable(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("too many requests: {0}")]
    TooManyRequests(String),
}

impl AppError {
    /// A PHI-free label for the *class* of failure.
    ///
    /// `Display` for these variants interpolates the inner message, and those
    /// messages can echo a bound parameter or a driver's rendering of a row —
    /// so `Display` must never reach execution provenance (plan 04a §7). This
    /// gives the provenance path something safe to report instead.
    pub fn kind_label(&self) -> &'static str {
        match self {
            AppError::Unauthorized => "unauthorized",
            AppError::Forbidden => "forbidden",
            AppError::NotFound => "not found",
            AppError::BadRequest(_) => "bad request",
            AppError::Database(_) => "database error",
            AppError::Unavailable(_) => "service unavailable",
            AppError::Internal(_) => "internal error",
            AppError::TooManyRequests(_) => "too many requests",
        }
    }

    fn status(&self) -> Status {
        match self {
            AppError::Unauthorized => Status::Unauthorized,
            AppError::Forbidden => Status::Forbidden,
            AppError::NotFound => Status::NotFound,
            AppError::BadRequest(_) => Status::BadRequest,
            AppError::Unavailable(_) => Status::ServiceUnavailable,
            AppError::TooManyRequests(_) => Status::TooManyRequests,
            AppError::Database(_) | AppError::Internal(_) => Status::InternalServerError,
        }
    }
}

impl<'r> Responder<'r, 'static> for AppError {
    fn respond_to(self, _req: &'r Request<'_>) -> response::Result<'static> {
        let status = self.status();
        // Log server-side faults with detail; the client gets a clean message.
        if status.code >= 500 {
            tracing::error!(error = %self, "request failed");
        }
        let body = json!({ "error": self.to_string() }).to_string();
        Response::build()
            .status(status)
            .header(ContentType::JSON)
            .sized_body(body.len(), Cursor::new(body))
            .ok()
    }
}

/// Convenience alias for route handlers.
pub type AppResult<T> = Result<T, AppError>;
