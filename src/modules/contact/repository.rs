use sqlx::PgPool;
use uuid::Uuid;

use super::model::ContactInquiry;

#[allow(clippy::too_many_arguments)]
pub async fn create(
    db: &PgPool,
    name: &str,
    email: &str,
    phone: Option<&str>,
    subject: &str,
    message: &str,
    inquiry_type: &str,
    metadata: Option<serde_json::Value>,
) -> Result<ContactInquiry, sqlx::Error> {
    sqlx::query_as::<_, ContactInquiry>(
        "INSERT INTO contact_inquiries (id, name, email, phone, subject, message, inquiry_type, metadata, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, now(), now()) \
         RETURNING id, name, email, phone, subject, message, status, assigned_to, inquiry_type, metadata, created_at, updated_at",
    )
    .bind(name)
    .bind(email)
    .bind(phone)
    .bind(subject)
    .bind(message)
    .bind(inquiry_type)
    .bind(metadata)
    .fetch_one(db)
    .await
}

pub async fn find_all(
    db: &PgPool,
    limit: u32,
    offset: u32,
) -> Result<Vec<ContactInquiry>, sqlx::Error> {
    sqlx::query_as::<_, ContactInquiry>(
        "SELECT id, name, email, phone, subject, message, status, assigned_to, inquiry_type, metadata, created_at, updated_at \
         FROM contact_inquiries \
         ORDER BY created_at DESC \
         LIMIT $1 OFFSET $2",
    )
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

pub async fn count_all(db: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM contact_inquiries")
        .fetch_one(db)
        .await
}

/// Partial (PATCH-style) update for admin follow-up (Round 4 Task B5) —
/// every argument optional. `status` is passed as the already-validated
/// canonical string (see `service::update_inquiry`) and cast to the
/// `inquiry_status` enum in SQL — mirrors `courses::repository::update`'s
/// `level` column. `assigned_to` is `Option<Option<Uuid>>` so callers can
/// distinguish "don't touch" (`None`), "unassign" (`Some(None)`), and
/// "assign" (`Some(Some(id))`) — mirrors `venues::repository::update`
/// (template: d91ad85). A non-existent `assigned_to` user id is not
/// pre-checked here; it is bound as-is and, if it doesn't reference an
/// existing `users.id`, surfaces as a generic 500 via the FK constraint —
/// same rigor level as `courses::coach_id` / `venues::category_id` (see task
/// report for the reasoning). Returns `Ok(None)` if `id` doesn't match any
/// row (caller maps to 404).
pub async fn update(
    db: &PgPool,
    id: Uuid,
    status: Option<&str>,
    assigned_to: Option<Option<Uuid>>,
) -> Result<Option<ContactInquiry>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "UPDATE contact_inquiries SET updated_at = now()",
    );

    if let Some(v) = status {
        qb.push(", status = ").push_bind(v).push("::inquiry_status");
    }
    if let Some(v) = assigned_to {
        qb.push(", assigned_to = ").push_bind(v);
    }

    qb.push(" WHERE id = ").push_bind(id);
    qb.push(
        " RETURNING id, name, email, phone, subject, message, status, assigned_to, inquiry_type, metadata, created_at, updated_at",
    );

    qb.build_query_as::<ContactInquiry>()
        .fetch_optional(db)
        .await
}
