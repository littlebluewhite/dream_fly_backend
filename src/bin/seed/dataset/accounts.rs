//! 帳號誕生 (Account Birth), seed side: `upsert_account` is the seed's one
//! `INSERT INTO users` — admin, test member, coaches and reporting members
//! are all born through it. Its rules:
//! - idempotent on `email`; a new account is born at its `joined_at`, a
//!   historical instant, never the run instant;
//! - opening points are one `LedgerDelta::admin_adjust` through
//!   `points::service::apply_delta_tx`, in the INSERT's transaction and only
//!   for a new row with `points > 0`; 0 points write no ledger row, and an
//!   existing row is never granted again, whatever the app has done to its
//!   balance since;
//! - accounts sharing a dev plaintext share one password hash: the caller
//!   hashes each plaintext once;
//! - not born through `auth::provisioning::create_account`, which would queue
//!   a `user_registered` outbox event on every rerun (that module's doc says
//!   why it leaves the seed out);
//! - roles stay with `run`'s `assign_role`.

use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::points::model::LedgerDelta;
use dream_fly_backend::modules::points::service as points_service;

/// One seed account as it is born: who, its dev password's hash, its
/// optional birth date (backs the age-bracket report bucket), its opening
/// points and `joined_at`, the historical instant it joined at — not the
/// run instant, so it predates every seeded row that references it.
pub(super) struct SeedAccount<'a> {
    pub(super) email: &'a str,
    pub(super) name: &'a str,
    pub(super) password_hash: &'a str,
    pub(super) birth_date: Option<NaiveDate>,
    pub(super) points: i64,
    pub(super) joined_at: DateTime<Utc>,
}

