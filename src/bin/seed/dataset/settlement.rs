//! 點數級距 (Points Tier) settlement, seed side: `settle_members` settles
//! each reporting member to its points-tier target (`member_points_target`)
//! with one `admin_adjust(target − balance)`, every run, after the purchase
//! timeline has written the members' `checkout_earn`/`refund_clawback`
//! rows. The rules are in the GLOSSARY entries 「點數級距」 and 「給點」.

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::points::model::{LedgerDelta, PointsTier};
use dream_fly_backend::modules::points::service as points_service;

/// Points-tier target balance for reporting member `i` (1..=24): members
/// block into `PointsTier::ALL` six at a time on `(i-1)/6`, and each lands
/// a fixed step above its tier's `floor()` — so the report's 4-tier /
/// 6-members-each shape follows the tier owner rather than a second copy
/// of its boundaries.
fn member_points_target(i: usize) -> i64 {
    let tier = PointsTier::ALL[(i - 1) / 6];
    let j = ((i - 1) % 6 + 1) as i64; // 1..=6 within the tier
    let above_floor = match tier {
        PointsTier::Regular => 100 + j * 50, //  150-400
        PointsTier::Bronze => j * 200,       //  700-1700
        PointsTier::Silver => j * 400,       // 2400-4400
        PointsTier::Gold => j * 500,         // 5500-8000
    };
    tier.floor() + above_floor
}

