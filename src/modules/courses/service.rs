use sqlx::PgPool;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;
use crate::utils::slug::slugify;
use crate::utils::studio_clock;

use super::dto::{
    CourseDetailResponse, CourseListResponse, CourseResponse, CourseScheduleSlotEntry,
    CourseScheduleSlotResponse, CreateCourseRequest, UpdateCourseRequest,
};
use super::model::CourseLevel;
use super::repository::{self, CourseSlotRow};

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
            if end <= start {
                return Err(AppError::Validation(
                    "schedule_slots end_time must be after start_time".into(),
                ));
            }
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

    // Cross-field validation: min_age must be <= max_age when both are set.
    if let (Some(min), Some(max)) = (req.min_age, req.max_age) {
        if min > max {
            return Err(AppError::Validation(
                "min_age must be less than or equal to max_age".into(),
            ));
        }
    }

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
        &req.name,
        &slug,
        &level,
        req.description.as_deref(),
        req.duration_minutes,
        req.price_cents,
        req.max_students,
        req.min_age,
        req.max_age,
        &features,
        req.coach_id,
        req.category.as_deref(),
        req.schedule_text.as_deref(),
        req.is_highlighted,
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

    let course = repository::update(
        &mut tx,
        id,
        req.name.as_deref(),
        req.slug.as_deref(),
        level_str.as_deref(),
        req.description.as_deref(),
        req.duration_minutes,
        req.price_cents,
        req.max_students,
        req.min_age,
        req.max_age,
        req.features.as_deref(),
        req.coach_id,
        req.category.as_ref().map(|o| o.as_deref()),
        req.schedule_text.as_ref().map(|o| o.as_deref()),
        req.is_highlighted,
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
