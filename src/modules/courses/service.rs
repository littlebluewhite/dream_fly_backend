use sqlx::PgPool;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;
use crate::utils::slug::slugify;
use crate::utils::studio_clock;

use super::dto::{
    CourseDetailResponse, CourseListResponse, CourseResponse, CourseScheduleSlotEntry,
    CourseScheduleSlotResponse, CreateCourseRequest, UpdateCourseRequest,
};
use super::model::{AgeRange, CourseLevel};
use super::repository::{self, CourseCreate, CourseSlotRow, CourseUpdate};

/// Parse+validate `schedule_slots` request entries into the tuple shape
/// `repository::replace_slots_tx` takes. `AppError::Validation`
/// (422) on an unparseable time or `end_time <= start_time` — the per-field
/// bounds (day_of_week 0-6, string length) are already enforced by
/// `ValidatedJson` via `CourseScheduleSlotEntry`'s own `Validate` derive
/// before the service layer ever sees this.
fn parse_schedule_slots(
    entries: &[CourseScheduleSlotEntry],
) -> Result<Vec<CourseSlotRow>, AppError> {
    entries
        .iter()
        .map(|e| {
            let start = studio_clock::parse_time_of_day(&e.start_time).ok_or_else(|| {
                AppError::Validation(format!("invalid start_time: {}", e.start_time))
            })?;
            let end = studio_clock::parse_time_of_day(&e.end_time).ok_or_else(|| {
                AppError::Validation(format!("invalid end_time: {}", e.end_time))
            })?;
            studio_clock::validate_time_window(start, end)?;
            Ok((e.day_of_week, start, end, e.venue.clone()))
        })
        .collect()
}

async fn slots_response(db: &PgPool, course_id: uuid::Uuid) -> Result<Vec<CourseScheduleSlotResponse>, AppError> {
    let slots = repository::find_slots_by_course(db, course_id).await?;
    Ok(slots.into_iter().map(CourseScheduleSlotResponse::from).collect())
}

pub async fn list_courses(
    db: &PgPool,
    pagination: &PaginationParams,
) -> Result<CourseListResponse, AppError> {
    let total = repository::count_active(db).await?;
    let courses =
        repository::find_all_active(db, pagination.limit(), pagination.offset()).await?;
    Ok(CourseListResponse {
        courses: courses.into_iter().map(CourseResponse::from).collect(),
        meta: pagination.meta(total),
    })
}

pub async fn get_course_by_slug_or_id(
    db: &PgPool,
    param: &str,
) -> Result<CourseDetailResponse, AppError> {
    let course = if let Ok(id) = param.parse::<uuid::Uuid>() {
        repository::find_by_id(db, id).await?
    } else {
        repository::find_by_slug(db, param).await?
    }
    .ok_or_else(|| AppError::NotFound("course not found".into()))?;

    let schedule_slots = slots_response(db, course.id).await?;
    Ok(CourseDetailResponse {
        course: CourseResponse::from(course),
        schedule_slots,
    })
}

