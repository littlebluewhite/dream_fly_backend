//! 場租佔位(Venue-Rental Occupancy)——「這筆場租預約(booking)是否佔用
//! 一個 `time_slots` 座位」這個協定的單一 owner:`time_slots.booked` 增減
//! 必須與 `bookings` 列的 insert/cancel 成對出現,本模組是唯一持有這四條
//! SQL 的地方,對外(`bookings` 模組以外)不可見。
//!
//! 【owner 放 bookings,不放 schedule】四個理由:依賴方向既定
//! (`bookings::service` 已 import `schedule`,反向為零,放 schedule 會
//! 造出循環依賴);判斷「這筆 booking 佔不佔位」的謂詞 owner
//! `BookingStatus::occupies_seat` 已在 `bookings::model`,快取維護協定應
//! 跟著謂詞走(「給點(Point Grant)」詞條的先例:欄位所在表的模組不等於
//! 寫協定的 owner);未來若出現第三個寫入者(狀態轉移端點,如
//! confirmed→completed/no_show),它也會落在 `bookings`;Rust 可見性只有
//! 搬進 `bookings` 才做得到「對外不可見」——留在 `schedule` 的話,
//! `bookings::service` 要呼叫就必須是 `pub`,其餘所有模組皆可繞過協定
//! 直接呼叫 increment/decrement。
//!
//! 這是 repo 第二處 repository.rs 以外的 SQL——先例是
//! [`crate::modules::courses::seats`]:佔位判斷的 SQL 必須與判斷邏輯同檔,
//! 佔位協定才能成為 interface 的一部分,而不是散在呼叫端的紀律。
//!
//! 【seed bypass】`src/bin/seed.rs` 佈歷史場租資料時**不消費**本模組——
//! 它是 pool-based 冪等批次(重複執行需先辨識既有列,`booked` 直接算好
//! 帶入 INSERT),本模組「`&mut Transaction` + WHERE gate」的 runtime 形狀
//! 對它是錯的形狀;硬套需要把這裡的函式升級成 `pub`,摧毀私有化的目的。
//! seed 消費的是同一個謂詞(`BookingStatus::occupies_seat`,決定要不要
//! 插入一筆 booking 列並讓 `booked` 反映佔用),不是本協定本身——這個
//! 「seed 共用謂詞、不共用協定」的形狀是既有先例,CONTEXT.md「給點
//! (Point Grant)」詞條即以 `occupies_seat` 的這個 runtime/seed 分工模式
//! 為對照,是記錄在案的 bypass,不是遺漏。
//!
//! 殘餘誠實縫(同 `orders::service::tx_witness::TxReleased` 的
//! `no_open_tx` 自證式):`SlotOccupancy` 的 `#[must_use]` 只擋得住「值
//! 完全未被使用」這個編譯期可查的情形——一旦綁定給變數,呼叫端仍可能只
//! 呼叫 `date()`/`start_time()` 讀時間、卻不呼叫
//! [`insert_occupying_booking_tx`] 消費它,讓它在交易仍 commit 的情況下
//! 被靜默丟棄,留下「已 `booked + 1`、無對應 booking 列」的漂移。型別
//! 系統只到這裡,再往上是呼叫端紀律。

use chrono::{NaiveDate, NaiveTime};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::modules::schedule::model::TimeSlot;

use super::model::Booking;

/// 佔位 witness:[`occupy_slot_tx`] 對某個 `time_slots` 列成功
/// `booked + 1` 之後回傳,唯一建構點。欄位私有;唯讀存取
/// `date()`/`start_time()` 供呼叫端(`bookings::service::create_booking`)
/// 做 `require_not_started` 時間檢查——`id`/`price_cents` 只在本檔內部
/// ([`insert_occupying_booking_tx`])直接取用,不對外洩漏。
#[must_use = "佔位後必須落列(insert_occupying_booking_tx)或讓交易 rollback"]
pub(super) struct SlotOccupancy {
    slot: TimeSlot,
}

impl SlotOccupancy {
    pub(super) fn date(&self) -> NaiveDate {
        self.slot.date
    }

