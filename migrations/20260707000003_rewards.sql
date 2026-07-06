-- =============================================================================
-- Rewards catalog & redemptions (Round 3 Task 6) — point-redeemable items.
-- 點數唯一真相仍是既有 point_ledger + users.points_balance（裁決 7）；本檔只新增
-- reward 目錄本身與兌換紀錄，不引入第二套點數欄位/機制。
-- =============================================================================

CREATE TABLE rewards (
    id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    name          VARCHAR(200) NOT NULL,
    description   TEXT,
    points_cost   INT          NOT NULL CHECK (points_cost > 0),
    stock         INT,
    is_active     BOOLEAN      NOT NULL DEFAULT true,
    display_order INT          NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    CONSTRAINT rewards_stock_nonneg CHECK (stock IS NULL OR stock >= 0)
);

CREATE TRIGGER trigger_rewards_updated_at BEFORE UPDATE ON rewards
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- Member-facing list is `WHERE is_active ORDER BY display_order` — supports
-- that query directly (admin's `?all=true` view has no filter, so a full
-- table scan there is fine at this catalog's expected size).
CREATE INDEX idx_rewards_active_display_order ON rewards(display_order) WHERE is_active = true;

CREATE TABLE reward_redemptions (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    reward_id    UUID        NOT NULL REFERENCES rewards(id),
    user_id      UUID        NOT NULL REFERENCES users(id),
    points_spent INT         NOT NULL CHECK (points_spent > 0),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Supports `GET /rewards/redemptions/me`'s `WHERE user_id = $1 ORDER BY
-- created_at DESC` (same precedent as idx_certificates_user_id / idx_point_ledger_user).
CREATE INDEX idx_reward_redemptions_user_id ON reward_redemptions(user_id, created_at DESC);