/// Settle `members` — reporting member `i` is `members[i - 1]` — to their
/// points-tier targets at `now`, returning how many needed a ledger row.
/// Every member, every run, not just newly created ones: the 12-month
/// report window slides, so each run's orders keep adding `checkout_earn`
/// for existing members, and only an every-run settlement keeps them inside
/// their tier. A member already at target computes `delta = 0` and writes
/// nothing, so a same-instant rerun is a no-op and a run that crashed before
/// settling is finished by the next. A developer's hand-edited balance is
/// reset to target too. Each member settles in its own transaction:
/// `lock_balance_tx`, `delta = target − lock.balance()`, apply, commit.
pub(super) async fn settle_members(
    db: &PgPool,
    members: &[Uuid],
    now: DateTime<Utc>,
) -> anyhow::Result<usize> {
    let mut settled = 0usize;
    for (i, user_id) in (1..).zip(members) {
        let target = member_points_target(i);
        let mut tx = db
            .begin()
            .await
            .with_context(|| format!("begin points settlement tx for seed member {user_id}"))?;
        let lock = points_service::lock_balance_tx(&mut tx, *user_id)
            .await
            .with_context(|| format!("lock points balance of seed member {user_id}"))?;
        let delta = target - lock.balance();
        if delta != 0 {
            points_service::apply_delta_tx(&mut tx, &lock, LedgerDelta::admin_adjust(delta), now)
                .await
                .with_context(|| format!("settle points-tier balance for seed member {user_id}"))?;
            settled += 1;
        }
        tx.commit()
            .await
            .with_context(|| format!("commit points settlement for seed member {user_id}"))?;
    }
    Ok(settled)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, TimeZone, Utc};
    use sqlx::PgPool;
    use uuid::Uuid;

    use dream_fly_backend::modules::points::model::{LedgerDelta, PointsTier};
    use dream_fly_backend::modules::points::repository::find_ledger_by_user;
    use dream_fly_backend::modules::points::service::{apply_delta_tx, lock_balance_tx};

    use super::{member_points_target, settle_members};
    use crate::dataset::{SeedAccount, upsert_account};

    /// The run instant every test here settles at: 2026-03-10T03:00Z.
    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap()
    }

    /// The smallest member list: reporting members 1 and 2, born with 0
    /// points through the account interface.
    async fn two_members(db: &PgPool) -> Vec<Uuid> {
        let mut ids = Vec::new();
        for email in ["seed-member-01@dreamfly.tw", "seed-member-02@dreamfly.tw"] {
            let account = SeedAccount {
                email,
                name: "示範會員",
                password_hash: "not-a-real-hash",
                birth_date: None,
                points: 0,
                joined_at: Utc.with_ymd_and_hms(2025, 3, 1, 4, 0, 0).unwrap(),
            };
            ids.push(upsert_account(db, &account, at()).await.expect("born"));
        }
        ids
    }

    async fn balance(db: &PgPool, id: Uuid) -> i64 {
        sqlx::query_scalar("SELECT points_balance::BIGINT FROM users WHERE id = $1")
            .bind(id)
            .fetch_one(db)
            .await
            .expect("fetch balance")
    }

    #[test]
    fn member_points_targets_match_the_legacy_formula() {
        // The 24 targets the old inline `match (i - 1) / 6` produced — the
        // PointsTier-derived formula must reproduce them exactly.
        let legacy: [i64; 24] = [
            150, 200, 250, 300, 350, 400, // <500
            700, 900, 1_100, 1_300, 1_500, 1_700, // 500-1999
            2_400, 2_800, 3_200, 3_600, 4_000, 4_400, // 2000-4999
            5_500, 6_000, 6_500, 7_000, 7_500, 8_000, // ≥5000
        ];
        for (i, expected) in (1..=24usize).zip(legacy) {
            assert_eq!(member_points_target(i), expected, "member {i}");
        }
    }

    #[test]
    fn member_points_targets_put_six_members_in_each_tier() {
        for &tier in PointsTier::ALL {
            let members = (1..=24usize)
                .filter(|&i| PointsTier::from_balance(member_points_target(i)) == tier)
                .count();
            assert_eq!(members, 6, "{tier:?}");
        }
    }

    /// Each member settles to its tier target exactly once: the first call
    /// writes one `admin_adjust` per member, the second finds both at
    /// target and writes nothing.
    #[sqlx::test]
    async fn settles_each_member_to_its_target_once(db: PgPool) {
        let members = two_members(&db).await;

        assert_eq!(
            settle_members(&db, &members, at()).await.expect("settle"),
            2
        );
        assert_eq!(balance(&db, members[0]).await, 150);
        assert_eq!(balance(&db, members[1]).await, 200);

        assert_eq!(
            settle_members(&db, &members, at()).await.expect("resettle"),
            0
        );
        for &id in &members {
            let rows = find_ledger_by_user(&db, id, 100, 0).await.expect("ledger");
            assert_eq!(rows.len(), 1, "member {id}");
        }
    }

    /// Drift (regression d819e21): +500 written out of time order — stamped
    /// before the settlement already in the ledger — is settled back to the
    /// target, and the member's ledger still lists as one chain from 0 to
    /// the balance.
    #[sqlx::test]
    async fn drift_written_out_of_time_order_settles_into_one_chain(db: PgPool) {
        let members = two_members(&db).await;
        settle_members(&db, &members, at()).await.expect("settle");

        let drifted = members[0];
        let mut tx = db.begin().await.expect("begin drift tx");
        let lock = lock_balance_tx(&mut tx, drifted).await.expect("lock");
        apply_delta_tx(
            &mut tx,
            &lock,
            LedgerDelta::admin_adjust(500),
            at() - Duration::days(1),
        )
        .await
        .expect("drift");
        tx.commit().await.expect("commit drift tx");

        assert_eq!(
            settle_members(&db, &members, at()).await.expect("resettle"),
            1
        );
        assert_eq!(balance(&db, drifted).await, 150);

        let rows = find_ledger_by_user(&db, drifted, 100, 0)
            .await
            .expect("ledger");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].balance_after, 150, "row 0 is the balance");
        for (i, w) in rows.windows(2).enumerate() {
            assert_eq!(
                w[1].balance_after,
                w[0].balance_after - w[0].delta,
                "row {} chains on from row {i}",
                i + 1
            );
        }
        let oldest = rows.last().expect("three rows");
        assert_eq!(oldest.balance_after - oldest.delta, 0, "opens from 0");
    }
}
