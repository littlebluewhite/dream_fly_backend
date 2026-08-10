# Dream Fly

Dream Fly 工作室預約與商務後端的領域語言。於 notifications 接縫架構審查(2026-06)首次建立,後續解析的術語陸續補入。

## Language

**Notification**:
持久化到 `notifications` 表、只顯示給單一使用者的 in-app 訊息,在某個領域動作 commit 之後以 best-effort 寫入;絕不阻擋或回滾觸發它的動作。「commit 之後」由 `PendingNotification`(`#[must_use]`,`.deliver(db)` 唯一 IO 入口)提醒,時機本身仍是位置慣例——是否收進型別系統已裁決不做,封存於 ADR-0009,重開條件見該 ADR。目前所有交付站點皆遵循「tx 內完成領域寫入 → commit → deliver」標準形,無例外站點。
_Avoid_: alert, push(本系統無外部推播通道), message

**Event**:
描述業務事實(如 order created、user registered)的 outbox/Kafka 紀錄,在業務交易內寫入,供 audit 與外部整合。非使用者可見——Event 不是 Notification。
_Avoid_: notification, message

**工作室時鐘(Studio Clock)**:
牆鐘語意的單一歸屬,`utils::studio_clock`,契約 §3.18 裁決 2。

**課程教練所有權(Course-Coach Ownership)**:
`coaches::service::resolve/require_course_coach`;三態政策=所有權 gate 403 / 範圍列表空集合 / 儀表板 404。所有權 gate 有**第二形狀**——教練—學員**關係** gate(契約 §3.22「教過此生,active 或 cancelled 皆算」),刻意 inline 於 `certificates::service::create_certificate`(src/modules/certificates/service.rs:80-90:`coaches::service::resolve` + `user_has_enrolment_with_coach` EXISTS 查詢),**不歸戶 coaches 模組**——單一呼叫端,依 ADR-0005 判準,為它建姊妹 helper 是淺模組;出現第二個「教練發給自己學員」類端點時再歸戶。對照:單課 gate `require_course_coach`(coaches/service.rs:44-64)已歸戶,`create_report_card`(certificates/service.rs:29)是其消費端。
_Avoid_: 把這條關係 gate 與單課 gate `require_course_coach` 混同

**訂單定價(Order Pricing)**:
`orders::pricing::price → PricingOutcome`,純函式,交易編排留 checkout。

**行計畫(Line Fulfilment)**:
`orders::fulfilment::plan → FulfilmentPlan`,純函式(pricing 的姊妹),對 `CartItemType` 一處 exhaustive match(無 `_` arm——新變體 = 此處編譯錯誤)把結帳購物車切成商品行(`ProductFulfilment`:reserve 庫存 + grant 訂閱)與課程 id(enrol),取代 checkout 原本兩次互斥 `.filter(matches!)`。**排序不在此**:寫入保留序(write-reservation order;type-major、id-minor)由拿寫鎖的 owner 各自負責——商品 `products::service::reserve_stock_tx`、課程 `enrolments::service::enrol_batch_from_purchase_tx`——plan() 只保留輸入序,不排序(一個 invariant 兩個 owner 比沒有 owner 更糟)。`orders::fulfilment::order_lines → Vec<OrderLine>` 是 `plan()` 的姊妹純函式:把同一份結帳快照(連同 `reserve_stock_tx` 回傳的 `reserved` map)攤平成具名 `OrderLine`,`stock_decremented` 的推導規則(product 行且 `reserved[pid].stock.is_some()` 才算扣過;pid 不在 reserved、或 course 行,恆 false)單一歸戶於此,取代原本埋在 `service::checkout` 建構六元組的匿名 closure 裡、不可單測的判斷。
_Avoid_: 分派/dispatch(那是動作,這裡指切出來的計畫結構)、鎖序/排序(不在此純函式,歸拿寫鎖的深函式)

**營收狀態集(Revenue Statuses)**:
謂詞 owner 是 `OrderStatus::is_revenue`(窮盡 match),`REVENUE_STATUSES` 是 SQL 綁定攣生面(products/reports 綁點不變),兩者由交叉測試錨定。

**留存(Retention)**:
`GET /reports/admin` 的 `retention` 段——近 6 個 studio 月的出席 cohort:會員某月有 ≥1 筆 `present` 出勤記錄即該月「活躍」;首次活躍月計入 `new_count`,此後再活躍計入 `returning_count`;`rate` = 上月與本月活躍會員的交集人數 ÷ 上月活躍人數,上月為空集合時為 `null`(undefined,非 0)。量的是「有沒有回來上課」,與 `subscriptions` 的續買/續卡(entitlement 續期,見 ADR-0003)是不同概念。
_Avoid_: 續訂率、回訪率、churn(本系統只表達留存,不另計流失率)

**漏斗(Funnel)**:
`GET /reports/admin` 的 `funnel` 段——誠實兩段、近 90 個 studio 天:`trial_inquiries`(試上預約計數,見下方「試上預約」條)→ `new_enrolments`(新報名數,不含已取消)。後端只給這兩個原始計數,不造中間段、不算轉換率(前端如需百分比自行以兩數相除)。
_Avoid_: 轉換率(後端無此欄位)、行銷漏斗(此處是資料聚合,非行銷全流程模型)

