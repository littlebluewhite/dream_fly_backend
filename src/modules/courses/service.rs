use sqlx::PgPool;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;
use crate::utils::slug::slugify;

use super::dto::{CourseListResponse, CourseResponse, CreateCourseRequest, UpdateCourseRequest};
use super::model::CourseLevel;
use super::repository;

pub async fn list_courses(
    db: &PgPool,
    pagination: &PaginationParams,
) -> Result<CourseListResponse, AppError> {
    let total = repository::count_active(db).await?;
    let courses =
        repository::find_all_active(db, pagination.limit(), pagination.offset()).await?;
    Ok(CourseListResponse {
        courses: courses.into_iter().map(CourseResponse::from).collect(),
        total,
        page: pagination.page,
        per_page: pagination.limit(),
    })
}

pub async fn get_course_by_slug_or_id(
    db: &PgPool,
    param: &str,
) -> Result<CourseResponse, AppError> {
    let course = if let Ok(id) = param.parse::<uuid::Uuid>() {
        repository::find_by_id(db, id).await?
    } else {
        repository::find_by_slug(db, param).await?
    };

    course
        .map(CourseResponse::from)
        .ok_or_else(|| AppError::NotFound("course not found".into()))
}

pub async fn create_course(
    db: &PgPool,
    req: CreateCourseRequest,
) -> Result<CourseResponse, AppError> {
    let level: CourseLevel = req.level.to_lowercase().parse().map_err(|_| {
        AppError::Validation(
            "invalid course level, must be one of: beginner, intermediate, advanced".into(),
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

    let course = repository::create(
        db,
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
    )
    .await?;

    Ok(CourseResponse::from(course))
}

pub async fn update_course(
    db: &PgPool,
    id: uuid::Uuid,
    req: UpdateCourseRequest,
) -> Result<CourseResponse, AppError> {
    // Validate level if provided
    let level_str = if let Some(ref level) = req.level {
        let _: CourseLevel = level.parse().map_err(|_| {
            AppError::Validation(
                "invalid course level, must be one of: beginner, intermediate, advanced".into(),
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

    let course = repository::update(
        db,
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
    )
    .await?;

    course
        .map(CourseResponse::from)
        .ok_or_else(|| AppError::NotFound("course not found".into()))
}
