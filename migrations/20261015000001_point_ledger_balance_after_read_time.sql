-- =============================================================================
-- point_ledger.balance_after 收斂為讀時推導(ADR-0018)。
--
-- `balance_after` 是寫入時落地的「這一筆之後的餘額」,取自 `apply_delta_tx`
-- 的 `UPDATE users … RETURNING points_balance`,所以依寫入順序累加;明細卻依
-- 業務時間 `created_at`(ADR-0017)列出。兩個順序不一致時,列出的明細就串不
-- 起來——seed 曾因此要靠排程與排列搜尋測試守住寫入順序。改成:DB 只存事實
-- (delta、時間),`balance_after` 在讀取時由 `points::repository::
-- find_ledger_by_user` 以讀取當下的 `users.points_balance` 為錨、減去比它新
-- 的列的 delta 總和推導。
--
-- 明細排序改為 `(created_at DESC, id DESC)`:`id` 讓同一刻的列有固定順序,
-- 翻頁穩定。`idx_point_ledger_user` 隨之重建成同一個順序。
--
-- 只能往前:套用後,舊版 binary 寫不了點數帳——它的 `INSERT` 指名的
-- `balance_after` 欄位已不存在。
-- =============================================================================

ALTER TABLE point_ledger DROP COLUMN balance_after;

DROP INDEX idx_point_ledger_user;
CREATE INDEX idx_point_ledger_user ON point_ledger (user_id, created_at DESC, id DESC);
