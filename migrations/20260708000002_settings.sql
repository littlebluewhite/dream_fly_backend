-- =============================================================================
-- Global key-value settings (Round 4 Task B6) — backing store for the admin
-- desktop "系統設定" page and the mobile-admin settings screen, both of which
-- are currently pure local/mock state.
--
-- Design decision: a single flat KV table, not a fine-grained schema per
-- setting group. `key` is a free-form string, unconstrained by any backend
-- enum — the frontend-convention keys this round expects
-- (`studio_profile` / `notification_flags` / `security`) are documented in
-- docs/api/integration-contract.md §3.25 only, not enforced here.
--
-- Out of scope: the "登入裝置清單" (logged-in device list) needs real session
-- management and is tracked as a separate task, not part of this table.
-- =============================================================================

CREATE TABLE settings (
    key        TEXT        PRIMARY KEY,
    value      JSONB       NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
