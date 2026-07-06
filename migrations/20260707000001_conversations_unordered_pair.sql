-- =============================================================================
-- Conversations: unordered-pair uniqueness (Round 3 Task 4 review fix).
--
-- The original UNIQUE(member_id, coach_id) treats the pair as ORDERED. When
-- two users each hold BOTH the coach and member roles, the service's role
-- normalization assigns whichever side CALLS as the coach: A→B stores
-- (member=B, coach=A) while B→A stores (member=A, coach=B) — two rows for
-- the same two people, splitting their message history. Replace the ordered
-- constraint with a unique expression index over the UNORDERED pair so each
-- pair of users gets at most one conversation regardless of who created it,
-- enforced at the DB layer against concurrent creates (the service's
-- get-or-create catches the 23505 and re-fetches).
-- =============================================================================

ALTER TABLE conversations DROP CONSTRAINT conversations_unique_pair;

CREATE UNIQUE INDEX conversations_unique_user_pair
    ON conversations (LEAST(member_id, coach_id), GREATEST(member_id, coach_id));