    pub(super) fn start_time(&self) -> NaiveTime {
        self.slot.start_time
    }
}

/// 原 `schedule::repository::increment_booked_tx`(SQL 逐字搬入)。原子
/// 遞增 `booked`——`WHERE id = $1 AND booked < capacity AND is_closed =
/// false` 這一個 WHERE 子句把三種失敗因由摺進同一個 `None`:slot 不存在、
/// 已滿、或 admin 關閉(`is_closed`)——`service::create_booking` 收到
/// `None` 一律映成同一句 400「time slot is full or closed」,不區分三者、
/// 不升級為 404(`create_booking_missing_slot_maps_to_full_or_closed_bad_request`
/// 釘住「不存在」這條分類)。`status` 不在此寫入:讀取時由
/// `booked`/`capacity`/`is_closed` 純函式推導(見
/// [`crate::modules::schedule::model::SlotStatus::derive`]),這裡只碰
/// 事實欄位。
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
    let slot = sqlx::query_as::<_, TimeSlot>(
        "UPDATE time_slots SET \
         booked = booked + 1, \
         updated_at = now() \
         WHERE id = $1 AND booked < capacity AND is_closed = false \
         RETURNING id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, booked, is_closed, created_at, updated_at",
    )
    .bind(slot_id)
    .fetch_optional(&mut **tx)
    .await?;

    Ok(slot.map(|slot| SlotOccupancy { slot }))
}

/// 原 `bookings::repository::create_tx`(SQL 逐字搬入)。以值消費
/// [`SlotOccupancy`]——一次佔位只換得恰一列 booking insert,witness 用過
/// 即隨參數消滅(Rust 所有權擋掉「同一次佔位插兩列」;「佔位後忘了插列」
/// 仍需交易 rollback 兜底,見 [`SlotOccupancy`] 的 `#[must_use]` 與模組
/// 文件的殘餘誠實縫說明)。`time_slot_id`/`price_cents` 皆取自 `hold`
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

/// 原 `bookings::repository::cancel_if_active_tx` +
/// `schedule::repository::decrement_booked_tx` 成對收攏(兩條 SQL 逐字
/// 搬入,交易內語句順序不變):條件 UPDATE 命中(非 cancelled →
/// cancelled)才接著 decrement;`None`(booking 已是 cancelled 或不存在)
/// 不觸發 decrement,呼叫端據此判斷「已取消過」回 409。
///
/// 【刻意不修】decrement 的 `WHERE booked > 0` 若沒命中(counter 已是
/// 0)靜默 no-op,回傳值被本函式丟棄、不檢查、不升級為錯誤——升級會讓一
/// 次合法的取消動作在 counter 已漂移時變成 500,而 counter 漂移只是快取
/// 偏差,不代表這次取消本身失敗。decrement 也刻意無 `is_closed`
/// gate:admin 事後關閉的 slot,既有預約仍必須能正常取消釋出座位——
/// `is_closed` 只 gate [`occupy_slot_tx`] 的新佔位。
/// `cancel_booking_on_closed_slot_still_releases_seat` 是這條的 pin 測試。
pub(super) async fn cancel_and_release_tx(
    tx: &mut Transaction<'_, Postgres>,
    booking_id: Uuid,
) -> Result<Option<Booking>, sqlx::Error> {
    let Some(booking) = sqlx::query_as::<_, Booking>(
        "UPDATE bookings \
         SET status = 'cancelled'::booking_status, updated_at = NOW() \
         WHERE id = $1 AND status <> 'cancelled'::booking_status \
         RETURNING id, user_id, time_slot_id, status, note, price_cents, created_at, updated_at",
    )
    .bind(booking_id)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };

    sqlx::query_as::<_, TimeSlot>(
        "UPDATE time_slots SET \
         booked = booked - 1, \
         updated_at = now() \
         WHERE id = $1 AND booked > 0 \
         RETURNING id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, booked, is_closed, created_at, updated_at",
    )
    .bind(booking.time_slot_id)
    .fetch_optional(&mut **tx)
    .await?;

    Ok(Some(booking))
}