**場租(Venue Rental)**:
`time_slots`(可預約場地時段)與 `bookings`(使用者對某時段的預訂)這組表所代表的營收來源——與 `courses`/`enrolments`(報名)是完全不同的產品線。`bookings.price_cents` 是建立當下從 `time_slots.price_cents` 快照的金額(見 migration `20260708000006`),之後時段改價不回溯影響既有預訂;取消預訂只把 `status` 改為 `cancelled`,`price_cents` 維持原值不清零——沒有退款欄位或沖銷分錄,「計收與否」單純由 report 聚合端的 `status ∈ (confirmed, completed)` 過濾決定。計收月份歸屬「場地使用日」(`time_slots` 的時段日期),不是下訂日。本系統只有單一場館,`venues` 表沒有分校/campus 維度(見 ADR-0004 的 `campusRevenue` 移除決策)。計收狀態集的 owner 是 `bookings::model::VENUE_REVENUE_STATUSES`,reports 直接消費同一常數,不再是消費端自持的攣生清單。
_Avoid_: 包場、分校營收/campusRevenue(不存在此維度)、訂場(那是動作,這裡指的是計收模型與資料表)

**場租佔位(Venue-Rental Occupancy)**:
`BookingStatus::occupies_seat` 是「這筆 booking 佔不佔一個座位」謂詞的單一 owner;`time_slots.booked` 是它的反正規化讀取快取。協定 owner 是 `bookings::occupancy`——佔位變化(booking 列 insert/cancel)與 `booked` 增減成對出現的唯一地點,四條 SQL 集中同檔;`schedule` 不再持有 increment/decrement。未來任何 `BookingStatus` 寫入者(如狀態轉移端點)必須經過此模組。seed(`src/bin/seed.rs`)僅消費同一謂詞——依 `occupies_seat` 決定要不要寫一筆 booking、`booked` 直接算好帶入 INSERT——不經過 occupancy 協定(seed 從未寫入 `Pending`,這個變體目前是零寫入者:runtime 佔位 insert 寫字面值 `confirmed`、取消轉 `cancelled`,seed 只用 `Completed`/`Cancelled`/`NoShow`)。
_Avoid_: 把課程「座位(Seats)」詞條與此混為一談(不同產品線)、在 `bookings::occupancy` 之外直寫 `bookings.status` 或增減 `time_slots.booked`

**給點(Point Grant)**:
`points::service::apply_delta_tx` 是「使用者點數餘額變動且同時落一列 `point_ledger`」的唯一路徑——`users.points_balance` 不接受業務端直寫(runtime 由此函式交易內的 UPDATE + INSERT 協定維護、seed 消費同一 owner:`upsert_user`/`upsert_seed_member` 在同一交易內以 `LedgerDelta::admin_adjust` 呼叫,比照 `occupies_seat` 的 runtime/seed 共用模式)。`apply_delta_tx` 現在收 `points::model::LedgerDelta` 而非裸 `(delta, reason, order_id)`——六個建構子與六個 `PointReason` 變體一對一同名,在型別層把「一個 reason ⇒ 固定正負號」(契約 §1.6)與「checkout/refund 類 reason 恆帶 `order_id`」(`uniq_point_ledger_refund_once` 依賴的前提,ADR-0007 決策 7)這兩條原本只能靠呼叫端自律的散文前提焊進建構子;`admin_adjust` 是唯一帶符號的逃生口,語意即「正負皆可的人工調整」,不是前五者遺漏的第六種固定符號。fixtures 的 `set_points_balance`(`tests/common/fixtures.rs`)是記錄在案的測試 harness bypass——直寫 `points_balance` 略過 ledger,只用於測試佈局階段擺出起始餘額,不是業務路徑,不受此 owner 約束。此 bypass 曾有三處未經記錄的手抄孿生——`tests/http_users.rs`(×2)、`tests/service_users.rs`(×1)各自手寫同一句 `UPDATE users SET points_balance...`,未呼叫共用 fixture——現已全數收編改呼叫 `set_points_balance`,「唯一記錄在案的 bypass 只有這一處」的宣稱至此才真正全面成立(先前這三處手抄孿生是未記錄在案的第二類 bypass,與本詞條的宣稱有落差)。
_Avoid_: 把 `set_points_balance` 當成業務可用的授點手段(它是測試專用的佈局捷徑)、把 `apply_delta_tx`「不 commit」誤讀成「不寫 ledger」(它一定寫 ledger,只是不 commit 交易,由呼叫端負責)、在呼叫端手寫 delta 負號或手傳 reason/order_id(繞過 `LedgerDelta` 建構子等於重新打開型別已經收斂掉的自由度)

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
「課程還有沒有位子」invariant 的單一 owner:`courses::seats`——課程層 `CourseSeats::is_full`(enrol 持鎖 `lock_course_seats_tx`、waitlist 無鎖 `course_seats`)與場次層 `SessionSeats::remaining`(實體座位模型 `max - active + leave - makeup`,契約 §3.20)。鎖策略由參數型別宣告:`&PgPool` = 無鎖快照、`&mut Transaction` + `lock_` 前綴 = `FOR UPDATE` 列鎖;`courses`/`sessions` repository 的 `enrolled_count` 是顯示用 inline 拷貝,拷貝的是對 `active_enrolments` view 的引用——謂詞單源已下沉至該 view,非決策端。場次層「先鎖列、再讀座位」的呼叫順序也已收進型別系統,比照「場次物化」詞條的 `MaterializedRange` 寫法:`lock_session_tx` 回傳 `SessionLock` witness(欄位私有、僅該函式能建構,唯讀存取 `session_id()`/`course_id()`),`session_seats_tx` 改收 `&SessionLock`——原本呼叫端另傳的 `course_id` 參數已不存在,「course_id 與被鎖場次不配對」整類錯誤隨之消失。
_Avoid_: capacity, quota

