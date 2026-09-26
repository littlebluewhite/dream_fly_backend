-- =============================================================================
-- refresh_tokens.expires_at:partial index(`WHERE revoked = false`)改 full。
--
-- `auth::session::purge_expired` 改為只刪過期列(`WHERE expires_at < NOW()`)
-- ——已 revoke 但未過期的列刻意保留,reuse detection 要靠它辨認被重放的
-- 已輪替 token(舊清理連 revoked 列一起刪,清理跑過後重放只會得到無整族
-- 撤銷的一般 401)。清理現在掃的是所有過期列(含 revoked),原本只涵蓋
-- `revoked = false` 的 partial index 撐不到這條查詢,改為 full index。
--
-- 鎖:DROP INDEX 對 refresh_tokens 取得 ACCESS EXCLUSIVE 鎖,並在同一個
-- migration 交易內一路持有到非 CONCURRENTLY 的 CREATE INDEX 建完、提交為止
-- (期間登入/refresh 對這張表的讀寫都會等待)。
-- 目前表小、本 migration 在 server 啟動時(開始接受連線之前)執行,可接受;
-- 表若變大,應改用 CREATE INDEX CONCURRENTLY(需拆出交易外執行)。
-- =============================================================================

DROP INDEX IF EXISTS idx_refresh_tokens_expires_at;

CREATE INDEX IF NOT EXISTS idx_refresh_tokens_expires_at
    ON refresh_tokens(expires_at);
