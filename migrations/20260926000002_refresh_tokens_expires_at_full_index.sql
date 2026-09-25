-- =============================================================================
-- refresh_tokens.expires_at:partial index(`WHERE revoked = false`)改 full。
--
-- `auth::session::purge_expired` 改為只刪過期列(`WHERE expires_at < NOW()`)
-- ——已 revoke 但未過期的列刻意保留,reuse detection 要靠它辨認被重放的
-- 已輪替 token(舊清理連 revoked 列一起刪,清理跑過後重放只會得到無整族
-- 撤銷的一般 401)。清理現在掃的是所有過期列(含 revoked),原本只涵蓋
-- `revoked = false` 的 partial index 撐不到這條查詢,改為 full index。
-- =============================================================================

DROP INDEX IF EXISTS idx_refresh_tokens_expires_at;

CREATE INDEX IF NOT EXISTS idx_refresh_tokens_expires_at
    ON refresh_tokens(expires_at);