**出席口徑(Countable Attendance)**:
`countable_attendance` view(migration `20260710000001`)——出席聚合報表口徑的單一 owner:view 成員資格(`status IN (present, absent)`)= 計入分母、leave 排除,顯式布林欄 `is_present` = 分子,view 內 `NOT is_present` 恆等於 absent。`reports::repository` 的 7 條聚合查詢(`kpis`/`coach_reports`/`attendance_distribution`/`retention`/`weekday_load`/`coach_attendance_in_range`/`member_attendance`)與 `enrolments::repository::find_by_user_with_course`(`GET /enrolments/me` 的 attended/total 統計)皆換底至此 view,不再各自手寫 `status` 判斷;`coach_today_and_pending` 的 `pending_attendance`(任一狀態 EXISTS)是另一個口徑,故意不進這張 view。
_Avoid_: 出勤率(那是 service 算出的 rate,不是這個口徑本身)、attendance_records(那是底表,口徑 owner 是 view 不是它)

**點名計畫(Marking Plan)**:
`attendance::marking::{parse, plan}`,純函式(`orders::pricing`/`orders::fulfilment` 的姊妹),`PUT /sessions/{id}/attendance` 批次點名的 parse(status 字串轉 `AttendanceStatus`,無效值 422)+ 成員資格校驗(requested enrolment id 集合與呼叫端已查得的 valid 集合相等判斷,不等 422)兩段驗證的純核 owner;`parse` 必須先於 enrolment DB 查詢執行——現狀無效 status 不觸發查詢,錯誤優先序若反過來,DB 故障時 422 會變 500。DB 查詢(`repository::find_active_enrolment_ids_in`)、空批次跳過查詢的 guard、寫入交易迴圈皆留在 `service::bulk_upsert_attendance`。
_Avoid_: 與「出席口徑(Countable Attendance)」混同——本核決定的是「這批點名寫入是否合法」,出席口徑決定的是「哪些既有紀錄計入出勤統計分母/分子」,寫入前驗證與讀取聚合是不同層。

**請假(Leave)**:
`leave_requests` 表——會員對已報名課程某一特定場次的請假申請,由該課教練或 admin 審核(契約 §3.20)。核准是**投影**動作:`leave::service::decide_leave_request` 同一交易內雙寫——更新假單為 `approved`,並 `attendance::repository::upsert_attendance_tx` 把該場次出勤投影成 `leave` 列(`marked_by` = 核准者)。**核准恆勝**:晚核准是營運常態,核准覆寫既有 `present`/`absent` 是合法裁決,`decide` 無時間閘、簽章不動;反方向由守衛擋下——**點名不可覆寫已核准請假**,雙層防護:`attendance::marking::plan` 第三輸入(該場次已核准請假成員集合,`find_approved_leave_enrolment_ids_tx` 於寫入 tx 內查得)做整批 422 pre-check,`upsert_attendance_tx` 的 `ON CONFLICT … WHERE` 三分支守衛關閉 pre-check 的 TOCTOU 殘餘窗。口頭請假(`PUT "leave"` 無核准單)不受守衛約束、可自由覆寫。座位面:請假釋出座位、補課佔座(見「座位(Seats)」詞條與 §3.20 名額公式——只讀 `leave_requests`,不讀投影出的出勤列)。兩個 known gap(approved 無撤銷的臨時出席死角、guard `EXISTS` 快照落後的極窄並發窗,可重點 `leave` 復原)與甲案(derive)落選理由見 ADR-0008。
_Avoid_: 把「請假投影出的 `leave` 出勤列」(有核准單背書、受守衛保護不可被點名覆寫)與「口頭請假的 `leave` 出勤列」(無單、可自由覆寫)當成同一種;把「核准恆勝」誤讀成雙向覆寫(只有 decide→attendance 恆勝,點名→已核准 leave 恆敗,方向刻意不對稱)。

**場次物化(Session Materialization)**:
「先物化、再讀取」呼叫順序 invariant 的單一 owner:`sessions::repository::materialize_range` 回傳 `MaterializedRange` witness(欄位私有,僅該函式能建構;唯讀存取 `course_ids()`/`from_date()`/`to_date()`),兩個 early-return 路徑也回傳 witness。讀取端(`sessions::find_sessions_in`、`reports::venue_usage`/`upcoming_session_count`)改收 `&MaterializedRange`,不再各自靠 doc 前置條件維繫呼叫順序。witness 只擔保「此範圍已物化」,**不**擔保每個讀取端都按 `course_ids` 過濾——`venue_usage` 只用其日期窗(全場館聚合由查詢本身表達),`find_sessions_in`/`upcoming_session_count` 才綁 `course_ids`。日期軸再分一支:`sessions::find_today_sessions_in`/`reports::coach_today_and_pending` 額外要求單日(`TodaySessionRow` 無日期欄、「今天」本身無多日語意),這個前提由姊妹型別 `MaterializedDay` 在建構點一次成立——`materialize_day` 是唯一建構點,內部呼叫 `materialize_range(db, ids, date, date)` 重用同一套冪等/早退邏輯,兩個消費端改收 `&MaterializedDay`,原本各自的 `mat.from_date() == mat.to_date()` debug_assert 已退役。
_Avoid_: 把 witness 當作 course 範圍過濾的保證(它只保證「已物化」)、materialize_range 呼叫順序仍是文件慣例(已收進型別系統)

**有效報名(Active Enrolments)**:
`active_enrolments` view(migration `20260711000001`)——「目前占用座位的有效報名」口徑的單一 owner:`WHERE status = 'active'` 篩選下沉至此,~22 個 READ 站點(`courses::seats` 的座位 COUNT、`courses`/`sessions` repository 的 `enrolled_count` 顯示子查詢、`attendance`/`leave` 的 active-enrolment 查找、`reports` 的會員/課程/教練統計,橫跨 7 個 module)換底至此 view,不再各自手寫 `status = 'active'` 判斷。刻意排除兩類站點,不下沉進本 view:(1) reports 的 3 個「報名事件」站點(`kpis` 的 new_enrolments_this/_last、`funnel` 的 new_enrolments)——量的是「這個月發生了幾次報名動作」,是事件流口徑而非占位口徑,即使二元 enum 下今天結果等價;(2) `enrolments` 寫側的狀態轉移語句(INSERT/UPDATE)——寫側不能讀自己正在寫的 view。`enrolments::repository` 的 `find_by_id_tx`/`find_owner`,以及各處「歷史列表」JOIN(/me 報名列表、certificates、leave、reports 的 `countable_attendance` JOIN 等)同樣刻意不換底,因為它們需要看到 cancelled 列(double-cancel 409 判斷、出席/證書/請假歷史等皆賴此)。
_Avoid_: 現役報名、未取消報名

