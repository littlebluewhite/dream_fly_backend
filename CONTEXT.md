# Dream Fly

Dream Fly 工作室預約與商務後端的領域語言。於 notifications 接縫架構審查(2026-06)首次建立,後續解析的術語陸續補入。

## Language

**Notification**:
持久化到 `notifications` 表、只顯示給單一使用者的 in-app 訊息,在某個領域動作 commit 之後以 best-effort 寫入;絕不阻擋或回滾觸發它的動作。「commit 之後」由 `PendingNotification`(`#[must_use]`,`.deliver(db)` 唯一 IO 入口)提醒,時機本身仍是位置慣例。目前所有交付站點皆遵循「tx 內完成領域寫入 → commit → deliver」標準形,無例外站點。
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

**行計畫(Line Fulfilment)**:
`orders::fulfilment::plan → FulfilmentPlan`,純函式(pricing 的姊妹),對 `CartItemType` 一處 exhaustive match(無 `_` arm——新變體 = 此處編譯錯誤)把結帳購物車切成商品行(`ProductFulfilment`:reserve 庫存 + grant 訂閱)與課程 id(enrol),取代 checkout 原本兩次互斥 `.filter(matches!)`。**排序不在此**:寫入保留序(write-reservation order;type-major、id-minor)由拿寫鎖的 owner 各自負責——商品 `products::service::reserve_stock_tx`、課程 `enrolments::service::enrol_batch_from_purchase_tx`——plan() 只保留輸入序,不排序(一個 invariant 兩個 owner 比沒有 owner 更糟)。
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
`points::service::apply_delta_tx` 是「使用者點數餘額變動且同時落一列 `point_ledger`」的唯一路徑——`users.points_balance` 不接受業務端直寫(runtime 由此函式交易內的 UPDATE + INSERT 協定維護、seed 消費同一 owner:`upsert_user`/`upsert_seed_member` 在同一交易內以 `PointReason::AdminAdjust` 呼叫,比照 `occupies_seat` 的 runtime/seed 共用模式)。fixtures 的 `set_points_balance`(`tests/common/fixtures.rs`)是記錄在案的測試 harness bypass——直寫 `points_balance` 略過 ledger,只用於測試佈局階段擺出起始餘額,不是業務路徑,不受此 owner 約束。
_Avoid_: 把 `set_points_balance` 當成業務可用的授點手段(它是測試專用的佈局捷徑)、把 `apply_delta_tx`「不 commit」誤讀成「不寫 ledger」(它一定寫 ledger,只是不 commit 交易,由呼叫端負責)

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
「先物化、再讀取」呼叫順序 invariant 的單一 owner:`sessions::repository::materialize_range` 回傳 `MaterializedRange` witness(欄位私有,僅該函式能建構;唯讀存取 `course_ids()`/`from_date()`/`to_date()`),兩個 early-return 路徑也回傳 witness。讀取端(`sessions::find_sessions_in`/`find_today_sessions_in`、`reports::venue_usage`/`coach_today_and_pending`/`upcoming_session_count`)改收 `&MaterializedRange`,不再各自靠 doc 前置條件維繫呼叫順序。witness 只擔保「此範圍已物化」,**不**擔保每個讀取端都按 `course_ids` 過濾——`venue_usage`/`coach_today_and_pending` 只用其日期窗(全場館聚合/coach scope 分別由查詢本身或 JOIN 表達),`find_sessions_in`/`find_today_sessions_in`/`upcoming_session_count` 才綁 `course_ids`。
_Avoid_: 把 witness 當作 course 範圍過濾的保證(它只保證「已物化」)、materialize_range 呼叫順序仍是文件慣例(已收進型別系統)

**有效報名(Active Enrolments)**:
`active_enrolments` view(migration `20260711000001`)——「目前占用座位的有效報名」口徑的單一 owner:`WHERE status = 'active'` 篩選下沉至此,~22 個 READ 站點(`courses::seats` 的座位 COUNT、`courses`/`sessions` repository 的 `enrolled_count` 顯示子查詢、`attendance`/`leave` 的 active-enrolment 查找、`reports` 的會員/課程/教練統計,橫跨 7 個 module)換底至此 view,不再各自手寫 `status = 'active'` 判斷。刻意排除兩類站點,不下沉進本 view:(1) reports 的 3 個「報名事件」站點(`kpis` 的 new_enrolments_this/_last、`funnel` 的 new_enrolments)——量的是「這個月發生了幾次報名動作」,是事件流口徑而非占位口徑,即使二元 enum 下今天結果等價;(2) `enrolments` 寫側的狀態轉移語句(INSERT/UPDATE)——寫側不能讀自己正在寫的 view。`enrolments::repository` 的 `find_by_id_tx`/`find_owner`,以及各處「歷史列表」JOIN(/me 報名列表、certificates、leave、reports 的 `countable_attendance` JOIN 等)同樣刻意不換底,因為它們需要看到 cancelled 列(double-cancel 409 判斷、出席/證書/請假歷史等皆賴此)。
_Avoid_: 現役報名、未取消報名

