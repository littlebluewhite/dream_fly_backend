-- =============================================================================
-- Report cards & certificates (Round 3 Task 5) — 純 metadata，無 PDF/檔案儲存
-- （裁決 6，任務規格原文）。report_cards 是教練對某位學員在某堂課「單一
-- enrolment」的期別評語/評分；certificates 是學員獲頒的證書，不綁定特定課程
-- （course_id 可為 NULL）。兩者皆為建立後不可變的紀錄，故無 updated_at。
-- =============================================================================

CREATE TABLE report_cards (
    id           UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    enrolment_id UUID         NOT NULL REFERENCES enrolments(id),
    term_label   VARCHAR(100) NOT NULL,
    comment      TEXT,
    rating       SMALLINT     CHECK (rating BETWEEN 1 AND 5),
    created_by   UUID         NOT NULL REFERENCES users(id),
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    UNIQUE (enrolment_id, term_label)
);

CREATE TABLE certificates (
    id         UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID         NOT NULL REFERENCES users(id),
    course_id  UUID         REFERENCES courses(id),
    title      VARCHAR(200) NOT NULL,
    level      VARCHAR(100),
    issued_on  DATE         NOT NULL,
    issued_by  UUID         NOT NULL REFERENCES users(id),
    note       TEXT,
    created_at TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

-- Supports `GET /certificates/me`'s `WHERE user_id = $1` lookup — brand-new
-- table with no existing index covering it (mirrors
-- `idx_conversations_coach_id`'s precedent of indexing a real query pattern
-- not already served by a UNIQUE index).
CREATE INDEX idx_certificates_user_id ON certificates(user_id);