**候補(Waitlist)**:
`waitlist_entries` 表——課程滿班時的**諮詢名單(advisory list)**,依加入序呈現(`GET /waitlist?course_id=`,admin only,舊到新,見 `waitlist::service::list_for_course`)。名額釋出(取消報名)不觸發任何自動遞補或通知;遞補一律由 admin 依名單順序人工聯絡,由候補者自行完成結帳——報名唯一入口是結帳(ADR-0002),系統不存在「保留座位給候補第一名」的模型(見 ADR-0006)。repo 現行「queue order」用語(同一支 doc comment)指的是這份名單的**列序**,與本詞條「不自動化」的定案並不衝突——列序本身仍有意義(人工聯絡依序進行),只是不會被系統自動出隊消費。「等待中(waiting)」這個口徑本身現由 `waiting_entries` view(migration `20260803000002`,`WHERE status = 'waiting'`)單一收斂——10 個原本各自手寫這個篩選的 READ 站點(`courses::repository` 的 7 個 `waitlist_count` 顯示子查詢、`waitlist::repository` 的 `exists_waiting`/`find_by_course_waiting`、`reports::repository::course_reports`)換底至此。寫側狀態轉移(`insert`/`cancel_if_waiting_tx`)與無 waiting 謂詞的讀側(`find_by_id_tx` 取消路徑用的 `FOR UPDATE` 行鎖、`find_by_user_with_course` 的 `/waitlist/me` 歷史列表)刻意不下沉——前者不能讀自己正在寫的 view,後者語意上需要看到非 waiting 列。此 view 純粹是讀取收斂,不帶任何寫入、觸發或狀態轉移邏輯,不引入本詞條上述已定案排除的自動遞補。
_Avoid_: 遞補佇列(「佇列」暗示自動出隊消費,與人工遞補定案相悖;僅避自動化暗示,不避「依序」語意本身)、waiting list promotion(`Promotion` 在本系統另指 `notifications`/`posts` 的行銷促銷分類,語意不同)

**時鐘 seam(Clock Seam)**:
`utils::clock`——handler 在請求開始經 `state.clock.now()` 取樣一次,以 `now: DateTime<Utc>` 參數往下傳入 service;牆鐘語意的 service 不再自行呼叫 `Utc::now()`；非牆鐘語意站點(auth token 效期、posts 發佈時戳)為記錄在案的 carve-out。`subscriptions` 的 entitlement 到期計算原本也列在這份 carve-out 名單裡,現已收斂進 seam——`entitlement::plan`(見「權益授予」詞條)改收 `now` 參數,不再自行讀鐘;這個 `now` 經 `grant_from_purchase_tx` 透傳,溯源仍是 checkout 自己的 handler-取樣值,故從 carve-out 名單移除。`utils::studio_clock` 的純函式(`today`/`has_started`/…)本身不變,一樣收 `now` 參數——這層只是把「由誰取樣」從 service 上移到 handler 一層。
_Avoid_: 把 `studio_clock` 也算進這層 seam(它的函式簽章未變,只是呼叫端現在傳的是 handler 取樣值)

**週課表(Weekly Schedule)**:
`course_schedule_slots` 表(型別 + CRUD)的單一 owner 是 `courses`(`courses::model::CourseScheduleSlot`、`courses::repository::find_slots_by_course`/`replace_slots_tx`),courses 的 create/update/get 是唯一消費端。`sessions::repository` 以原生 SQL 直接讀這張表做物化(`materialize_range`)、今日場次(`find_today_sessions_in`)、我的課表(`find_my_weekly_schedule`)——三者皆不碰這組 Rust 型別,是記錄在案的跨模組讀表慣例(與 `find_all_course_ids` 直接讀 `courses` 表同款)。
_Avoid_: 把 `time_slots`(場租,見『場租(Venue Rental)』詞條)也稱作 schedule——兩者是完全不同的表。

**時段狀態(Slot Status)**:
`schedule::model::SlotStatus::derive`;依 `booked`/`capacity`/`is_closed` 純函式即時推導的四態(`available`/`limited`/`full`/`closed`),讀取時計算、不落地儲存——比照「場次狀態」詞條的裁決,`time_slots` 表已無 `status` 欄(migration `20260717000001` 收掉欄位與背後的 `slot_status` enum 型別)。`is_closed` 是管理意圖旗標(`PATCH /schedule/slots/{id}`,admin only),優先於 booked/capacity 的判斷;gate 於 `bookings::occupancy::occupy_slot_tx` 的 WHERE 子句(`AND is_closed = false`)——closed 時段無法再被新預約增量,但既有預約仍可正常取消(`cancel_and_release_tx` 不設 gate)。
_Avoid_: 把 `bookings.status`(`BookingStatus`,`confirmed`/`cancelled`/`completed`/`no_show`,仍落地儲存的預約狀態機)與本詞條混為一談——兩者是不同表、不同語意的「狀態」。

