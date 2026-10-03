-- =============================================================================
-- time_slots.booked 收斂為讀時計數(ADR-0015)。
--
-- `booked` 是「這個 slot 上有幾筆佔位 booking」的落地快取,靠 runtime 的
-- increment/decrement 協定(`bookings::occupancy`)、seed 的同步 UPDATE、
-- 測試 fixture 的 bump 三處各自維護——任何一處漏掉,快取就與 bookings 列
-- 漂移,而 `SlotStatus::derive`(available/limited/full)直接讀它。改成:
-- DB 只存事實(bookings 列),`booked` 在讀取時由本 view 計數。
--
-- 佔位口徑下沉為 view `occupying_bookings`(先例:`active_enrolments`,
-- migration `20260711000001`):狀態清單必須恰為 Rust 端
-- `BookingStatus::occupies_seat()` 為真的變體——只有 `cancelled` 釋出座位,
-- `completed`/`no_show` 是終局但仍佔位。兩邊一致由
-- `tests/service_bookings.rs::occupying_bookings_view_matches_occupies_seat`
-- 釘住(含 PG enum 與 Rust 變體的窮舉 tripwire)。放 view 而不是 Rust 常數:
-- 讀取端 `schedule::repository` 不能 import `bookings`(`bookings::service`
-- 已依賴 `schedule`,反向會成循環),view 讓兩個模組共用同一份 SQL 定義。
--
-- 欄位顯式列出(不用 `SELECT *`),對齊 `bookings` 表現況 8 欄(init 7 欄 +
-- `20260708000006` 的 `price_cents`)。
--
-- 併發:佔位改為「`FOR NO KEY UPDATE` 鎖 slot 列 → 另一條 statement COUNT
-- 本 view」(比照 `courses::seats`),容量上限由鎖序列化擔保,不再靠
-- `time_slots_booked_bound` CHECK——該 CHECK 隨欄位一起刪除。
-- =============================================================================

CREATE VIEW occupying_bookings AS
SELECT id, user_id, time_slot_id, status, note, price_cents,
       created_at, updated_at
  FROM bookings
 WHERE status IN ('pending', 'confirmed', 'completed', 'no_show');

ALTER TABLE time_slots
    DROP CONSTRAINT time_slots_booked_bound,
    DROP COLUMN booked;
