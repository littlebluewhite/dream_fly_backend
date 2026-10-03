use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type, ts_rs::TS)]
#[sqlx(type_name = "waitlist_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum WaitlistStatus {
    Waiting,
    Cancelled,
}

impl WaitlistStatus {
    /// Every variant, in declaration (= PG label) order — single owner of the value
    /// domain.
    pub const ALL: [Self; 2] = [Self::Waiting, Self::Cancelled];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Bare `waitlist_entries` table row.
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct WaitlistEntry {
    pub id: Uuid,
    pub user_id: Uuid,
    pub course_id: Uuid,
    pub status: WaitlistStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The waitlist read projection's one row shape — a `waitlist_entries` row
/// JOINed with its course's `name`. Field names mirror `WaitlistResponse`
/// 1:1 (see `dto.rs`). Single owner of the projection's column list:
/// `repository::VIEW_COLUMNS`/`VIEW_JOINS` assemble it for every read
/// (`find_by_user_with_course` over the table, `find_by_course_waiting` over
/// the `waiting_entries` view) and for `insert`, whose data-modifying CTE
/// re-joins its own `RETURNING` through the same two consts — the join
/// response is this same row, not a hand-copied echo of it.
/// `cancel_if_waiting_tx` changes the row but returns the bare
/// [`WaitlistEntry`] — `DELETE` has no response body to project.
#[derive(Debug, sqlx::FromRow)]
pub struct WaitlistEntryWithCourse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub status: WaitlistStatus,
    pub created_at: DateTime<Utc>,
}