**退款(Refund)**:
訂單從計入營收的狀態(`OrderStatus::is_revenue`——paid/processing/completed)轉往終態 cancelled 或 refunded 時的補償語意,`orders::service::update_order_status` 內的私有步驟 `compensate_order_artifacts_tx`,由 `orders::refund::compensation_required` 判斷是否觸發。**Cancelled 與 Refunded 是同一補償語意的兩個終態標籤,不是兩種不同的補償**。補償一律讀「結帳當下的痕跡」而非現況推測——`order_items.stock_decremented` 快照決定要不要回補庫存、`point_ledger` 的 `checkout_earn`/`checkout_redeem` 實錄決定點數反轉幅度(方向依序 `refund_restore` 先、`refund_clawback` 後,契約 §1.6),報名/訂閱依 `order_id` 整批取消。是**整單**語意:不論已核銷/使用多少,一律全額反轉,不按使用比例折算。餘額不足時整筆回滾(409「點數不足」),不 clamp——修復迴路是 admin 補點端點(`POST /points/adjustments`,§3.14)。十個決策點的完整論證見 ADR-0007。
_Avoid_: 沖銷(那是點數反轉裡「收回已賺點數」單一方向的動作 `refund_clawback`,不是整套補償語意的統稱)、退貨(本系統無實體物流退貨流程,這裡指的是撤銷結帳建立的內部副作用——庫存/點數/entitlement,不涉及商品寄還)、刪單(`orders`/`order_items` 從不刪除,退款是狀態機轉移到終態,原始下單紀錄永久可查)

**對話配對(Conversation Pairing)**:
`messages::pairing::resolve_pair → (member_id, coach_id)`,純函式(`orders::pricing`/`orders::fulfilment` 的姊妹),`POST /conversations` 的 member/coach 配對與自我拒斥純核 owner。自我拒斥在 `service::resolve_member_coach` 與純核各查一次——service 端先查、擋在 DB round trip 之前(自我請求必 422、不多打一次 DB),純核內重複同一檢查只是讓函式自洽,不是兩套優先序。雙角色(coach 且 member)caller 恆落 coach 側(分支順序:caller-is-coach 先判),故雙角色×雙角色的 A→B 與 B→A 是鏡像對(`(B,A)`/`(A,B)`)而非同一對;兩方向仍共享同一 conversation,由 DB 端無序 unique index(`LEAST`/`GREATEST`)保證,非本核職責。唯一 DB 依賴(`permissions_repository::find_role_names_by_user` 取對方角色)與 get-or-create/unique-violation race 收斂留在 `service`。
_Avoid_: 與 participants 授權(`authorize_participant`,`GET/POST .../messages` 等端點「呼叫者是否為此對話成員」的檢查)混同——那是既存對話的存取控制,配對是「建立/取得哪一個對話」的角色判斷,發生在對話是否存在確定之前。

**授權閘門(Authorization Gate)**:
route 層單點角色檢查家族——`middleware::require_admin`/`require_staff`/`require_coach`,取代逐 handler 首行 `auth.require_role`/`require_any_role` 儀式。`startup.rs` 依角色層級分三個 router 區塊(`admin_api`/`staff_api`/`coach_api`),各自 merge 對應模組的 `admin_router()`/`staff_router()`/`coach_router()` 後掛一個 `route_layer`。三個閘門共用同一兩步 fail-closed 結構(先 401 平價的 token 驗證,再 403 平價的角色判斷,任一失敗 `next` 不執行),差異僅角色集合:`require_admin` = `admin`;`require_staff` = `admin` 或 `coach`;`require_coach` = 僅 `coach`(admin 刻意排除,不是 `require_staff` 的子集)。驗證通過者把 `AuthUser` 注入 request extensions,`extractors::auth` 的 fast path 命中即 clone 回傳,不重打 Redis/DB。Request-data-dependent 的細粒度檢查(`require_course_coach`、`is_admin()` 分支)不屬此類,留在 service;碩果僅存的 handler-level 例外只剩 `rewards::list` 的條件式 `?all=true` 閘門(依賴 query 參數,本質不可上移)。
第四個訊號 `extractors::auth::LoginRequired`(newtype 包 `AuthUser`)補的是「需登入、不看角色」這一類——不掛三個 route_layer 之一的 handler,首行若仍是裸 `AuthUser` 參數,讀者無法從簽章分辨這是「刻意只要求登入」還是「忘了寫角色檢查」;換成具名的 `_login: LoginRequired` 參數,登入閘門本身變成簽章可見的宣告,與已刪除的 37 個儀式 `_auth`/`require_role` 呼叫區隔(目前兩站:`GET /coupons/{code}/validate`、`GET /courses/{id}/sessions`)。`tests/http_authorization_gates.rs` 是這一組(三個 route_layer 閘門 + `LoginRequired`)的回歸網——每個閘門挑一個代表端點釘住「無 token / 錯角色 / 對角色」三態的 shape,不逐 handler 複查(逐 handler 角色覆蓋率留在各自 `tests/http_*.rs`);`tests/middleware_auth_extractor.rs` 覆蓋的是 extractor 本身的 token 細節(過期/偽造/停用帳號),兩份測試檔分工不重疊。
_Avoid_: 把三個閘門實作成同一個參數化 factory(刻意各自獨立函式,見 `require_staff`/`require_coach` 檔頭)、把 `require_coach` 誤認為 `require_staff` 的簡化版或子集(語意不同——後者含 admin bypass,前者不含)。

