-- =============================================================================
-- events_outbox.next_attempt_at:派送器的租約與退避時間點。
--
-- 舊派送器用一個交易包住 `SELECT … FOR UPDATE SKIP LOCKED` → 逐筆 await
-- Kafka publish → 標記 → commit;publish 一慢,連線閒置在交易中超過
-- `idle_in_transaction_session_timeout`(main.rs 設 30s)就被 Postgres 砍掉,
-- 記帳落不了地。改為 `kafka::outbox::drain_once` 以一條 autocommit UPDATE
-- 認領(把 `next_attempt_at` 推到 NOW() + 租約),在交易外 publish,最後用
-- 短交易記帳;失敗列的 `next_attempt_at` 設為 NOW() + 指數退避。
--
-- 既有列 DEFAULT NOW() 即「立刻可派送」,與部署前行為相同。
-- 掃描謂詞從 `ORDER BY created_at` 改為 `next_attempt_at <= NOW()`,partial
-- index 隨之從 `(created_at)` 換成 `(next_attempt_at)`,仍只涵蓋未發送列。
--
-- 鎖:ADD COLUMN 的 DEFAULT NOW() 非 volatile,於 ALTER 時求值一次、不重寫表
-- (PG 11+);DROP/CREATE INDEX 在 migration 交易內持有 ACCESS EXCLUSIVE 鎖
-- 直到建完,於 server 啟動、開始接受連線之前執行,可接受。
-- =============================================================================

ALTER TABLE events_outbox
    ADD COLUMN next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

DROP INDEX idx_events_outbox_pending;

CREATE INDEX idx_events_outbox_due
    ON events_outbox (next_attempt_at)
    WHERE published_at IS NULL;