**候補(Waitlist)**:
`waitlist_entries` 表——課程滿班時的**諮詢名單(advisory list)**,依加入序呈現(`GET /waitlist?course_id=`,admin only,舊到新,見 `waitlist::service::list_for_course`)。名額釋出(取消報名)不觸發任何自動遞補或通知;遞補一律由 admin 依名單順序人工聯絡,由候補者自行完成結帳——報名唯一入口是結帳(ADR-0002),系統不存在「保留座位給候補第一名」的模型(見 ADR-0006)。repo 現行「queue order」用語(同一支 doc comment)指的是這份名單的**列序**,與本詞條「不自動化」的定案並不衝突——列序本身仍有意義(人工聯絡依序進行),只是不會被系統自動出隊消費。
_Avoid_: 遞補佇列(「佇列」暗示自動出隊消費,與人工遞補定案相悖;僅避自動化暗示,不避「依序」語意本身)、waiting list promotion(`Promotion` 在本系統另指 `notifications`/`posts` 的行銷促銷分類,語意不同)

**時鐘 seam(Clock Seam)**:
`utils::clock`——handler 在請求開始經 `state.clock.now()` 取樣一次,以 `now: DateTime<Utc>` 參數往下傳入 service;牆鐘語意的 service 不再自行呼叫 `Utc::now()`；非牆鐘語意站點(auth token 效期、posts 發佈時戳、subscriptions entitlement 到期計算)為記錄在案的 carve-out。`utils::studio_clock` 的純函式(`today`/`has_started`/…)本身不變,一樣收 `now` 參數——這層只是把「由誰取樣」從 service 上移到 handler 一層。
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
_Avoid_: 把三個閘門實作成同一個參數化 factory(刻意各自獨立函式,見 `require_staff`/`require_coach` 檔頭)、把 `require_coach` 誤認為 `require_staff` 的簡化版或子集(語意不同——後者含 admin bypass,前者不含)。

**Session 簽發(Session Issuance)**:
`auth::service::issue_session` 是簽發 access/refresh token 對的單一 owner——register、login、google_auth、refresh_token 四路共用同一份簽發儀式(舊 `build_auth_response` 雙胞胎已刪)。三個 invariant 由它獨力維護:refresh token 進庫前必雜湊(`jwt::hash_token`,SHA-256,DB 外洩不直接洩漏可用憑證)、access/refresh 恆成對簽發(不存在只發一邊的中間態)、`expires_at` = 簽發當下 `now + jwt_refresh_expiration_days`(auth token 效期是「時鐘 seam(Clock Seam)」詞條記錄在案的 carve-out,直呼 `Utc::now()` 而非經 handler 取樣)。呼叫端負責 conn/tx 邊界——函式只收 `&mut PgConnection`:register/google_auth/refresh_token 三路在既有交易內呼叫(refresh 簽發已原子化於同一 tx,新 token 簽發與舊 token revoke 同進同出),login 則走一般 pooled 連線。
_Avoid_: 另造第二份簽發儀式(舊 `build_auth_response` 雙胞胎已刪,勿復辟)

**跨模組讀表(Cross-Module Table Reads)**:
模組直接 `SELECT`/`JOIN` 另一模組的表,不必先繞經對方模組的 repository/service,是本庫行之有年的常態——witness 與 owner 協定治理的是「寫」,不是「讀」:讀開放、寫歸戶。「週課表(Weekly Schedule)」詞條的 `sessions::repository` 直讀 `course_schedule_slots`/`courses` 已先點名此例;現碼另有四處可查:`attendance::repository::find_approved_leave_enrolment_ids_tx` 直讀 `leave_requests`(leave 的表);`courses::seats::session_seats_tx` 的座位公式同樣直讀 `leave_requests`;`cart::repository::find_cart_items_for_checkout_tx`(orders 結帳流程呼叫)直讀 `products`/`courses`,對 product 列取 `FOR SHARE`——讀鎖不等於寫入,真正扣庫存仍轉交 owner `products::service::reserve_stock_tx`;`reports::repository::income_by_source` 一條查詢直讀 `orders`/`order_items`/`products`/`bookings`/`time_slots` 五表聚合月營收。與「給點」的 `apply_delta_tx`、「場租佔位」的 `bookings::occupancy`、「座位」的 `lock_`/`FOR UPDATE` 協定等既有詞條對照:那些詞條收斂的是「誰能動這張表」,本詞條收斂的是「誰都能看這張表」,分界只在寫、不在讀。
_Avoid_: 為了跨模組讀而新增一層轉手 repository/wrapper——ADR-0005 已裁定 CRUD 轉手 service 層維持現狀不收攏,讀端再包一層只是加寬介面、沒加深功能,換來一顆更淺的模組,不是解法。
