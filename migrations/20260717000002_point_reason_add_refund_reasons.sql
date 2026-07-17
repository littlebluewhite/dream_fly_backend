-- =============================================================================
-- Step 10a(arch-deepening-r5)— point_reason 加兩個退款方向的值域。
--
-- `refund_restore`/`refund_clawback`:訂單退款/取消補償(Step 10)沖銷結帳
-- 當下點數流的兩個方向。契約 §1.6 的「一個 reason ⇒ 固定正負號」invariant
-- 延伸至此:`refund_restore` 恆正(沖回 checkout_redeem 扣掉的點數)、
-- `refund_clawback` 恆負(沖回 checkout_earn 賺到的點數)——兩個值而非一個,
-- 因為退款可能同時要「還點」與「收點」,兩個方向符號相反,唯有各自一個
-- reason 才能讓 `uniq_point_ledger_refund_once`(下一個 migration 的 partial
-- unique index)以 reason 分辨方向、各自「至多一列」。
--
-- 獨立成檔:PostgreSQL 禁止在新增 enum 值的同一交易內「使用」該值,sqlx
-- 每個 migration 檔各自一個交易——下一個 migration 的 partial unique index
-- 會在 WHERE 子句引用這兩個新值,故不能與本檔合併(範式:`20260707000004`)。
-- 本檔內兩個 ADD VALUE 彼此不互相引用、同檔內誰都沒被「用」,同檔安全
-- (範式:`20260707000005`)。
-- =============================================================================

ALTER TYPE point_reason ADD VALUE IF NOT EXISTS 'refund_restore';
ALTER TYPE point_reason ADD VALUE IF NOT EXISTS 'refund_clawback';
