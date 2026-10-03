# ADR-0015: 場租佔位讀時計數;刪除 `time_slots.booked`

## Context

`time_slots.booked` 是「這個時段上有幾筆佔位 booking」的落地快取。佔位謂詞的 owner 是
`BookingStatus::occupies_seat`(只有 `cancelled` 不佔位),但快取靠三處各自維護:

- runtime:`bookings::occupancy` 的 `booked = booked + 1 WHERE booked < capacity` 與取消時的
  `booked = booked - 1 WHERE booked > 0`(後者沒命中時靜默 no-op——漂移被吞掉);
- seed:`upsert_time_slot` 插入時帶入算好的 `booked`,既有列再以 `WHERE booked = 0` 的 UPDATE 同步;
- 測試 fixture:`seed_booking` 依 `occupies_seat` 在同一交易內 `booked += 1`。

任何一處漏掉,`booked` 就與 bookings 列不符,而 `SlotStatus::derive`(available/limited/full)直接讀它。
三處的同步規則各寫一份,另靠 seed 的不變量測試與一條「對帳」斷言事後抓漂移。

課程端已有同形狀的解法:`courses::seats` 不存報名數,鎖 `courses` 列後 COUNT `active_enrolments`
view(migration `20260711000001`)。

## Decision

**佔用數不落地,讀時計數;計數走新 view `occupying_bookings`;佔位改「鎖列 + 計數」。**
(Controller 裁決 4。)

- Migration `20261003000002_time_slots_booked_read_time_count`:
  `CREATE VIEW occupying_bookings AS SELECT … FROM bookings WHERE status IN ('pending','confirmed',
  'completed','no_show')`;`ALTER TABLE time_slots DROP CONSTRAINT time_slots_booked_bound, DROP COLUMN
  booked`。
- View 的狀態清單 = `occupies_seat()` 為真的變體。`tests/service_bookings.rs::
  occupying_bookings_view_matches_occupies_seat` 每個變體插一筆、比對 view 讀出的集合;同一測試以窮舉
  `match`(無 `_` arm)與 `enum_range(NULL::booking_status)` 比對當 tripwire——新增變體(Rust 或 PG 任一
  邊)不先決定它佔不佔位,測試編譯失敗或斷言失敗。
- 佔位 `bookings::occupancy::occupy_slot_tx`:語句 1 `SELECT … FROM time_slots WHERE id = $1 AND
  is_closed = false FOR NO KEY UPDATE`;語句 2(分開下)`SELECT COUNT(*) FROM occupying_bookings WHERE
  time_slot_id = $1`,`>= capacity` → `None`。COUNT 必須在取鎖之後的另一條 statement:READ COMMITTED 的
  snapshot 以 statement 為單位,等鎖完成後才建立的 snapshot 才看得到前一位佔位者剛 commit 的 booking 列。
  `NO KEY UPDATE` 而非 `UPDATE`:不改主鍵,也不擋 bookings 外鍵檢查的 `FOR KEY SHARE`。三種失敗(不存在、
  已關閉、已滿)仍摺成同一個 `None` → 400「time slot is full or closed」。
- 釋出 `cancel_occupying_booking_tx`(原 `cancel_and_release_tx`):只剩 bookings 的條件 UPDATE,不碰
  `time_slots`。`cancel_booking` 原本「先 `FOR SHARE` 讀 slot、再 UPDATE 同一列」的鎖升級隨之消失。
- 讀取 `schedule::repository::SLOT_COLUMNS`:6 個投影站(4 個 SELECT、`bulk_create_tx` 與 `set_closed` 的
  `RETURNING`)共用一份欄位清單,`booked` 是對 view 的 correlated `COUNT(*)::int`。`TimeSlot.booked`、
  `SlotStatus::derive`、wire `TimeSlotResponse.booked` 皆不變(契約 §3.6)。
- 計數放 view、不放 Rust 常數:`schedule` 讀取端不能 import `bookings`(`bookings::service` 已依賴
  `schedule`,反向成循環);view 讓兩個模組共用同一份 SQL 定義。
- seed 與 `seed_booking` 不再寫或同步任何佔用數;seed 不變量「每個時段 `booked` = 佔位 booking 數」與
  `service_bookings.rs` 的對帳斷言一併刪除(對著同一個 view 自比是套套邏輯)。

## 落選方案

- **保留欄位、補 trigger 維護**:把三處同步收成一處,但快取仍是第二份事實;trigger 對 seed/fixture 的
  直寫也要正確,且容量上限仍需鎖或 CHECK 另外擔保。讀時計數直接消滅第二份事實。
- **單條 `UPDATE … WHERE (SELECT COUNT …) < capacity` 或 `SELECT … FOR UPDATE` 帶 COUNT 子查詢**:子查詢的
  snapshot 在等鎖之前就取好,併發下兩位都數到 0,同時通過——正是 `courses::seats` 文件記下「COUNT 不可
  併入取鎖那條 statement」的原因。
- **`SERIALIZABLE` 交易**:正確但要呼叫端處理序列化失敗重試;本庫其他名額判斷(`courses::seats`)都是
  READ COMMITTED + 列鎖,維持同一種模型。
- **在 Rust 端算 `booked`(讀 bookings 列後過濾 `occupies_seat`)**:讀取端每個 slot 多一次查詢或要把
  bookings 撈回記憶體,且 `schedule` 仍得 import `bookings`。

## Consequences

- 時段讀取多一個 correlated COUNT;`idx_bookings_time_slot_id` 支撐它,月曆讀取的 slot 數量級(百)不構成
  負擔。
- 容量上限只由 `occupy_slot_tx` 的鎖序擔保,DB 不再有 `booked <= capacity` 的 CHECK。繞過協定直寫
  bookings(seed、fixture、手動 SQL)可以讓計數超過 capacity——讀取端 `derive` 仍回 `full`,不會出錯。
- 新增 `BookingStatus` 變體:同時改 `occupies_seat` 與以新 migration 重建 view(`CREATE OR REPLACE VIEW`),
  否則 `occupying_bookings_view_matches_occupies_seat` 失敗。
- 任何新的 slot 投影必須用 `SLOT_COLUMNS`,不要手寫 `booked`(欄位已不存在,手寫會在執行期失敗)。
