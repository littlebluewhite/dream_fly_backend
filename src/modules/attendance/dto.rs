use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::{AttendanceStatus, MyStudentRow, RosterRow, StudentCourseBrief};

// ---------------------------------------------------------------------------
// GET /sessions/{id}/roster, PUT /sessions/{id}/attendance
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct RosterEntryResponse {
    pub enrolment_id: Uuid,
    pub user_id: Uuid,
    pub user_name: String,
    pub attendance_status: Option<AttendanceStatus>,
}

impl From<RosterRow> for RosterEntryResponse {
    fn from(r: RosterRow) -> Self {
        Self {
            enrolment_id: r.enrolment_id,
            user_id: r.user_id,
            user_name: r.user_name,
            attendance_status: r.attendance_status,
        }
    }
}

/// One entry of a `PUT /sessions/{id}/attendance` body. `status` is a raw
/// string (parsed to `AttendanceStatus` in `service`, mirroring
/// `orders::dto::UpdateOrderStatusRequest`'s string-then-`FromStr` pattern)
/// rather than a directly-deserialized enum, so an invalid value fails
/// validation with the module's own 422 message instead of a generic
/// "invalid JSON body" rejection.
#[derive(Debug, Deserialize, Validate)]
pub struct AttendanceRecordEntry {
    pub enrolment_id: Uuid,
    #[validate(length(min = 1, max = 32))]
    pub status: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct BulkUpsertAttendanceRequest {
    #[validate(nested)]
    pub records: Vec<AttendanceRecordEntry>,
}

// ---------------------------------------------------------------------------
// GET /coaches/me/students
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct MyStudentResponse {
    pub user_id: Uuid,
    pub name: String,
    pub phone: Option<String>,
    pub courses: Vec<StudentCourseBrief>,
}

impl From<MyStudentRow> for MyStudentResponse {
    fn from(r: MyStudentRow) -> Self {
        Self {
            user_id: r.user_id,
            name: r.name,
            phone: r.phone,
            courses: r.courses.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::attendance::model::AttendanceStatus;

    fn row(attendance_status: Option<AttendanceStatus>) -> RosterRow {
        RosterRow {
            enrolment_id: Uuid::nil(),
            user_id: Uuid::nil(),
            user_name: "u".into(),
            attendance_status,
        }
    }

    /// Golden wire shape: an unmarked roster entry carries an explicit `null`.
    #[test]
    fn roster_entry_json_is_golden() {
        assert_eq!(
            serde_json::to_string(&RosterEntryResponse::from(row(None))).unwrap(),
            r#"{"enrolment_id":"00000000-0000-0000-0000-000000000000","user_id":"00000000-0000-0000-0000-000000000000","user_name":"u","attendance_status":null}"#
        );
        assert_eq!(
            serde_json::to_string(&RosterEntryResponse::from(row(Some(AttendanceStatus::Present))))
                .unwrap(),
            r#"{"enrolment_id":"00000000-0000-0000-0000-000000000000","user_id":"00000000-0000-0000-0000-000000000000","user_name":"u","attendance_status":"present"}"#
        );
    }
}
