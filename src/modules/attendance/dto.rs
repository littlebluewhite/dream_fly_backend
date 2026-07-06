use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::{MyStudentRow, RosterRow, StudentCourseBrief};

// ---------------------------------------------------------------------------
// GET /sessions/{id}/roster, PUT /sessions/{id}/attendance
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RosterEntryResponse {
    pub enrolment_id: Uuid,
    pub user_id: Uuid,
    pub user_name: String,
    pub attendance_status: Option<String>,
}

impl From<RosterRow> for RosterEntryResponse {
    fn from(r: RosterRow) -> Self {
        Self {
            enrolment_id: r.enrolment_id,
            user_id: r.user_id,
            user_name: r.user_name,
            attendance_status: r.attendance_status.map(|s| s.as_str().to_string()),
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

#[derive(Debug, Serialize)]
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
