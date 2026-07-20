//! 對話配對 (Conversation Pairing) — the role-resolution step of
//! `POST /conversations`, pulled out of the service body into a pure
//! function: given the caller and a target user's ids and role lists,
//! decide which one is the `member_id` side and which is the `coach_id`
//! side of the (at most one, unordered-pair-unique) conversation between
//! them, contract §3.21. Same shape as `orders::pricing`/
//! `orders::fulfilment`: pure function, zero DB, zero async —
//! `service::resolve_member_coach` still owns the one DB round trip
//! (`permissions_repository::find_role_names_by_user` for the target's
//! roles; the caller's roles are already on the loaded `AuthUser`, no query
//! needed), and `service::create_conversation` still owns the get-or-create
//! step and its unique-violation race handling (`:60-85`) — that's DB
//! orchestration, not a pairing decision.
//!
//! **Self-rejection is deliberately checked in *two* places.**
//! `service::resolve_member_coach` checks `target_id == auth.user_id`
//! itself, before it ever queries the target's roles — a self-request must
//! never fail differently depending on whether that DB round trip
//! succeeds, and must not spend a query on a request that's rejected
//! either way. [`resolve_pair`] repeats the identical check as its first
//! line, so the pure function is self-sufficient (correct on its own if
//! ever called directly, not just when a caller has pre-filtered
//! self-pairs) — the same check in two places is deliberate defense in
//! depth, not two different precedence rules.
//!
//! **Branch order is deliberately fixed and not "commutative-looking":**
//! the caller-is-coach branch is checked before the caller-is-member
//! branch. For a dual-role (coach *and* member) caller and a dual-role
//! target, both branches' conditions are simultaneously true — the first
//! match wins, so the caller always lands on the coach side of the pair.
//! This means A→B and B→A between two dual-role users do NOT produce the
//! same `(member_id, coach_id)` tuple — they produce a *mirror* pair (A→B
//! gives `(B, A)`, B→A gives `(A, B)`), which is exactly why the DB's
//! uniqueness constraint on the pair must be unordered (`LEAST`/`GREATEST`,
//! not a plain `(member_id, coach_id)` unique index): two ordered rows for
//! the same two people would silently split their message history in two.
//! Reordering these branches changes which side of an existing
//! conversation a dual-role caller resolves to.

use uuid::Uuid;

use crate::error::AppError;

/// Domain-validation message for every way a `POST /conversations` role
/// check can fail — same wording regardless of which side (or both) is
/// wrong, per contract §3.21.
pub const ROLE_VIOLATION: &str = "僅支援教練與會員間的對話";

/// Resolve `(member_id, coach_id)` for a conversation between `caller_id`
/// and `target_id` — order-independent in the sense that the caller may be
/// the coach side or the member side, so long as the *other* one holds the
/// complementary role. See the module doc for why branch order still
/// matters when both users hold both roles.
pub fn resolve_pair(
    caller_id: Uuid,
    caller_roles: &[String],
    target_id: Uuid,
    target_roles: &[String],
) -> Result<(Uuid, Uuid), AppError> {
    if target_id == caller_id {
        return Err(AppError::Validation(ROLE_VIOLATION.into()));
    }

    let caller_is_coach = caller_roles.iter().any(|r| r == "coach");
    let caller_is_member = caller_roles.iter().any(|r| r == "member");
    let target_is_coach = target_roles.iter().any(|r| r == "coach");
    let target_is_member = target_roles.iter().any(|r| r == "member");

    if caller_is_coach && target_is_member {
        Ok((target_id, caller_id)) // (member_id, coach_id)
    } else if caller_is_member && target_is_coach {
        Ok((caller_id, target_id)) // (member_id, coach_id)
    } else {
        Err(AppError::Validation(ROLE_VIOLATION.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn both_members_is_role_violation() {
        // create_role_violation_both_members_returns_422 (tests/http_messages.rs)
        let (caller, target) = (Uuid::now_v7(), Uuid::now_v7());
        let err = resolve_pair(caller, &roles(&["member"]), target, &roles(&["member"]))
            .expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == ROLE_VIOLATION),
            "got: {err:?}"
        );
    }

    #[test]
    fn admin_only_target_is_role_violation() {
        // create_role_violation_admin_only_returns_422 (tests/http_messages.rs):
        // an admin holding neither coach nor member fails just like two
        // members do.
        let (caller, target) = (Uuid::now_v7(), Uuid::now_v7());
        let err = resolve_pair(caller, &roles(&["admin"]), target, &roles(&["member"]))
            .expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == ROLE_VIOLATION),
            "got: {err:?}"
        );
    }

    #[test]
    fn self_target_is_role_violation() {
        // create_targeting_self_returns_422 (tests/http_messages.rs)
        let id = Uuid::now_v7();
        let err = resolve_pair(id, &roles(&["coach"]), id, &roles(&["coach"]))
            .expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == ROLE_VIOLATION),
            "got: {err:?}"
        );
    }

    #[test]
    fn dual_role_self_target_is_role_violation() {
        // create_targeting_self_with_dual_roles_returns_422
        // (tests/http_messages.rs): a coach+member dual-role user targeting
        // themself WOULD pass the complementary-role check (caller-as-coach
        // + self-as-member) if the self-check weren't first.
        let id = Uuid::now_v7();
        let dual = roles(&["coach", "member"]);
        let err = resolve_pair(id, &dual, id, &dual).expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == ROLE_VIOLATION),
            "got: {err:?}"
        );
    }

    #[test]
    fn coach_caller_member_target_normalizes_to_member_coach() {
        // create_between_coach_and_member_is_order_independent_and_idempotent
        // (tests/http_messages.rs), caller=coach direction.
        let (coach, member) = (Uuid::now_v7(), Uuid::now_v7());
        let (member_id, coach_id) =
            resolve_pair(coach, &roles(&["coach"]), member, &roles(&["member"])).expect("resolves");
        assert_eq!(member_id, member);
        assert_eq!(coach_id, coach);
    }

    #[test]
    fn member_caller_coach_target_normalizes_to_member_coach() {
        // Same fixture, caller=member direction — same output pair
        // regardless of who calls.
        let (coach, member) = (Uuid::now_v7(), Uuid::now_v7());
        let (member_id, coach_id) =
            resolve_pair(member, &roles(&["member"]), coach, &roles(&["coach"])).expect("resolves");
        assert_eq!(member_id, member);
        assert_eq!(coach_id, coach);
    }

    #[test]
    fn dual_role_pair_first_matching_branch_wins_caller_lands_on_coach_side() {
        // create_between_dual_role_users_is_idempotent_in_both_directions
        // (tests/http_messages.rs) + the module doc above: both users hold
        // both roles, so both branches' conditions are
        // simultaneously true for either direction — the caller-is-coach
        // branch is checked first, so the caller always lands on the coach
        // side. A→B and B→A are therefore a MIRROR pair, not the same pair.
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let dual = roles(&["coach", "member"]);

        let a_to_b = resolve_pair(a, &dual, b, &dual).expect("resolves");
        assert_eq!(a_to_b, (b, a), "A→B: caller A lands on the coach side");

        let b_to_a = resolve_pair(b, &dual, a, &dual).expect("resolves");
        assert_eq!(
            b_to_a,
            (a, b),
            "B→A: caller B lands on the coach side — mirror of A→B"
        );
    }
}
