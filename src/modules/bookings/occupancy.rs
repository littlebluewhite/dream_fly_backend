//! 場租佔位(Venue-Rental Occupancy)——「這筆場租預約(booking)能不能
//! 佔用一個 `time_slots` 座位」這個協定的單一 owner:佔位(鎖 slot 列 →
//! 計數 → 插入 booking)與釋出(booking 轉 cancelled)的 SQL 只住在本模組,
//! 對外(`bookings` 模組以外)不可見。
//!
//! 【讀時計數,無快取欄位】(ADR-0015)slot 已被佔用的座位數不落地:
//! `time_slots.booked` 欄位已刪除(migration
//! `20261003000002_time_slots_booked_read_time_count`),一律 COUNT
//! `occupying_bookings` view——佔位口徑的 SQL 端單一定義,狀態清單與
//! `BookingStatus::occupies_seat` 一致(由
//! `occupying_bookings_view_matches_occupies_seat` 釘住)。讀取端
//! (`schedule::repository::SLOT_COLUMNS`)也計同一個 view,所以釋出座位
//! 只是 booking 列轉 cancelled,沒有第二個欄位要同步。
//!
//! 【owner 放 bookings,不放 schedule】依賴方向既定(`bookings::service`
//! 已 import `schedule`,反向為零,放 schedule 會造出循環依賴);判斷
//! 「這筆 booking 佔不佔位」的謂詞 owner `BookingStatus::occupies_seat`
//! 已在 `bookings::model`,佔位協定跟著謂詞走;Rust 可見性只有搬進
//! `bookings` 才做得到「對外不可見」。
//!
//! 這是 repo 第二處 repository.rs 以外的 SQL——先例是
//! [`crate::modules::courses::seats`]:佔位判斷的 SQL 必須與判斷邏輯同檔,
//! 鎖協定才能成為 interface 的一部分,而不是散在呼叫端的紀律。
//!
//! 【seed bypass】`src/bin/seed/dataset.rs` 佈歷史場租資料時**不消費**本模組——
//! 它是 pool-based 冪等批次,直接插入指定狀態(`completed`/`cancelled`/
//! `no_show`)的 booking 列;沒有快取欄位要同步,佔不佔位由
//! `occupying_bookings` 讀時決定,所以 seed 連謂詞都不必呼叫。記錄在案的
//! bypass,不是遺漏;測試 fixture `seed_booking` 同理。
//!
//! 殘餘誠實縫(同 `orders::tx_witness::TxReleased` 的
//! `no_open_tx` 自證式):`SlotOccupancy` 的 `#[must_use]` 只擋得住「值
//! 完全未被使用」這個編譯期可查的情形——一旦綁定給變數,呼叫端仍可能只
//! 呼叫 `date()`/`start_time()` 讀時間、卻不呼叫
//! [`insert_occupying_booking_tx`] 消費它。沒有快取欄位後,這只會白白持有
//! slot 列鎖到交易結束,不再留下計數漂移。

use chrono::{NaiveDate, NaiveTime};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::model::Booking;

/// [`occupy_slot_tx`] 鎖住的 slot 列中,佔位判斷與落列需要的欄位。
#[derive(sqlx::FromRow)]
struct LockedSlot {
    id: Uuid,
    date: NaiveDate,
    start_time: NaiveTime,
    capacity: i32,
    price_cents: i64,
}

/// 佔位 witness:[`occupy_slot_tx`] 鎖住某個 `time_slots` 列、確認尚有
/// 空位之後回傳,唯一建構點。欄位私有;唯讀存取
/// `date()`/`start_time()` 供呼叫端(`bookings::service::create_booking`)
/// 做 `require_not_started` 時間檢查——`id`/`price_cents` 只在本檔內部
/// ([`insert_occupying_booking_tx`])直接取用,不對外洩漏。
#[must_use = "佔位後必須落列(insert_occupying_booking_tx)或讓交易 rollback"]
pub(super) struct SlotOccupancy {
    slot: LockedSlot,
}

impl SlotOccupancy {
    pub(super) fn date(&self) -> NaiveDate {
        self.slot.date
    }

    pub(super) fn start_time(&self) -> NaiveTime {
        self.slot.start_time
    }
}