/// Insert `a` (idempotent on `email`) and return its id whether the row was
/// just inserted or already existed. Takes an already-hashed password so
/// callers sharing a plaintext share one Argon2 hash. The INSERT always
/// writes `points_balance = 0`; a newly inserted row with `points > 0` is
/// then locked (`points_service::lock_balance_tx`) and granted its opening
/// points as one `LedgerDelta::admin_adjust` at `now`, in the same
/// transaction as the INSERT (a failed grant rolls the insert back too). An
/// existing row is never granted again, whatever its balance is now, so a
/// rerun is a no-op. Roles are the caller's (`run`'s `assign_role`).
pub(super) async fn upsert_account(
    db: &PgPool,
    a: &SeedAccount<'_>,
    now: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let email = a.email;
    let mut tx = db
        .begin()
        .await
        .with_context(|| format!("begin tx for user {email}"))?;

    let inserted: Option<Uuid> = sqlx::query_scalar(
        r#"
        INSERT INTO users (id, email, name, password_hash, phone_verified, is_active, points_balance, birth_date, created_at, updated_at)
        VALUES ($1, $2, $3, $4, false, true, 0, $5, $6, NOW())
        ON CONFLICT DO NOTHING
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(a.name)
    .bind(a.password_hash)
    .bind(a.birth_date)
    .bind(a.joined_at)
    .fetch_optional(&mut *tx)
    .await
    .with_context(|| format!("insert user {email}"))?;

    let user_id = match inserted {
        Some(id) => {
            if a.points > 0 {
                let lock = points_service::lock_balance_tx(&mut tx, id)
                    .await
                    .with_context(|| format!("lock points balance of user {email}"))?;
                points_service::apply_delta_tx(
                    &mut tx,
                    &lock,
                    LedgerDelta::admin_adjust(a.points),
                    now,
                )
                .await
                .with_context(|| format!("grant seed points to user {email}"))?;
            }
            id
        }
        None => sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
            .bind(email)
            .fetch_one(&mut *tx)
            .await
            .with_context(|| format!("fetch id for user {email}"))?,
    };

    tx.commit()
        .await
        .with_context(|| format!("commit tx for user {email}"))?;

    Ok(user_id)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, NaiveDate, TimeZone, Utc};
    use sqlx::PgPool;
    use uuid::Uuid;

    use dream_fly_backend::modules::points::model::LedgerDelta;
    use dream_fly_backend::modules::points::service::{apply_delta_tx, lock_balance_tx};

    use super::{SeedAccount, upsert_account};

    /// The run instant every test here grants at: 2026-03-10T03:00Z.
    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap()
    }

    /// The historical join instant every account here is born at.
    fn joined_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 3, 1, 4, 0, 0).unwrap()
    }

    /// The smallest account set: one account with `points` opening points.
    fn account(points: i64, birth_date: Option<NaiveDate>) -> SeedAccount<'static> {
        SeedAccount {
            email: "someone@dreamfly.tw",
            name: "測試帳號",
            password_hash: "not-a-real-hash",
            birth_date,
            points,
            joined_at: joined_at(),
        }
    }

    struct Row {
        created_at: DateTime<Utc>,
        birth_date: Option<NaiveDate>,
        points_balance: i64,
    }

    async fn user_row(db: &PgPool, id: Uuid) -> Row {
        let (created_at, birth_date, points_balance) = sqlx::query_as(
            "SELECT created_at, birth_date, points_balance::BIGINT FROM users WHERE id = $1",
        )
        .bind(id)
        .fetch_one(db)
        .await
        .expect("fetch user row");
        Row {
            created_at,
            birth_date,
            points_balance,
        }
    }

    /// `(reason, delta, created_at)` of every ledger row of `id`, oldest first.
    async fn ledger(db: &PgPool, id: Uuid) -> Vec<(String, i64, DateTime<Utc>)> {
        sqlx::query_as(
            "SELECT reason::TEXT, delta::BIGINT, created_at FROM point_ledger \
             WHERE user_id = $1 ORDER BY created_at, id",
        )
        .bind(id)
        .fetch_all(db)
        .await
        .expect("fetch ledger")
    }

    /// A new account is born at its `joined_at`, not the run instant, with
    /// the birth date it was given.
    #[sqlx::test]
    async fn new_account_is_born_at_joined_at(db: PgPool) {
        let birth = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap();
        let id = upsert_account(&db, &account(0, Some(birth)), at())
            .await
            .expect("upsert account");

        let row = user_row(&db, id).await;
        assert_eq!(row.created_at, joined_at());
        assert_eq!(row.birth_date, Some(birth));
    }

    /// Opening points land as exactly one `admin_adjust` row at the run
    /// instant, and the balance matches it.
    #[sqlx::test]
    async fn opening_points_write_one_admin_adjust(db: PgPool) {
        let id = upsert_account(&db, &account(1250, None), at())
            .await
            .expect("upsert account");

        assert_eq!(
            ledger(&db, id).await,
            vec![("admin_adjust".to_owned(), 1250, at())]
        );
        assert_eq!(user_row(&db, id).await.points_balance, 1250);
    }

    /// An account opened with 0 points writes no ledger row.
    #[sqlx::test]
    async fn zero_points_write_no_ledger(db: PgPool) {
        let id = upsert_account(&db, &account(0, None), at())
            .await
            .expect("upsert account");

        assert!(ledger(&db, id).await.is_empty());
        assert_eq!(user_row(&db, id).await.points_balance, 0);
    }

    /// Drift: once the app has moved the balance, a rerun returns the same
    /// account and grants nothing — the opening grant is for a new row only.
    #[sqlx::test]
    async fn rerun_after_the_app_moved_the_balance_grants_nothing(db: PgPool) {
        let id = upsert_account(&db, &account(1250, None), at())
            .await
            .expect("first upsert");

        let mut tx = db.begin().await.expect("begin redeem tx");
        let lock = lock_balance_tx(&mut tx, id).await.expect("lock balance");
        apply_delta_tx(&mut tx, &lock, LedgerDelta::redeem(250), at())
            .await
            .expect("redeem");
        tx.commit().await.expect("commit redeem tx");

        let again = upsert_account(&db, &account(1250, None), at())
            .await
            .expect("rerun upsert");

        assert_eq!(again, id);
        assert_eq!(ledger(&db, id).await.len(), 2);
        let row = user_row(&db, id).await;
        assert_eq!(row.points_balance, 1000);
        assert_eq!(row.created_at, joined_at());
    }
}
