-- =============================================================================
-- Step 8(arch-deepening-r5)— time_slots.status 收斂為讀時推導,DB 只存事實。
--
-- `status`(slot_status enum)是「booked/capacity 的函數 + 管理意圖(closed)」
-- 的落地快照,會漂移:booked 變動時若忘記同步 CASE 運算式,status 就與實際
-- 不符。收斂為讀時推導(`schedule::model::SlotStatus::derive`)——DB 只保留
-- 事實欄位:`booked`、`capacity`、新增的 `is_closed`(管理意圖旗標,取代舊
-- `status = 'closed'` 變體的落地表達)。status 本身不再落地儲存。
--
-- 執行順序關鍵:先加欄位、**回填既有 closed 意圖**、才能丟欄位——回填必須
-- 發生在 DROP COLUMN status 之前,否則既有「已關閉」時段的管理意圖會直接
-- 遺失(booked/capacity 反推不出「這格是被 admin 手動關閉,還是單純還沒
-- 被訂滿」)。
--
-- `time_slots_booked_bound` CHECK 與場地防重疊 EXCLUDE(`time_slots_venue_no_overlap`)
-- 皆與 status 無關,不動。歷史 migration(`20260410000001_init.sql` 的
-- `CREATE TYPE slot_status`)也不動——新庫仍按時序重放歷史、再由本 migration
-- 收掉,不回頭改寫既有 migration 檔。
-- =============================================================================

ALTER TABLE time_slots ADD COLUMN is_closed BOOLEAN NOT NULL DEFAULT false;

UPDATE time_slots SET is_closed = true WHERE status = 'closed';

ALTER TABLE time_slots DROP COLUMN status;

DROP TYPE slot_status;