/// 佔位:兩條 statement,比照 [`crate::modules::courses::seats::course_seats_tx`]。
///
/// 1. `SELECT … FROM time_slots WHERE id = $1 AND is_closed = false FOR NO
///    KEY UPDATE`——鎖住 slot 列,序列化同一 slot 的併發佔位。`NO KEY`:
///    不擋 `bookings` 外鍵檢查取的 `FOR KEY SHARE`,也不改主鍵。
/// 2. COUNT `occupying_bookings`,`>= capacity` → `None`。**COUNT 不可併入
///    取鎖那條 statement**:READ COMMITTED 下 snapshot 以 statement 為單位,
///    取鎖(可能阻塞等前一筆佔位 commit)完成後才建立的 snapshot,才數得到
///    對方剛插入的 booking 列。
///
/// 三種失敗因由摺進同一個 `None`:slot 不存在、admin 關閉(`is_closed`)、
/// 或已滿——`service::create_booking` 收到 `None` 一律映成同一句 400
/// 「time slot is full or closed」,不區分三者、不升級為 404
/// (`create_booking_missing_slot_maps_to_full_or_closed_bad_request`
/// 釘住「不存在」這條分類)。
///
/// 【刻意不修】呼叫端(`bookings::service::create_booking`)先呼叫本函式
/// 佔位、後做 `studio_clock::require_not_started` 時間檢查,這個順序不
/// 重排:對調的話,「slot 不存在」得先靠一次獨立 SELECT 才能做時間檢查,
/// 目前摺進本函式 `None`(400)的分類會變成 404;一個已滿又已開始的
/// slot,現行一律先被本函式擋下報「已滿」,對調後會變成先報「已開始」,
/// 優先序反轉。現行寫法靠提早 `return` 讓交易 rollback 兜底,沒有資源
/// 洩漏。`create_booking_full_and_started_slot_reports_full_not_started`
/// 是這條優先序的機器見證。
pub(super) async fn occupy_slot_tx(
    tx: &mut Transaction<'_, Postgres>,
    slot_id: Uuid,
) -> Result<Option<SlotOccupancy>, sqlx::Error> {
    let Some(slot) = sqlx::query_as::<_, LockedSlot>(
        "SELECT id, date, start_time, capacity, price_cents FROM time_slots \
         WHERE id = $1 AND is_closed = false \
         FOR NO KEY UPDATE",
    )
    .bind(slot_id)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };

    let occupied = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM occupying_bookings WHERE time_slot_id = $1",
    )
    .bind(slot.id)
    .fetch_one(&mut **tx)
    .await?;

    if occupied >= slot.capacity as i64 {
        return Ok(None);
    }
    Ok(Some(SlotOccupancy { slot }))
}

/// 原 `bookings::repository::create_tx`(SQL 逐字搬入)。以值消費
/// [`SlotOccupancy`]——一次佔位只換得恰一列 booking insert,witness 用過
/// 即隨參數消滅(Rust 所有權擋掉「同一次佔位插兩列」——同一份 COUNT 結果
/// 只換得一列;「佔位後忘了插列」見 [`SlotOccupancy`] 的 `#[must_use]` 與
/// 模組文件的殘餘誠實縫說明)。`time_slot_id`/`price_cents` 皆取自 `hold`
/// 內部欄位,不再由呼叫端另傳——`price_cents` 是 slot *下訂當下* 的價格
/// 快照(見 `bookings::model::Booking::price_cents`),兩者同出
/// [`occupy_slot_tx`] 讀到的同一列,消滅「傳錯 price/slot 配對」整類
/// 錯誤。
pub(super) async fn insert_occupying_booking_tx(
    tx: &mut Transaction<'_, Postgres>,
    hold: SlotOccupancy,
    user_id: Uuid,
    note: Option<&str>,
) -> Result<Booking, sqlx::Error> {
    sqlx::query_as::<_, Booking>(
        "INSERT INTO bookings (id, user_id, time_slot_id, status, note, price_cents, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, 'confirmed'::booking_status, $3, $4, now(), now()) \
         RETURNING id, user_id, time_slot_id, status, note, price_cents, created_at, updated_at",
    )
    .bind(user_id)
    .bind(hold.slot.id)
    .bind(note)
    .bind(hold.slot.price_cents)
    .fetch_one(&mut **tx)
    .await
}

/// 釋出座位:條件 UPDATE(非 cancelled → cancelled),命中即釋出——
/// 佔用數由 `occupying_bookings` 讀時計數,沒有第二個欄位要同步,因此
/// 這裡不碰 `time_slots`,也就沒有「取消時把 slot 讀鎖升級成寫鎖」這一步。
/// `None`(booking 已是 cancelled 或不存在)→ 呼叫端回 409「已取消過」。
///
/// 不看 `is_closed`:admin 事後關閉的 slot,既有預約仍必須能正常取消釋出
/// 座位——`is_closed` 只 gate [`occupy_slot_tx`] 的新佔位。
/// `cancel_booking_on_closed_slot_still_releases_seat` 是這條的 pin 測試。
pub(super) async fn cancel_occupying_booking_tx(
    tx: &mut Transaction<'_, Postgres>,
    booking_id: Uuid,
) -> Result<Option<Booking>, sqlx::Error> {
    sqlx::query_as::<_, Booking>(
        "UPDATE bookings \
         SET status = 'cancelled'::booking_status, updated_at = NOW() \
         WHERE id = $1 AND status <> 'cancelled'::booking_status \
         RETURNING id, user_id, time_slot_id, status, note, price_cents, created_at, updated_at",
    )
    .bind(booking_id)
    .fetch_optional(&mut **tx)
    .await
}
