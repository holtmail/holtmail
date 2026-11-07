use std::sync::{Arc, RwLock};

use axum::Json;
use axum::extract::State;
use holtmail_core::Timestamp;
use serde::Serialize;
use utoipa::ToSchema;

use crate::dto::ErrorDto;
use crate::{ApiResult, ApiState};

/// Where a loaded category data file came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataOrigin {
    /// Compiled into the binary.
    Bundled,
    /// Downloaded, verified and stored in the data directory.
    Downloaded,
}

/// One category data table the server votes with.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LoadedDataFile {
    /// The table name, `wikidata` or `ut1`.
    pub name: String,
    /// The published file name, such as `wikidata-categories.tsv.gz`.
    pub file: String,
    /// The table's version from its header.
    pub version: String,
    /// The rows kept on load.
    pub rows: u64,
    /// The SPDX identifier of the data licence.
    pub license: String,
    /// Where the table came from.
    pub origin: DataOrigin,
    /// The attribution the licence asks for, when it asks for one.
    pub attribution: Option<String>,
}

/// The outcome of the last check for new category data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataOutcome {
    /// A new release was verified, stored and loaded.
    Updated,
    /// The published release is the one loaded.
    NotModified,
    /// The release failed verification; nothing changed.
    Refused,
    /// The release could not be downloaded or stored; nothing changed.
    Failed,
}

/// Whether and how the server looks for new category data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct DataUpdates {
    /// The server checks for new releases on a schedule.
    pub enabled: bool,
    /// The downloaded tables the server uses, by table name.
    pub data_sources: Vec<String>,
    /// When the last check finished.
    pub last_checked_at: Option<Timestamp>,
    /// What the last check found.
    pub last_outcome: Option<DataOutcome>,
}

/// The loaded category data and the update state, as one value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct DataSnapshot {
    /// The loaded tables, sorted by name.
    pub files: Vec<LoadedDataFile>,
    /// The update state.
    pub updates: DataUpdates,
}

/// The category data the server has loaded, shared with the server that loads it. Safe to share
/// between threads; [`DataStatus::get`] always returns a whole value written by one
/// [`DataStatus::set`], never a mix of two.
#[derive(Debug, Default)]
pub struct DataStatus(RwLock<DataSnapshot>);

impl DataStatus {
    /// Replaces the whole value.
    pub fn set(&self, snapshot: DataSnapshot) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = snapshot;
    }

    /// The last value set, or an empty one with updates disabled before the first `set`.
    pub fn get(&self) -> DataSnapshot {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Where a data file came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DataOriginDto {
    /// Compiled into the server.
    Bundled,
    /// Downloaded and verified by the server.
    Downloaded,
}

/// The outcome of the last check for new category data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DataOutcomeDto {
    /// A new release was verified and loaded.
    Updated,
    /// The published release is the one loaded.
    NotModified,
    /// The release failed verification; nothing changed.
    Refused,
    /// The release could not be downloaded or stored; nothing changed.
    Failed,
}

/// One category data table the server uses.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataFileDto {
    /// The table name: `wikidata` or `ut1`.
    pub name: String,
    /// The published file name.
    pub file: String,
    /// The table's version.
    pub version: String,
    /// The rows loaded.
    pub rows: u64,
    /// The SPDX identifier of the data licence.
    pub license: String,
    pub origin: DataOriginDto,
    /// The attribution to show with the data, when its licence asks for one.
    pub attribution: Option<String>,
}

/// Whether the server looks for new category data.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataUpdatesDto {
    /// `[insights.categories] update`, and the server runs the check.
    pub enabled: bool,
    /// The downloaded tables the server uses, by name.
    pub data_sources: Vec<String>,
    /// Unix milliseconds of the last check; `null` before the first.
    pub last_checked_at: Option<i64>,
    /// What the last check found; `null` before the first.
    pub last_outcome: Option<DataOutcomeDto>,
}

/// The server's version and the category data it uses, for Settings → About.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InsightsDataDto {
    /// The server's version.
    pub holtmail_version: String,
    /// The loaded tables, sorted by name. Empty when insights or the domain categorizer are off.
    pub files: Vec<DataFileDto>,
    pub updates: DataUpdatesDto,
}

impl From<LoadedDataFile> for DataFileDto {
    fn from(file: LoadedDataFile) -> Self {
        Self {
            name: file.name,
            file: file.file,
            version: file.version,
            rows: file.rows,
            license: file.license,
            origin: match file.origin {
                DataOrigin::Bundled => DataOriginDto::Bundled,
                DataOrigin::Downloaded => DataOriginDto::Downloaded,
            },
            attribution: file.attribution,
        }
    }
}

impl From<DataOutcome> for DataOutcomeDto {
    fn from(outcome: DataOutcome) -> Self {
        match outcome {
            DataOutcome::Updated => Self::Updated,
            DataOutcome::NotModified => Self::NotModified,
            DataOutcome::Refused => Self::Refused,
            DataOutcome::Failed => Self::Failed,
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/insights/data",
    description = "The server's version and the category data files it uses. Every signed-in \
                   user may read it. With insights off it answers 200 with no files and updates \
                   disabled, so the About page still shows the version.",
    responses(
        (status = 200, body = InsightsDataDto),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn insights_data(
    State(state): State<Arc<ApiState>>,
) -> ApiResult<Json<InsightsDataDto>> {
    let snapshot = state
        .insights
        .as_ref()
        .map(|config| config.data.get())
        .unwrap_or_default();
    Ok(Json(InsightsDataDto {
        holtmail_version: env!("CARGO_PKG_VERSION").to_owned(),
        files: snapshot.files.into_iter().map(Into::into).collect(),
        updates: DataUpdatesDto {
            enabled: snapshot.updates.enabled,
            data_sources: snapshot.updates.data_sources,
            last_checked_at: snapshot.updates.last_checked_at.map(|t| t.as_millis()),
            last_outcome: snapshot.updates.last_outcome.map(Into::into),
        },
    }))
}
