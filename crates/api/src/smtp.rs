use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use utoipa::ToSchema;

use crate::ApiState;
use crate::dto::ErrorDto;

/// One SMTP route.
#[derive(Debug, Serialize, ToSchema)]
pub struct SmtpRouteDto {
    /// An address or `*@domain`.
    pub recipient: String,
    pub archive_id: String,
    /// The folder of the archive's "Received by SMTP" account.
    pub folder: String,
    /// Exchange journal reports are unwrapped to the original message.
    pub journal: bool,
}

/// The SMTP listener, from the server's config.
#[derive(Debug, Serialize, ToSchema)]
pub struct SmtpDto {
    /// `false` when the config has no `[smtp]`; the other fields are then empty.
    pub enabled: bool,
    pub listen: Option<String>,
    pub hostname: Option<String>,
    pub allow: Vec<String>,
    pub starttls: bool,
    pub require_tls: bool,
    pub auth: bool,
    pub max_size_mb: Option<u64>,
    pub routes: Vec<SmtpRouteDto>,
    /// Counted since the server started.
    pub received: u64,
    pub deleted: u64,
    pub refused_recipients: u64,
    pub failed: u64,
    /// Unix milliseconds.
    pub last_received_at: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/api/smtp",
    responses((status = 200, body = SmtpDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto)),
)]
pub(crate) async fn status(State(state): State<Arc<ApiState>>) -> Json<SmtpDto> {
    let Some(smtp) = state.operations.smtp() else {
        return Json(SmtpDto {
            enabled: false,
            listen: None,
            hostname: None,
            allow: Vec::new(),
            starttls: false,
            require_tls: false,
            auth: false,
            max_size_mb: None,
            routes: Vec::new(),
            received: 0,
            deleted: 0,
            refused_recipients: 0,
            failed: 0,
            last_received_at: None,
        });
    };
    Json(SmtpDto {
        enabled: true,
        listen: Some(smtp.listen),
        hostname: Some(smtp.hostname),
        allow: smtp.allow,
        starttls: smtp.starttls,
        require_tls: smtp.require_tls,
        auth: smtp.auth,
        max_size_mb: Some(smtp.max_size_mb),
        routes: smtp
            .routes
            .into_iter()
            .map(|r| SmtpRouteDto {
                recipient: r.recipient,
                archive_id: r.archive_id.to_string(),
                folder: r.folder,
                journal: r.journal,
            })
            .collect(),
        received: smtp.received,
        deleted: smtp.deleted,
        refused_recipients: smtp.refused_recipients,
        failed: smtp.failed,
        last_received_at: smtp.last_received_at.map(|t| t.as_millis()),
    })
}
