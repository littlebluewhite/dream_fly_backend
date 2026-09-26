//! 座位(Seats)——「課程還有沒有位子」這個 invariant 的單一 owner。
//!
//! 鎖協定、實體座位公式(契約 §3.20)是本模組持有的兩個面向;COUNT 謂詞
//! (原 `enrolments.status = 'active'`)這第三個面向已下沉為
//! `active_enrolments` view(migration `20260711000001`)單一持有——拆散
//! 到各模組的 repository 就會重造出靠「Copied (not imported)」註解人肉
//! 同步的 keep-in-sync 接縫(本檔成立前 enrolments/waitlist/leave 三處各
//! 持一份拷貝)。因此三個決策端(enrol、waitlist join、makeup)一律呼叫
//! 本模組取座位快照;`courses::repository`/`sessions::repository` 顯示用
//! 的 `enrolled_count` 子查詢改讀該 view(見各該檔案的註解)——原本在
//! 「函式化(= N+1 查詢)」與「共用 const(需 `format!` 組裝、犧牲字串
//! SQL 的可 grep 性)」間取捨的裁決,view 換底後兩個反對理由皆不成立。
//!
//! 這是 repo 第一處 repository.rs 以外的 SQL——**刻意、有文件的例外**
//! (先例:`orders/pricing.rs` 已是「模組第七檔」):seat 判斷的 SQL 與
//! 判斷邏輯必須同檔,鎖協定才能成為 interface 的一部分,而不是散在呼叫端
//! 的紀律。
//!
//! 【鎖策略】鎖策略以參數型別宣告,不靠呼叫端自律:
//! - `&PgPool`、無前綴(`course_seats`)= 無鎖快照,stale 可接受;
//! - `&mut Transaction` + `lock_` 前綴(`lock_courses_tx`、
//!   `lock_session_tx`)= 在呼叫端交易內取 `FOR UPDATE` 列鎖;
//! - `_tx` 後綴、無 `lock_` 前綴(`course_seats_tx`、`session_seats_tx`)=
//!   交易內讀取,呼叫前必須已持對應列鎖——由 witness 型別擔保
//!   ([`CourseLocks`] 唯一建構點 `lock_courses_tx`、[`SessionLock`] 唯一建構點
//!   `lock_session_tx`,欄位皆私有),不再只靠 doc 前綴宣示。

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;

/// 課程層座位快照:容量與 active 報名數。
#[derive(Debug)]
pub struct CourseSeats {
    pub max_students: i32,
    pub active_count: i64,
}

impl CourseSeats {
    /// 滿班判定:`active_count >= max_students`(active == max 即滿)。
    pub fn is_full(&self) -> bool {
        self.active_count >= self.max_students as i64
    }
}

/// 見證 `lock_courses_tx` 已在呼叫端仍開啟的交易內,對這些 `courses` 列取得
/// `FOR UPDATE` 鎖——訂單鎖協定(`orders::locks`)的 courses 站。欄位私有,
/// 僅 `lock_courses_tx` 能建構(比照 [`SessionLock`])。
///
/// `ids` 是實際鎖到的列 id,升序去重——即取鎖順序(PostgreSQL 的 `uuid`
/// 依位元組排序,與 `Uuid` 的 `Ord` 一致)。
/// [`CourseLocks::in_lock_order`] 是「依鎖序走訪」的單一 owner,寫鎖端
/// (`enrolments::service::enrol_batch_from_purchase_tx`)不再自行排序。
#[derive(Debug)]
pub struct CourseLocks {
    ids: Vec<Uuid>,
}

impl CourseLocks {
    pub fn ids(&self) -> &[Uuid] {
        &self.ids
    }

    /// 把 `items` 排成本 witness 的鎖序(course id 升序)。witness 未涵蓋的
    /// id → `AppError::Internal`——寫一列沒鎖過的課程是協定 bug,不是業務拒絕。
    pub fn in_lock_order<T>(
        &self,
        items: Vec<T>,
        id: impl Fn(&T) -> Uuid,
    ) -> Result<Vec<T>, AppError> {
        if let Some(item) = items.iter().find(|item| !self.covers(id(item))) {
            return Err(not_covered(id(item)));
        }
        let mut items = items;
        items.sort_by_key(|item| id(item));
        Ok(items)
    }

    fn covers(&self, course_id: Uuid) -> bool {
        self.ids.binary_search(&course_id).is_ok()
    }
}

fn not_covered(course_id: Uuid) -> AppError {
    AppError::Internal(anyhow::anyhow!(
        "course {course_id} is not covered by CourseLocks"
    ))
}

/// 【courses 列 FOR UPDATE,升序】訂單鎖協定的 courses 站
/// (`orders::locks::acquire_checkout_locks`)。`ORDER BY id` 升序取鎖、
/// 回傳 [`CourseLocks`] witness;查無的 id 不在 witness 內。直接取
/// `FOR UPDATE`——之後 enrol 需要的就是這個強度,不再升級(兩個買同課程的
/// 人在此排隊,而非各持 SHARE 互等升級而死鎖)。
pub async fn lock_courses_tx(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<CourseLocks, sqlx::Error> {
    let ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM courses WHERE id = ANY($1) ORDER BY id FOR UPDATE",
    )
    .bind(ids)
    .fetch_all(&mut **tx)
    .await?;

    Ok(CourseLocks { ids })
}