**Session 簽發(Session Issuance)**:
`auth::service::issue_session` 是簽發 access/refresh token 對的單一 owner——register、login、google_auth、refresh_token 四路共用同一份簽發儀式(舊 `build_auth_response` 雙胞胎已刪)。三個 invariant 由它獨力維護:refresh token 進庫前必雜湊(`jwt::hash_token`,SHA-256,DB 外洩不直接洩漏可用憑證)、access/refresh 恆成對簽發(不存在只發一邊的中間態)、`expires_at` = 簽發當下 `now + jwt_refresh_expiration_days`(auth token 效期是「時鐘 seam(Clock Seam)」詞條記錄在案的 carve-out,直呼 `Utc::now()` 而非經 handler 取樣)。呼叫端負責 conn/tx 邊界——函式只收 `&mut PgConnection`:register/google_auth/refresh_token 三路在既有交易內呼叫(refresh 簽發已原子化於同一 tx,新 token 簽發與舊 token revoke 同進同出),login 則走一般 pooled 連線。
_Avoid_: 另造第二份簽發儀式(舊 `build_auth_response` 雙胞胎已刪,勿復辟)

**上架可見性(Listing Visibility)**:
`products`/`courses`/`venues`/`coaches` 四模組的公開明細端點統一收斂:owner 是各自 repository 的 scoped finder——`find_active_by_slug`/`find_active_by_id`(products、courses 兩者皆備;venues 僅 `find_active_by_slug`,因其明細端點本就 slug-only;coaches 僅 `find_active_by_id`,因其明細端點是 UUID-only),`is_active = true` 直接寫進 SQL WHERE,不是 fetch 後再濾——service 端(`get_by_slug`/`get_by_id`/`get_detail`)拿到 `None` 就地回 `NotFound`,已下架資源因此與不存在同形(契約用語見各端點的「已下架資源走公開明細一律 404」註記,含 coaches 的公開班表端點同一謂詞下沉)。公開列表端點(`find_all_active`)本已濾,這輪收斂補的是明細/班表側先前敞開的側門。
Cart 加入購物車路徑刻意不重用這條謂詞:`cart::service::add_product_item`/`add_course_item` 走一般(未限定 active)的 `find_by_id`,改由 `Product::ensure_purchasable`(`products/model.rs:88`)回 400「product is not available」——已登入買家主動加入購物車時,「這項目目前不可購買」比對外瀏覽用的遮蔽性 404 更有用。結帳自己的下架 gate(甲案,見「結帳快照」詞條)是第三種形狀(422、整批、列名)。同一件事(下架)在三個操作站點各自對應不同狀態碼,是刻意分流,不是三套裁決漂移。
_Avoid_: 把 cart 的 400 或結帳的 422 誤認為上架可見性謂詞沒收乾淨的殘留破口——三者是刻意不同語意,不該收斂成同一種寫法。

**結帳快照(Checkout Snapshot)**:
`cart::repository::find_cart_items_for_checkout_tx` 刻意不再以 `is_active` 過濾結帳快照——下架行原樣隨隊回傳(`is_active` 欄位隨行攜帶),不再靜默消失。`orders::fulfilment::ensure_all_purchasable`(甲案)是把「消失」改回「具名拒絕」的唯一站點:在購物車為空的 400 檢查之後、優惠碼載入之前執行,收集快照裡每個 `!is_active` 行的名稱(維持快照原序,不排序),清單非空就整批 422(`以下項目已下架,請先自購物車移除再結帳:「A」、「B」`);清單為空才放行進 `pricing`/`fulfilment::plan`。刻意不比照本函式其他分支接 `TxReleased` 重播——品項下架不是併發結帳造成的(是獨立的 admin 側操作),沒有「贏的孿生交易」可重播。cart 側的 MUST 條款(呼叫端必須先跑過 `ensure_all_purchasable` 才能把這份快照視為可購買)單一 owner 是 `cart::repository::find_cart_items_for_checkout_tx` 的 doc comment,`cart::service` 同名轉手層不重複散文、只指回這裡。
_Avoid_: 把這個 422 與空購物車的 400 混同(購物車非空、但全部品項下架時,回的是這個 422,不是「cart is empty」的 400)。

**帳號誕生(Account Birth)**:
`auth::provisioning::create_account` 是「`INSERT users` + 指派 `member` 角色 + 排入 `user_registered` outbox 事件」三步驟的單一 owner。四個帳號誕生呼叫端中兩個換線於此:`auth::service::register`、`users::service::create_user`(`POST /users`,admin 代建)——後者是新裁決:admin 代建帳號現在與自助註冊共用同一 owner,因此也一併排入 `user_registered` 事件(此前無此行為),但不簽發 session、不發歡迎通知,因為帳號不是使用者本人的註冊動作。另兩個誕生點刻意不納入這個 owner,理由記錄在該模組文件而非留給呼叫端猜:`auth::repository::create_or_update_google_user_tx`(google OAuth 的 Create 分支)靠 `ON CONFLICT (google_id) DO UPDATE` 擋首次登入併發,換成裸 INSERT 會讓這個 race 從安全 upsert 退化成唯一鍵違例 500;`bin/seed.rs` 的 `upsert_user`/`upsert_seed_member` 是冪等開發種子,若也走 `create_account` 會讓每次重跑 seed 都各自生一筆 `user_registered` 事件,這個副作用不該存在。
_Avoid_: 把 google_auth 的 Create 分支或 seed 的 upsert 也算進 `create_account` 的呼叫端(兩者刻意不歸戶,理由見上,不是遺漏)。

