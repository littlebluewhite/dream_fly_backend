-- =============================================================================
-- Messages (Round 3 Task 4) — one-to-one conversation between exactly one
-- `coach`-role user and one `member`-role user (RBAC role check happens in
-- `messages::service`, not here; this schema only enforces the two ends
-- being distinct users). v1 has no file-attachment support — `messages.body`
-- is plain text only.
-- =============================================================================

CREATE TABLE conversations (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    member_id       UUID        NOT NULL REFERENCES users(id),
    coach_id        UUID        NOT NULL REFERENCES users(id),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_message_at TIMESTAMPTZ,
    CONSTRAINT conversations_member_coach_distinct CHECK (member_id <> coach_id),
    CONSTRAINT conversations_unique_pair UNIQUE (member_id, coach_id)
);

-- Supports `GET /conversations/me`'s `WHERE member_id = $1 OR coach_id = $1`
-- lookup from the coach side — the UNIQUE index above is member_id-leading,
-- so it doesn't serve a coach-id-only filter.
CREATE INDEX idx_conversations_coach_id ON conversations(coach_id);

CREATE TABLE messages (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id UUID        NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    sender_id       UUID        NOT NULL REFERENCES users(id),
    body            TEXT        NOT NULL CHECK (char_length(body) BETWEEN 1 AND 2000),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    read_at         TIMESTAMPTZ
);

-- Supports both `GET /conversations/{id}/messages`'s paginated
-- `created_at DESC` listing and the `unread_count`/`last_message_body`
-- correlated subqueries in `GET /conversations/me` — all keyed by
-- conversation_id and ordered by created_at.
CREATE INDEX idx_messages_conversation_created_at ON messages(conversation_id, created_at DESC);