/// 【`&CourseLocks` 見證課程列鎖已持有】enrol 用
/// (`enrolments::service::enrol_batch_from_purchase_tx`),取代原
/// `lock_course_seats_tx`。`course_id` 不在 witness 內 → `AppError::Internal`。
/// 鎖已由 `lock_courses_tx` 先取,COUNT 仍是取鎖**之後**的另一條 statement:
/// READ COMMITTED 下 snapshot 以 statement 為單位——取鎖(可能阻塞等待前一筆
/// 報名 commit)完成後才建立的 snapshot,才數得到對方剛寫入的列。**COUNT 不可
/// 併入取鎖那條 statement。** `None` = 課程不存在。
pub async fn course_seats_tx(
    tx: &mut Transaction<'_, Postgres>,
    locks: &CourseLocks,
    course_id: Uuid,
) -> Result<Option<CourseSeats>, AppError> {
    if !locks.covers(course_id) {
        return Err(not_covered(course_id));
    }

    let Some(max_students) =
        sqlx::query_scalar::<_, i32>("SELECT max_students FROM courses WHERE id = $1")
            .bind(course_id)
            .fetch_optional(&mut **tx)
            .await?
    else {
        return Ok(None);
    };

    let active_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM active_enrolments WHERE course_id = $1",
    )
    .bind(course_id)
    .fetch_one(&mut **tx)
    .await?;

    Ok(Some(CourseSeats {
        max_students,
        active_count,
    }))
}

/// 【無鎖快照】waitlist join 用(`waitlist::service::join_waitlist`)。
/// `&PgPool` 在型別層宣告不持鎖:waitlist join 與併發退課 race 而讀到
/// stale 的「已滿」count,對候補功能是可接受的 staleness(原 waitlist
/// repository 的文件化理由,隨遷移搬至此)。`None` = 課程不存在。
pub async fn course_seats(
    db: &PgPool,
    course_id: Uuid,
) -> Result<Option<CourseSeats>, sqlx::Error> {
    let Some(max_students) =
        sqlx::query_scalar::<_, i32>("SELECT max_students FROM courses WHERE id = $1")
            .bind(course_id)
            .fetch_optional(db)
            .await?
    else {
        return Ok(None);
    };

    let active_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM active_enrolments WHERE course_id = $1",
    )
    .bind(course_id)
    .fetch_one(db)
    .await?;

    Ok(Some(CourseSeats {
        max_students,
        active_count,
    }))
}

/// 場次層座位快照——實體座位模型(契約 §3.20 名額公式)的四個輸入。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionSeats {
    pub max_students: i32,
    pub active_count: i64,
    pub approved_leave_count: i64,
    pub makeup_count: i64,
}

impl SessionSeats {
    /// 目標場次剩餘座位 = `max_students - active + 該場次核准請假 - 已補進
    /// 該場次的補課`——請假釋出座位、補課佔用座位(controller ruling
    /// 2026-07-06,契約 §3.20)。
    pub fn remaining(&self) -> i64 {
        self.max_students as i64 - self.active_count + self.approved_leave_count
            - self.makeup_count
    }
}

/// 見證 `lock_session_tx` 已在呼叫端仍開啟的交易內,對此場次列取得
/// `FOR UPDATE` 鎖——把「呼叫 [`session_seats_tx`] 前必須已持場次列鎖」這條
/// 原本只靠該函式 doc 前綴宣示的呼叫順序 invariant,收進型別系統:
/// [`session_seats_tx`] 改收 `&SessionLock`,不再收裸 `session_id`。
///
/// 欄位私有,僅 `lock_session_tx` 能建構;唯讀存取 `session_id()`/
/// `course_id()`。兩個欄位取自同一列 `SELECT`,故持有此值同時擔保兩件事:
/// 「已持有該場次列鎖」與「session↔course 為同一列讀出的配對」——原設計
/// `session_seats_tx` 另外收一個呼叫端自由傳入的 `course_id` 參數,傳錯
/// 配對沒有型別防護。
#[derive(Debug, Clone)]
pub struct SessionLock {
    session_id: Uuid,
    course_id: Uuid,
}

impl SessionLock {
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn course_id(&self) -> Uuid {
        self.course_id
    }
}

/// 【course_sessions 列 FOR UPDATE】makeup 前置鎖(自
/// `leave::repository::lock_session_tx` 原樣搬入):在 [`session_seats_tx`]
/// 計數前鎖住目標場次列,序列化**不同**假單搶同一場次名額(controller
/// ruling 2026-07-06)——假單自身的列鎖只擋同一張假單的重複預約。回傳
/// [`SessionLock`] witness,一併帶出鎖定列自身的 `course_id`,供
/// [`session_seats_tx`] 使用。`None` = 場次不存在。
pub async fn lock_session_tx(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
) -> Result<Option<SessionLock>, sqlx::Error> {
    let row = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT id, course_id FROM course_sessions WHERE id = $1 FOR UPDATE",
    )
    .bind(session_id)
    .fetch_optional(&mut **tx)
    .await?;

    Ok(row.map(|(session_id, course_id)| SessionLock {
        session_id,
        course_id,
    }))
}