**studio 時間軸 SQL 攣生(Studio Timeline SQL Twin)**:
`reports::repository` 16 個查詢站點裡,「先把 UTC 的 `now` 換成 studio 牆鐘、再截斷/轉型」這條算式曾各自內嵌(`studio_month_anchor` 這個「本月 anchor」idiom 11 次、`studio_today` 這個「今天」idiom 5 次),與 `utils::studio_clock::{month_key, today}` 是同一條規則的兩份手抄副本(Rust 一份、SQL 16 處各一份)。migration `20260803000001` 把 SQL 側收斂成兩個 STABLE SQL function(`studio_month_anchor(now, tz)`/`studio_today(now, tz)`),16 個站點改呼叫這兩個 function,不再各自內嵌;STABLE 而非 IMMUTABLE 是因為 tzdata 本身可變,不是因為讀了當下時間(`now`/`tz` 仍是呼叫端參數)。Rust 與 SQL 兩份定義沒有被合併成一份(語言邊界擺在那裡,合不了),而是靠 cross-test 錨定彼此:`tests/service_reports.rs` 的 `month_key_matches_sql_studio_month_anchor`/`today_matches_sql_studio_today` 對同一組跨時區樣本(Asia/Taipei 月界前後一秒、America/New_York 逆向跨月、UTC)分別跑 Rust 版與 SQL 版,斷言逐一相等——與 `orders::model` 的 `revenue_predicate_matches_revenue_statuses_array`(錨定 `is_revenue()` 對 `REVENUE_STATUSES`)同一手法。窗長(11 個月/30 天/90 天……)不在這兩個 function 之內,維持 Rust/呼叫端常數。
_Avoid_: 誤以為這兩個 function 把 Rust 版也取代掉了(兩份手抄依然並存,只是現在互相錨定,不是各自維持同步的信任假設)。

**報表組裝(Report Assembly)**:
`reports::assembly::assemble_admin_report`(`AdminReportInputs → AdminReportResponse`)是 `GET /reports/admin` 的欄位塑形/推導純核——`service::admin_report` 原本 13 個循序 repository 查詢之後緊接的每一條 ratio/position-index/filter/split 規則,原本 inline 寫在 service 裡,現在收進這裡:zero IO、owned output、`#[cfg(test)]` 錨測試,與 `orders::pricing`/`orders::fulfilment`/`subscriptions::entitlement` 同一紀律,是這個純核姊妹家族的第六個成員(pricing → fulfilment → marking::plan → pairing::resolve_pair → entitlement::plan → assembly::assemble_admin_report)。`current_month_key` 刻意是獨立參數而非 `AdminReportInputs` 的欄位——「server/now 不進純核」:它衍生自呼叫端取樣的 `now`,擺在 struct 外才能讓 `AdminReportInputs` 每個欄位都單純是 repository 查詢結果,不混入 clock 語意。`safe_ratio`(count-over-count、分母 0 回 `None` 而非 NaN/Infinity)是三種比率計算與 `category_split` 共用的同一個零安全 helper。
_Avoid_: 把 `current_month_key` 塞回 `AdminReportInputs` 的欄位(那會讓這個純輸入 struct 混入一個衍生自 clock 的值,污染「每個欄位都是查詢結果」的不變量)。

**權益授予(Entitlement Grant)**:
`subscriptions::entitlement::plan` 是 `grant_from_purchase_tx` 原本 inline 計算的四分支規則抽出的純核(zero IO、owned output,`orders::pricing`/`orders::fulfilment` 同一紀律)——決定「買 `quantity` 份 `product` 授予什麼」:非 entitlement 商品(非 membership/ticket)回 `Ok(None)`;`session_count` 有設定 → 一列,`total_sessions = remaining_sessions = session_count × quantity`(`valid_days` 若也有設定則 `expires_at` 一併計);否則 `valid_days` 有設定 → 只設 `expires_at`,`quantity` 必須是 1(時間制方案不可疊買,否則 `AppError::Validation`);兩者皆無 → 無期無額的 unlimited 會籍。`now` 是呼叫端取樣值(checkout 自己的 `now`,經 `grant_from_purchase_tx` 透傳),不在此讀 `Utc::now()`——四個分支的 `expires_at` 因此能在 `#[test]` 裡斷言精確到 `now + Duration::days(n)`,不必留 DB 往返造成的 ±1 天窗。此核從不碰 `subscription_derived_status`——那個 SQL function 仍是讀時到期/狀態推導的唯一 owner(ADR-0003 Addendum);`plan()` 只算寫入當下要授予什麼。
_Avoid_: 把 `subscription_derived_status`(讀時狀態推導)與 `entitlement::plan`(寫時授予計算)當成互相取代的兩份邏輯——寫時決定授予什麼,讀時決定現在算不算 active,是同一個 ADR-0003 底下分工的兩側,不是重複。

**唯一鍵衝突映射(Unique-Conflict Mapping)**:
`AppError::conflict_on_unique`/`conflict_on_constraint`/`conflict_on_exclusion`(`src/error/mod.rs`)是「DB 唯一/排他約束違例 → 409」翻譯的單一 owner——判斷真相是 constraint 本身(SQLSTATE `23505`/`23P01`,`conflict_on_constraint` 額外比對違反的具名約束),不是應用層預檢:先 `SELECT` 查是否已存在、查無再 `INSERT` 的預檢寫法已禁用(Phase 7 的 slug 衝突改吃 DB constraint,刪 SELECT 預檢——`courses`/`products`/`posts`/`venues` 四表皆同),因為預檢與寫入之間永遠有 TOCTOU 窗口,constraint 違例才是唯一不可能漏判的判準。十餘個模組共用同一組函式(email、coupon code、slug、教練班表時段重疊……),各自只傳自己的錯誤文案字串,映射邏輯不重複。
五個帶 `slug` 欄位的表分成兩種唯一性口徑,是這個 owner 底下的一個具體案例:`venues`/`products`/`posts`/`courses` 四表以 `LOWER(slug)` 功能性 unique index(`uq_<table>_slug_lower`)實作,與各自 `find_by_slug` 查詢的 `WHERE LOWER(slug) = LOWER($1)` 對稱——大小寫不敏感貫穿唯一性檢查與查找兩側,四表的 create/update 皆由 `conflict_on_unique`/`conflict_on_constraint` 接在 repository 呼叫後轉譯 409。`coaches` 是唯一例外:`coaches_slug_key` 是欄位級 `UNIQUE`(大小寫敏感),且 coaches 從未對外提供 slug-based 查找端點(明細端點走 UUID)——`slug` 目前只是展示用欄位,尚未真正需要案例不敏感語意。此不對稱是已知、記錄在案的落差,非 slug 衝突慣例統一(Phase 7)的修復範圍。
_Avoid_: 假設 coaches 的 slug 衝突判斷與其他四表同樣大小寫不敏感(目前不是)、在任何新唯一性檢查前補一道 SELECT 預檢(繞過這個 owner 等於重新打開 TOCTOU 窗口)。