pub async fn create_course(
    db: &PgPool,
    req: CreateCourseRequest,
) -> Result<CourseDetailResponse, AppError> {
    let level: CourseLevel = req.level.to_lowercase().parse().map_err(|_| {
        AppError::Validation(
            "invalid course level, must be one of: foundation, beginner, intermediate, advanced, elite".into(),
        )
    })?;

    // AgeRange owns the "legal age range" invariant (ordering + 0..=150
    // bounds). The bounds half is already enforced by
    // `CreateCourseRequest`'s own `#[validate(range)]` attributes, so this
    // re-check is idempotent on the create path.
    let age_range = AgeRange::new(req.min_age, req.max_age)?;

    let slug = req.slug.unwrap_or_else(|| slugify(&req.name));

    // Check slug uniqueness
    if repository::find_by_slug(db, &slug).await?.is_some() {
        return Err(AppError::Conflict("course slug already exists".into()));
    }

    let features = req.features.unwrap_or_default();

    // Parse before opening the transaction so a bad time string 422s without
    // ever touching the DB.
    let parsed_slots = req
        .schedule_slots
        .as_ref()
        .map(|entries| parse_schedule_slots(entries))
        .transpose()?;

    let mut tx = db.begin().await?;

    let course = repository::create(
        &mut tx,
        CourseCreate {
            name: &req.name,
            slug: &slug,
            level: &level,
            description: req.description.as_deref(),
            duration_minutes: req.duration_minutes,
            price_cents: req.price_cents,
            max_students: req.max_students,
            min_age: age_range.min_age(),
            max_age: age_range.max_age(),
            features: &features,
            coach_id: req.coach_id,
            category: req.category.as_deref(),
            schedule_text: req.schedule_text.as_deref(),
            is_highlighted: req.is_highlighted,
        },
    )
    .await?;

    if let Some(slots) = &parsed_slots {
        repository::replace_slots_tx(&mut tx, course.id, slots).await?;
    }

    tx.commit().await?;

    let schedule_slots = slots_response(db, course.id).await?;
    Ok(CourseDetailResponse {
        course: CourseResponse::from(course),
        schedule_slots,
    })
}

pub async fn update_course(
    db: &PgPool,
    id: uuid::Uuid,
    req: UpdateCourseRequest,
) -> Result<CourseDetailResponse, AppError> {
    // Validate level if provided
    let level_str = if let Some(ref level) = req.level {
        let _: CourseLevel = level.parse().map_err(|_| {
            AppError::Validation(
                "invalid course level, must be one of: foundation, beginner, intermediate, advanced, elite".into(),
            )
        })?;
        Some(level.to_lowercase())
    } else {
        None
    };

    // Check slug uniqueness if changing
    if let Some(ref new_slug) = req.slug {
        if let Some(existing) = repository::find_by_slug(db, new_slug).await? {
            if existing.id != id {
                return Err(AppError::Conflict("course slug already exists".into()));
            }
        }
    }

    let parsed_slots = req
        .schedule_slots
        .as_ref()
        .map(|entries| parse_schedule_slots(entries))
        .transpose()?;

    let mut tx = db.begin().await?;

    // Lock pre-read to avoid two concurrent PATCHes validating against the
    // same stale row — full rationale on `find_age_bounds_for_update_tx`.
    let existing = repository::find_age_bounds_for_update_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::NotFound("course not found".into()))?;

    // Merge the tri-state PATCH into its effective post-write value:
    // `None` (absent) keeps the existing bound, `Some(v)` (including
    // `Some(None)` — explicit null) overrides it.
    let effective_min_age = req.min_age.unwrap_or(existing.min_age);
    let effective_max_age = req.max_age.unwrap_or(existing.max_age);
    AgeRange::new(effective_min_age, effective_max_age)?;

    let course = repository::update(
        &mut *tx,
        id,
        CourseUpdate {
            name: req.name.as_deref(),
            slug: req.slug.as_deref(),
            level: level_str.as_deref(),
            description: req.description.as_deref(),
            duration_minutes: req.duration_minutes,
            price_cents: req.price_cents,
            max_students: req.max_students,
            min_age: req.min_age,
            max_age: req.max_age,
            features: req.features.as_deref(),
            coach_id: req.coach_id,
            category: req.category.as_ref().map(|o| o.as_deref()),
            schedule_text: req.schedule_text.as_ref().map(|o| o.as_deref()),
            is_highlighted: req.is_highlighted,
        },
    )
    .await?
    .ok_or_else(|| AppError::NotFound("course not found".into()))?;

    if let Some(slots) = &parsed_slots {
        repository::replace_slots_tx(&mut tx, course.id, slots).await?;
    }

    tx.commit().await?;

    let schedule_slots = slots_response(db, course.id).await?;
    Ok(CourseDetailResponse {
        course: CourseResponse::from(course),
        schedule_slots,
    })
}
