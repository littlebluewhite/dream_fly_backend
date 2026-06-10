use sqlx::PgPool;

use super::model::ContactInquiry;

pub async fn create(
    db: &PgPool,
    name: &str,
    email: &str,
    phone: Option<&str>,
    subject: &str,
    message: &str,
) -> Result<ContactInquiry, sqlx::Error> {
    sqlx::query_as::<_, ContactInquiry>(
        "INSERT INTO contact_inquiries (id, name, email, phone, subject, message, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, now(), now()) \
         RETURNING id, name, email, phone, subject, message, status, assigned_to, created_at, updated_at",
    )
    .bind(name)
    .bind(email)
    .bind(phone)
    .bind(subject)
    .bind(message)
    .fetch_one(db)
    .await
}

pub async fn find_all(
    db: &PgPool,
    limit: u32,
    offset: u32,
) -> Result<Vec<ContactInquiry>, sqlx::Error> {
    sqlx::query_as::<_, ContactInquiry>(
        "SELECT id, name, email, phone, subject, message, status, assigned_to, created_at, updated_at \
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