/// 【`&SessionLock` 見證場次列鎖已持有】四數單查詢(自
/// `leave::repository::find_makeup_capacity_tx` 原樣搬入),把課程的
/// `max_students` 與三個 correlated count 一次讀齊。`course_id`/
/// `target_session_id` 皆取自 `lock`(`lock.course_id()`/
/// `lock.session_id()`),不再是呼叫端另傳的裸參數——見 [`SessionLock`]。
/// 兩個請假/補課計數皆只計 enrolment 仍為 `active` 者(controller ruling
/// 2026-07-06):請假後退課的人不釋出幽靈座位,補課後退課的人不繼續佔位。
/// `None` = 課程不存在。
pub async fn session_seats_tx(
    tx: &mut Transaction<'_, Postgres>,
    lock: &SessionLock,
) -> Result<Option<SessionSeats>, sqlx::Error> {
    sqlx::query_as::<_, SessionSeats>(
        "SELECT c.max_students, \
                (SELECT COUNT(*) FROM active_enrolments \
                  WHERE course_id = c.id) AS active_count, \
                (SELECT COUNT(*) FROM leave_requests lr \
                  JOIN active_enrolments e ON e.id = lr.enrolment_id \
                  WHERE lr.session_id = $2 AND lr.status = 'approved'::leave_status) AS approved_leave_count, \
                (SELECT COUNT(*) FROM leave_requests lr \
                  JOIN active_enrolments e ON e.id = lr.enrolment_id \
                  WHERE lr.makeup_session_id = $2) AS makeup_count \
         FROM courses c WHERE c.id = $1",
    )
    .bind(lock.course_id())
    .bind(lock.session_id())
    .fetch_optional(&mut **tx)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- CourseSeats::is_full 邊界 ---

    #[test]
    fn course_full_when_active_equals_max() {
        // service_enrolments::enrol_full_course_returns_course_is_full_conflict
        // / service_waitlist::join_full_course_creates_waiting_entry:
        // active == max 即滿(enrol 409 "course is full";waitlist 放行)。
        let seats = CourseSeats {
            max_students: 2,
            active_count: 2,
        };
        assert!(seats.is_full());
    }

    #[test]
    fn course_not_full_at_max_minus_one() {
        // service_waitlist::join_course_not_full_returns_conflict:
        // active == max - 1 仍未滿(waitlist 409 "course is not full";
        // enrol 放行搶最後一席)。
        let seats = CourseSeats {
            max_students: 2,
            active_count: 1,
        };
        assert!(!seats.is_full());
    }

    // --- CourseLocks::in_lock_order ---

    fn ids() -> (Uuid, Uuid, Uuid) {
        let mut v = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
        v.sort();
        (v[0], v[1], v[2])
    }

    #[test]
    fn in_lock_order_sorts_items_ascending_by_id() {
        let (low, mid, high) = ids();
        let locks = CourseLocks {
            ids: vec![low, mid, high],
        };
        let ordered = locks
            .in_lock_order(vec![high, low, mid], |id| *id)
            .expect("all covered");
        assert_eq!(ordered, vec![low, mid, high]);
    }

    #[test]
    fn in_lock_order_rejects_an_id_outside_the_witness_as_internal() {
        let (low, mid, high) = ids();
        let locks = CourseLocks {
            ids: vec![low, high],
        };
        let err = locks
            .in_lock_order(vec![low, mid], |id| *id)
            .expect_err("mid was never locked");
        assert!(matches!(err, AppError::Internal(_)), "got {err:?}");
    }

    // --- SessionSeats::remaining 表格(取自既有測試註解與契約 §3.20 範例)---

    #[test]
    fn remaining_table() {
        // (max_students, active, approved_leave, makeup) → remaining
        let cases: [(i32, i64, i64, i64, i64); 4] = [
            // service_leave::concurrent_makeup_different_requests_last_seat_only_one_wins:
            // 3 - 2 + 0 - 0 = 1(恰好最後一席)
            (3, 2, 0, 0, 1),
            // 同測試 loser 在 winner commit 後重數:3 - 2 + 0 - 1 = 0
            // (409「該場次名額已滿」)
            (3, 2, 0, 1, 0),
            // 契約 §3.20 範例一:滿班但該場次 3 人核准請假 →
            // 10 - 10 + 3 - 0 = 3,可補課
            (10, 10, 3, 0, 3),
            // 契約 §3.20 範例二:10 - 8 + 0 - 2 = 0 → 409
            (10, 8, 0, 2, 0),
        ];
        for (max_students, active_count, approved_leave_count, makeup_count, expected) in cases {
            let seats = SessionSeats {
                max_students,
                active_count,
                approved_leave_count,
                makeup_count,
            };
            assert_eq!(
                seats.remaining(),
                expected,
                "({max_students},{active_count},{approved_leave_count},{makeup_count})"
            );
        }
    }
}
