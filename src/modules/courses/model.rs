use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use strum::VariantArray;
use uuid::Uuid;

use crate::error::AppError;

#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ts_rs::TS, VariantArray,
)]
#[sqlx(type_name = "course_level", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum CourseLevel {
    Foundation,
    Beginner,
    Intermediate,
    Advanced,
    Elite,
}

impl CourseLevel {
    /// Every variant, in wire-spelling order — single owner of the value
    /// domain; `FromStr` and the allowed-values text both derive from this
    /// instead of hand-copying the list.
    pub const ALL: &[Self] = Self::VARIANTS;

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Foundation => "foundation",
            Self::Beginner => "beginner",
            Self::Intermediate => "intermediate",
            Self::Advanced => "advanced",
            Self::Elite => "elite",
        }
    }
}

impl std::str::FromStr for CourseLevel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // 既有 wire 政策：大小寫不敏感，先轉小寫再比對。
        let s = s.to_lowercase();
        Self::ALL
            .iter()
            .find(|v| v.as_str() == s)
            .cloned()
            .ok_or(())
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Course {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub level: CourseLevel,
    pub description: Option<String>,
    pub duration_minutes: i32,
    pub price_cents: i64,
    pub max_students: i32,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub features: Vec<String>,
    pub is_active: bool,
    pub coach_id: Option<Uuid>,
    pub category: Option<String>,
    pub schedule_text: Option<String>,
    pub is_highlighted: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Computed via a correlated subquery against `enrolments` (not a table
    /// column) — inlined at each query site in `repository.rs`; the COUNT
    /// predicate's owner is `courses::seats` (see that module's doc).
    pub enrolled_count: i64,
    /// Computed via a correlated subquery against `waitlist_entries` (not a
    /// table column) — inlined at each query site in `repository.rs`.
    pub waitlist_count: i64,
}

impl Course {
    /// 課程行的合法數量：一堂課只能買一份，否則 `Validation` (422)
    /// `"course quantity must be 1"`。不需要課程列（cart 在查課程前就先擋，
    /// 讓 422 排在查無課程的 404 之前）。
    ///
    /// 年齡規則若要在加入購物車／候補時檢查，也歸這裡（與
    /// [`Self::ensure_purchasable`] 同一個 owner）。
    pub fn ensure_line_quantity(quantity: i32) -> Result<(), AppError> {
        if quantity != 1 {
            return Err(AppError::Validation("course quantity must be 1".into()));
        }
        Ok(())
    }

    /// 課程現在能不能買／排候補：下架則 `BadRequest` (400)
    /// `"course is not available"`。cart 加入與 waitlist 加入共用；結帳的
    /// 下架 gate（422、整批）與公開瀏覽的 404 是刻意不同的分流，不走這裡
    /// （CONTEXT「上架可見性」）。
    pub fn ensure_purchasable(&self) -> Result<(), AppError> {
        if !self.is_active {
            return Err(AppError::BadRequest("course is not available".into()));
        }
        Ok(())
    }
}

/// A course's structured weekly meeting pattern — one row per (day_of_week,
/// start_time). Mirrors `coach_schedules`' shape. `day_of_week` is 0=Sunday
/// .. 6=Saturday (PostgreSQL `EXTRACT(DOW)` convention — see
/// `sessions::calendar::materialize_range`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct CourseScheduleSlot {
    pub id: Uuid,
    pub course_id: Uuid,
    pub day_of_week: i16,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub venue: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// The two columns `update_course`'s locking pre-read consumes — fetched
/// `FOR UPDATE` by `repository::find_age_bounds_for_update_tx` instead of
/// duplicating the full `Course` projection (and its two correlated COUNT
/// subqueries) on every PATCH.
#[derive(Debug, sqlx::FromRow)]
pub struct CourseAgeBounds {
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
}

/// Owner of a course's "legal age range" invariant: each bound, if present,
/// falls within `0..=150`, and if both are present `min_age <= max_age`. The
/// DB `courses_age_range` CHECK constraint enforces the ordering half as a
/// backstop, but does not know about the 0..=150 bounds — this type is the
/// only place both halves are validated before a write.
#[derive(Debug, Clone, Copy)]
pub struct AgeRange {
    min_age: Option<i32>,
    max_age: Option<i32>,
}

impl AgeRange {
    pub fn new(min_age: Option<i32>, max_age: Option<i32>) -> Result<Self, AppError> {
        if let Some(min) = min_age {
            if !(0..=150).contains(&min) {
                return Err(AppError::Validation(
                    "min_age must be between 0 and 150".into(),
                ));
            }
        }
        if let Some(max) = max_age {
            if !(0..=150).contains(&max) {
                return Err(AppError::Validation(
                    "max_age must be between 0 and 150".into(),
                ));
            }
        }
        if let (Some(min), Some(max)) = (min_age, max_age) {
            if min > max {
                return Err(AppError::Validation(
                    "min_age must be less than or equal to max_age".into(),
                ));
            }
        }
        Ok(Self { min_age, max_age })
    }

    pub fn min_age(&self) -> Option<i32> {
        self.min_age
    }

    pub fn max_age(&self) -> Option<i32> {
        self.max_age
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Boundary table: both-Some legal/equal/reversed, one-sided `None`,
    /// both-`None`, inclusive edges (0/150), and out-of-range (-1/151).
    #[test]
    fn age_range_new_boundary_table() {
        let cases: &[(Option<i32>, Option<i32>, bool)] = &[
            (Some(3), Some(10), true),
            (Some(5), Some(5), true),
            (Some(10), Some(3), false),
            (Some(5), None, true),
            (None, Some(10), true),
            (None, None, true),
            (Some(0), Some(150), true),
            (Some(-1), None, false),
            (None, Some(151), false),
            (Some(151), None, false),
            (None, Some(-1), false),
        ];

        for (min, max, should_succeed) in cases.iter().copied() {
            let result = AgeRange::new(min, max);
            assert_eq!(
                result.is_ok(),
                should_succeed,
                "min={min:?} max={max:?} expected ok={should_succeed}, got {result:?}"
            );
        }
    }

    #[test]
    fn age_range_new_rejects_reversed_range_with_exact_message() {
        let err = AgeRange::new(Some(10), Some(3)).unwrap_err();
        match err {
            AppError::Validation(msg) => {
                assert_eq!(msg, "min_age must be less than or equal to max_age")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn age_range_new_rejects_min_age_below_zero_with_exact_message() {
        let err = AgeRange::new(Some(-1), None).unwrap_err();
        match err {
            AppError::Validation(msg) => assert_eq!(msg, "min_age must be between 0 and 150"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn age_range_new_rejects_max_age_above_150_with_exact_message() {
        let err = AgeRange::new(None, Some(151)).unwrap_err();
        match err {
            AppError::Validation(msg) => assert_eq!(msg, "max_age must be between 0 and 150"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }
    // --- ensure_line_quantity ---

    #[test]
    fn course_line_quantity_of_one_is_ok() {
        assert!(Course::ensure_line_quantity(1).is_ok());
    }

    #[test]
    fn course_line_quantity_other_than_one_is_validation_error() {
        for qty in [i32::MIN, -1, 0, 2, 999, 1000, i32::MAX] {
            let err = Course::ensure_line_quantity(qty).expect_err("must reject");
            assert!(
                matches!(err, AppError::Validation(ref m) if m == "course quantity must be 1"),
                "got: {err:?} for qty={qty}"
            );
        }
    }
}