**跨模組讀表(Cross-Module Table Reads)**:
模組直接 `SELECT`/`JOIN` 另一模組的表,不必先繞經對方模組的 repository/service,是本庫行之有年的常態——witness 與 owner 協定治理的是「寫」,不是「讀」:讀開放、寫歸戶。「週課表(Weekly Schedule)」詞條的 `sessions::repository` 直讀 `course_schedule_slots`/`courses` 已先點名此例;現碼另有四處可查:`attendance::repository::find_approved_leave_enrolment_ids_tx` 直讀 `leave_requests`(leave 的表);`courses::seats::session_seats_tx` 的座位公式同樣直讀 `leave_requests`;`cart::repository::find_cart_items_for_checkout_tx`(orders 結帳流程呼叫)直讀 `products`/`courses`,對 product 列取 `FOR SHARE`——讀鎖不等於寫入,真正扣庫存仍轉交 owner `products::service::reserve_stock_tx`;`reports::repository::income_by_source` 一條查詢直讀 `orders`/`order_items`/`products`/`bookings`/`time_slots` 五表聚合月營收。與「給點」的 `apply_delta_tx`、「場租佔位」的 `bookings::occupancy`、「座位」的 `lock_`/`FOR UPDATE` 協定等既有詞條對照:那些詞條收斂的是「誰能動這張表」,本詞條收斂的是「誰都能看這張表」,分界只在寫、不在讀。
_Avoid_: 為了跨模組讀而新增一層轉手 repository/wrapper——ADR-0005 已裁定 CRUD 轉手 service 層維持現狀不收攏,讀端再包一層只是加寬介面、沒加深功能,換來一顆更淺的模組,不是解法。

**執行環境(AppEnv)**:
`config::AppEnv` 是 `APP_ENV` 讀取與比對的單一 owner——`AppConfig::load()`、`main.rs` 的 production 守衛與 log 格式判斷、`bin/seed.rs` 的 production 拒跑檢查,原本四處各自讀取、比對語意不一(混雜大小寫敏感的 `==`/`!=`),現在共用同一個 newtype。`is_development()`/`is_production()` 兩謂詞刻意獨立、不可合併:`AppConfig::load()` 自己的檢查是「非 dev 都跑」(fail-closed,`!is_development()`),`validate_production_config`(原 main.rs 守衛,現搬進本模組、可單元測試)是「僅 production 跑」(`is_production()`),語意不同。兩謂詞皆 `eq_ignore_ascii_case`(大小寫不敏感);`config/{env}.toml` overlay 檔名則用 `raw()` 保留原始字串(大小寫照舊)。因為「非 dev」嚴格涵蓋「production」,`main()` 啟動路徑上 `AppConfig::load()` 先跑、`?` 短路,`validate_production_config` 的 placeholder secret 分支在該路徑下實際不可達(production 淨效果不變——兩者都拒啟動,只是拒絕文案來源不同);此分支保留是刻意的 defense-in-depth,`validate_production_config` 必須對任意呼叫者(不只 `main()`,含未經 `load()` 的手工建構 `AppConfig`)自足有效。
_Avoid_: 與 `config` crate 自帶的 `config::Environment`(`env_source()` 用的 config 來源,讀 `APP__*` 環境變數到設定樹)混同——兩者名字容易撞,語意完全無關,新型別故意不叫 `Environment`。

**消費決策(Consumer Decision)**:
`kafka::consumer::decide`(private,零 IO、不 log)是 audit consumer 主迴圈「這一輪 poll 要不要 commit、要不要清 retry 計數」的單一決策 owner——`PollOutcome`(`StreamError`/`NonUtf8Payload`/`EmptyPayload`/`HandledOk`/`HandledPoison`/`HandledTransient { attempts }`)→`LoopAction`(`CommitAndClear`/`LeaveForRetry`/`PollAgain`)的表格式純函式,取代原本散在迴圈五處的手抄 commit+remove 對;`commit_message` 與 `retry_counts.remove` 的實作收斂到單一 helper `commit_and_clear`(`LoopAction::CommitAndClear` 的唯一處理點),`MAX_TRANSIENT_RETRIES` 邊界(`attempts == 5` 已 `CommitAndClear`、不再重試)由 `#[cfg(test)]` 表格測試逐列釘住。`domain_resource` 的 prefix fallback(`order_*`/`booking_*`/`user_registered` 三條)已刪——對現有 5 個 event_type 是死碼,卻會把未建模的同 prefix 子型別(如未來的 `order_refunded`)悄悄誤攔成 "order";刪除後未建模 event_type 一律落既有的 `data.resource` 讀取路徑(預設 `"audit"`),`domain_resource` 收斂為 `spec_for_event_type(event_type).map(...)` 的單純包裝。
_Avoid_: 把 stream-Err 分支誤讀成迴圈裡真的呼叫了 `decide(StreamError)`——那裡結構上早於 `message`/`retry_key` 存在,是殼層早退捷徑,`PollAgain` 這條政策靠表格測試釘住,不是執行期呼叫路徑。
