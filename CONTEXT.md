# Dream Fly

Dream Fly 工作室預約與商務後端的領域語言。於 notifications 接縫架構審查(2026-06)首次建立,後續解析的術語陸續補入。

## Language

**Notification**:
持久化到 `notifications` 表、只顯示給單一使用者的 in-app 訊息,在某個領域動作 commit 之後以 best-effort 寫入;絕不阻擋或回滾觸發它的動作。
_Avoid_: alert, push(本系統無外部推播通道), message

**Event**:
描述業務事實(如 order created、user registered)的 outbox/Kafka 紀錄,在業務交易內寫入,供 audit 與外部整合。非使用者可見——Event 不是 Notification。
_Avoid_: notification, message

**工作室時鐘(Studio Clock)**:
牆鐘語意的單一歸屬,`utils::studio_clock`,契約 §3.18 裁決 2。

**課程教練所有權(Course-Coach Ownership)**:
`coaches::service::resolve/require_course_coach`;三態政策=所有權 gate 403 / 範圍列表空集合 / 儀表板 404。

**訂單定價(Order Pricing)**:
`orders::pricing::price → PricingOutcome`,純函式,交易編排留 checkout。

**營收狀態集(Revenue Statuses)**:
`orders::model::REVENUE_STATUSES`;products 的「paid-class」售出計數(`find_sold_counts`)直接消費同一常數,不再是另一份攣生清單。

**留存(Retention)**:
`GET /reports/admin` 的 `retention` 段——近 6 個 studio 月的出席 cohort:會員某月有 ≥1 筆 `present` 出勤記錄即該月「活躍」;首次活躍月計入 `new_count`,此後再活躍計入 `returning_count`;`rate` = 上月與本月活躍會員的交集人數 ÷ 上月活躍人數,上月為空集合時為 `null`(undefined,非 0)。量的是「有沒有回來上課」,與 `subscriptions` 的續買/續卡(entitlement 續期,見 ADR-0003)是不同概念。
_Avoid_: 續訂率、回訪率、churn(本系統只表達留存,不另計流失率)

**漏斗(Funnel)**:
`GET /reports/admin` 的 `funnel` 段——誠實兩段、近 90 個 studio 天:`trial_inquiries`(試上預約計數,見下方「試上預約」條)→ `new_enrolments`(新報名數,不含已取消)。後端只給這兩個原始計數,不造中間段、不算轉換率(前端如需百分比自行以兩數相除)。
_Avoid_: 轉換率(後端無此欄位)、行銷漏斗(此處是資料聚合,非行銷全流程模型)

**場租(Venue Rental)**:
`time_slots`(可預約場地時段)與 `bookings`(使用者對某時段的預訂)這組表所代表的營收來源——與 `courses`/`enrolments`(報名)是完全不同的產品線。`bookings.price_cents` 是建立當下從 `time_slots.price_cents` 快照的金額(見 migration `20260708000006`),之後時段改價不回溯影響既有預訂;取消預訂只把 `status` 改為 `cancelled`,`price_cents` 維持原值不清零——沒有退款欄位或沖銷分錄,「計收與否」單純由 report 聚合端的 `status ∈ (confirmed, completed)` 過濾決定。計收月份歸屬「場地使用日」(`time_slots` 的時段日期),不是下訂日。本系統只有單一場館,`venues` 表沒有分校/campus 維度(見 ADR-0004 的 `campusRevenue` 移除決策)。
_Avoid_: 包場、分校營收/campusRevenue(不存在此維度)、訂場(那是動作,這裡指的是計收模型與資料表)

**試上預約(Trial Inquiry)**:
`contact_inquiries` 表 `inquiry_type = 'trial'` 的列——試上(trial class)預約走既有的洽詢資料表,不是獨立的預約表,結構化欄位(類別/學員年齡/偏好日期時段/家長姓名電話/學員姓名/備註)存進 `metadata` JSONB,後端不逐欄驗證。與「場租(Venue Rental)」的 `bookings` 是兩張完全不同的表,不要混為一談——前者是「想試上一堂課」的意向登記,後者是「已確定要用某個場地時段」的預訂。
_Avoid_: 試聽(啦啦/體操課程用語是「上課」不是「聽課」)、trial booking(容易被誤會是 `bookings` 表的一筆列)、試聽預約

**系統設定(Settings)**:
`settings` 表——admin 可讀寫的全域 key-value 設定(`key` 自由字串、`value` 任意合法 JSON,不逐欄驗證),供 admin 桌面「系統設定」頁與 mobile-admin 設定畫面使用。與另外兩個「設定」概念不同:`users.preferences` 是單一會員自己的偏好(JSONB,per-user,見 `PATCH /users/me`);`AppConfig`(`config/*.toml` + `APP__*` 環境變數)是伺服器啟動期設定,不是這張執行期可由 admin 透過 API 讀寫的資料表。
_Avoid_: 偏好設定(那是 `users.preferences`,per-user 不是全域)、組態/config(那是 `AppConfig`,啟動期而非這張表)、設定檔(這是資料庫表,不是檔案)

**場次狀態(Session Status)**:
`sessions::model::SessionStatus::derive`;依 `studio_clock::has_started`/`has_ended`([start, end) 閉開)即時推導的三態(`upcoming`/`ongoing`/`done`),讀取時計算、不落地儲存,`course_sessions` 表仍無 status 欄。
_Avoid_: state, live/done

**座位(Seats)**:
「課程還有沒有位子」invariant 的單一 owner:`courses::seats`——課程層 `CourseSeats::is_full`(enrol 持鎖 `lock_course_seats_tx`、waitlist 無鎖 `course_seats`)與場次層 `SessionSeats::remaining`(實體座位模型 `max - active + leave - makeup`,契約 §3.20)。鎖策略由參數型別宣告:`&PgPool` = 無鎖快照、`&mut Transaction` + `lock_` 前綴 = `FOR UPDATE` 列鎖;`courses`/`sessions` repository 的 `enrolled_count` 是顯示用 inline 拷貝,非決策端。
_Avoid_: capacity, quota
