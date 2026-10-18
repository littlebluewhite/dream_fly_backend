# Dream Fly

Dream Fly 工作室預約與商務後端的領域語言。於 notifications 接縫架構審查(2026-06)首次建立,後續解析的術語陸續補入。

## Language

**Notification**:
持久化、只顯示給單一使用者的 in-app 訊息。在領域動作 commit 之後 best-effort 寫入,絕不阻擋或回滾觸發它的動作(ADR-0009)。owner 是 `notifications::service`。
_Avoid_: alert, push(本系統無外部推播通道), message

**Event**:
描述業務事實(如 order created、user registered)的 outbox/Kafka 紀錄,在業務交易內寫入,供 audit 與外部整合。非使用者可見——Event 不是 Notification。
_Avoid_: notification, message

**工作室時鐘(Studio Clock)**:
牆鐘語意的單一歸屬,`utils::studio_clock`,契約 §3.18 裁決 2。
`StudioNow { tz, now }`(`Copy`)是這個時間情境本身的單一值化——取代逐 service 各自收 `server: &ServerConfig, now: DateTime<Utc>` 兩個參數的舊形狀;唯一 production 建構點是 `AppState::studio_now()`(取樣一次 `clock.now()`,配上 `config.server.studio_timezone`)。
seed 的唯一取樣點是 `bin/seed/main.rs`——同一 `StudioNow { tz: config.server.studio_timezone, now: Utc::now() }` 建構一次,交給 `dataset::run` 往下傳(見「Seed 資料集」詞條)。
`studio_clock` 模組內的自由函式(`today`/`has_started`/`require_not_started`/…)簽章不變,仍各自收 `(tz, now)`——`StudioNow` 只是呼叫端把這兩個值一起攜帶的容器,不改變這一層純函式的介面。
`ServerConfig::studio_timezone` 本身也從 `String` 改型別為 `chrono_tz::Tz`(直接 deserialize,非法 IANA 名稱在 config 載入當下就拒絕,不再是 runtime fallback)。

**課程教練所有權(Course-Coach Ownership)**:
單一資源的教練 gate:呼叫者是不是這堂課的教練,admin 直接放行,不是則 403。owner 是 `coaches::service::require_course_coach`。發證書用的「教過此生」關係 gate 是另一種形狀,留在 certificates。
_Avoid_: 把發證書的關係 gate 與單課 gate 混同、與「教練範圍」混同(那是名下範圍,不是單資源 gate)

**教練範圍(Coach Scope)**:
教練名下的課程與學員:`courses.coach_id` 指向該教練的全部課程,不論是否下架;學員是這些課程有效報名去重後的使用者。owner 是 `coaches::service::resolve_scope`,為什麼含下架課程見 ADR-0012。
_Avoid_: 用下架把課程移出教練名下(要取消報名或改派教練)、替單一讀取端加回上架過濾

**訂單定價(Order Pricing)**:
結帳的金額算術:小計、優惠碼折抵、點數折抵上限、應付總額與這張訂單賺的點數,是不碰資料庫的純函式。owner 是 `orders::pricing`。
_Avoid_: 計價、報價、quote

**購買時刻(Purchase Instant)**:
一次結帳取樣的那一個業務時間;這次購買寫下的訂單、訂單行、點數帳、報名與訂閱都蓋這個時刻(ADR-0017)。owner 是 `orders::service::checkout`。
_Avoid_: 下單時間、付款時間(結帳沒有另外的付款步驟,兩者都是購買時刻)

**行計畫(Line Fulfilment)**:
把可購買的購物車切成商品行(扣庫存、授予權益)與課程(報名)的計畫;只切不排序,也不會失敗。owner 是 `orders::fulfilment`。
_Avoid_: 分派、dispatch(那是動作,這裡指切出來的計畫)、鎖序(排序歸鎖的 witness)

**訂單鎖協定(Order Lock Protocol)**:
結帳與退款在第一筆寫入前取得所有列鎖的固定順序:買家的 users 列最先,再依 id 升序鎖要動的商品與課程;之後的寫入都要出示鎖的 witness(ADR-0007)。owner 是 `orders::locks`。
_Avoid_: 預鎖、pre-lock、在寫入端各自再排序一次

**營收狀態集(Revenue Statuses)**:
計入營收的訂單狀態:paid、processing、completed。owner 是 `OrderStatus::is_revenue`,報表與售出件數的 SQL 綁同一集合。
_Avoid_: 有效訂單、paid 訂單(處理中與已完成也計入)

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
`BookingStatus::occupies_seat` 是「這筆 booking 佔不佔一個座位」謂詞的 Rust 端 owner;它的 SQL 攣生面是 view `occupying_bookings`(migration `20261003000002`,狀態清單 `pending`/`confirmed`/`completed`/`no_show`——只有 `cancelled` 釋出座位),兩者由 `service_bookings.rs::occupying_bookings_view_matches_occupies_seat` 逐變體錨定(含 PG enum 與 Rust 變體的窮舉 tripwire)。
slot 的佔用數**讀時計數、不落地**(ADR-0015):`time_slots.booked` 欄位已刪,讀取端 `schedule::repository::SLOT_COLUMNS` 對 view 做 correlated COUNT,wire 的 `booked` 欄位不變。
協定 owner 是 `bookings::occupancy`:佔位 = `occupy_slot_tx` 先 `FOR NO KEY UPDATE` 鎖 slot 列(`AND is_closed = false`)、再以**另一條** statement COUNT view,`>= capacity` 即拒(比照「座位」詞條的 `courses::seats`;COUNT 不可併入取鎖那條,READ COMMITTED 的 snapshot 以 statement 為單位),通過才 `insert_occupying_booking_tx` 落列;釋出 = `cancel_occupying_booking_tx` 把 booking 轉 `cancelled`,不碰 `time_slots`。
任何新的預約(新增 booking)路徑必須經過 `occupancy::occupy_slot_tx`,不得自行 INSERT;未來任何 `BookingStatus` 寫入者(如狀態轉移端點)必須經過此模組;新增 `BookingStatus` 變體時必須同時決定它佔不佔位,改 `occupies_seat` 並以新 migration 重建 view。
seed(`src/bin/seed/dataset/rental.rs` 的 `seed_rentals`)與測試 fixture `seed_booking`(`tests/common/fixtures.rs`)直寫指定狀態的 booking 列(ADR-0010 bypass),不經 occupancy 協定——沒有快取欄位要同步,佔不佔位由 view 讀時決定,所以兩者連謂詞都不必呼叫(seed 從未寫入 `Pending`,這個變體目前是零寫入者:runtime 佔位 insert 寫字面值 `confirmed`、取消轉 `cancelled`,seed 只用 `Completed`/`Cancelled`/`NoShow`)。
seed 端的規則(`seed_rentals(db, at, venues: &[(Uuid, i64)], members) -> (usize, usize)`,`run` 用 slug 查 4 個場館交給它):每個場館每週 3 個容量 1 的時段、共 14 週(往前 8 週 + 本週 + 往後 5 週);只有日期早於 `at.today()` 的時段依**日期**推導的索引決定預約狀態與預約會員,所以之後的執行不會改寫已過去時段的預約、只補新變成過去的時段;同一時段永遠不超過容量;同一時刻重跑不新增時段或預約;開發者改成取消的種子預約,重跑後維持取消;「是否已有預約」只看 `time_slot_id`、不看會員與狀態,所以開發者在 app 裡用別的會員訂走的未來種子時段,變成過去後重跑也不會再被種子訂一筆(檔內 `#[sqlx::test]` 逐條釘住)。
_Avoid_: 把課程「座位(Seats)」詞條與此混為一談(不同產品線)、在 `bookings::occupancy` 之外(seed 與上述 fixture 這兩處記錄在案的 bypass 除外)寫 `bookings.status`、在 SQL 手寫 `status <> 'cancelled'` 或另一份狀態清單計佔位(一律經 `occupying_bookings`)、把佔用數重新落地成 `time_slots` 欄位

**給點(Point Grant)**:
使用者點數餘額的每一次變動,連同記下它的那一列點數帳;沒有只動其中一邊的業務路徑。owner 是 `points::service`。
_Avoid_: 加點、扣點(正負都是給點,方向由點數帳的 reason 決定)、直接改餘額

**點數級距(Points Tier)**:
會員依點數餘額分入的固定 4 級報表維度(regular、bronze、silver、gold),只用來分桶,不帶任何權益。owner 是 `points::model::PointsTier`。
_Avoid_: 會員等級、membership level

**Seed 資料集(Seed Dataset)**:
dev 資料庫的種子資料:固定的開發帳號與目錄,加上 12 個月、可重現的報表資料集;冪等,重跑不重複寫入。只活在 seed 這個 bin,owner 是 `dataset::run`(ADR-0010)。
_Avoid_: fixture(那是測試 harness 的佈局)、demo data、假資料

**`run` 只做組合(Run Only Composes)**:
seed 資料集本體的分工:有自己規則的種子寫入各是 `dataset` 底下一個有介面的子 module,本體只把它們和單表的目錄寫入依序串起來(ADR-0010 Addendum 2026-10-16)。
_Avoid_: 把子 module 的規則寫回資料集公式之間、只靠整份資料集測一條種子規則

**點名(Seed Attendance)**:
種子出勤:報表資料集的每筆有效報名,在已過去的場次上依固定公式各記一筆出勤。直寫、不經 runtime 的點名流程(ADR-0010),owner 是 `dataset::attendance`。
_Avoid_: 與「點名計畫」混為一談(那是 runtime 的批次點名)

**待付款對照單(Pending Contrast Orders)**:
報表資料集每月一筆從未付款的種子訂單,作為已付訂單的對照。結帳只建已付訂單,所以它是訂單路徑上唯一不經結帳的種子列,owner 是 `dataset::pending`。
_Avoid_: 未付款購買、草稿訂單

**種子購買時間線(Seed Purchase Timeline)**:
seed 經真實購物車與結帳買下已付種子訂單、再經訂單狀態機推到目標狀態的那一步;訂單、點數帳、授權與報名都是結帳自己的產物。owner 是 `dataset::timeline`(ADR-0010)。
_Avoid_: 直寫種子訂單、在 seed 裡另抄結帳或狀態機的步驟

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
「課程還有沒有位子」invariant 的單一 owner:`courses::seats`——課程層 `CourseSeats::is_full`(enrol 持鎖 `course_seats_tx`——收 `&CourseLocks`、鎖由 `lock_courses_tx` 先取;waitlist 無鎖 `course_seats`)與場次層 `require_room_tx`(實體座位模型 `max - active + leave - makeup > 0`,契約 §3.20——場次座位一次回答:四數快照 `SessionSeats` 私有、只以 `has_room()` 對內作答,沒位子 409「該場次名額已滿」文案由本模組持有,呼叫端拿不到計數、也不自己比大小;情境測試在 `tests/service_seats.rs`)。
鎖策略由參數型別宣告:`&PgPool` = 無鎖快照、`&mut Transaction` + `lock_` 前綴 = `FOR UPDATE` 列鎖;`courses`/`sessions` repository 的 `enrolled_count` 是顯示用 inline 拷貝,拷貝的是對 `active_enrolments` view 的引用——謂詞單源已下沉至該 view,非決策端。
場次層「先鎖列、再讀座位」的呼叫順序也已收進型別系統,比照「場次物化」詞條的 `MaterializedRange` 寫法:`lock_session_tx` 回傳 `SessionLock` witness(欄位私有、僅該函式能建構,唯讀存取 `session()`——鎖定列整筆 `sessions::model::CourseSession`),`require_room_tx(tx, &SessionLock)` 只收 witness——原本呼叫端另傳的 `course_id` 參數已不存在,「course_id 與被鎖場次不配對」整類錯誤隨之消失;場次列已鎖、課程外鍵必存在,故 `fetch_one`,不再有到不了的「課程不存在」404。
補課端直接拿 `lock.session()` 驗目標場次(見「請假規則」),目標場次只讀一次。
課程層同款:`lock_courses_tx(tx, ids) → CourseLocks`(升序 `FOR UPDATE`,「訂單鎖協定」的 courses 站)是唯一建構點,`course_seats_tx(tx, &CourseLocks, course_id)` 取代原 `lock_course_seats_tx`——鎖已先取,COUNT 仍是取鎖之後的另一條 statement;`course_id` 不在 witness 內 → `AppError::Internal`。
課程列一開始就取 enrol 需要的 `FOR UPDATE`,不再有「購物車讀取持 SHARE、enrol 再升級 UPDATE」的同列死鎖(`40P01`)拓撲。
_Avoid_: capacity, quota

**出席口徑(Countable Attendance)**:
`countable_attendance` view(migration `20260710000001`)——出席聚合報表口徑的單一 owner:view 成員資格(`status IN (present, absent)`)= 計入分母、leave 排除,顯式布林欄 `is_present` = 分子,view 內 `NOT is_present` 恆等於 absent。`reports::repository` 的 7 條聚合查詢(`kpis`/`coach_reports`/`attendance_distribution`/`retention`/`weekday_load`/`coach_attendance_in_range`/`member_attendance`)與 `enrolments::repository::find_by_user_with_course`(`GET /enrolments/me` 的 attended/total 統計)皆換底至此 view,不再各自手寫 `status` 判斷;`coach_today_and_pending` 的 `pending_attendance`(任一狀態 EXISTS)是另一個口徑,故意不進這張 view。
_Avoid_: 出勤率(那是 service 算出的 rate,不是這個口徑本身)、attendance_records(那是底表,口徑 owner 是 view 不是它)

**點名計畫(Marking Plan)**:
`attendance::marking::{parse, plan}`,純函式(`orders::pricing`/`orders::fulfilment` 的姊妹),`PUT /sessions/{id}/attendance` 批次點名的 parse(status 字串轉 `AttendanceStatus`,無效值 422)+ 成員資格校驗(requested enrolment id 集合與呼叫端已查得的 valid 集合相等判斷,不等 422)兩段驗證的純核 owner,並持有核准恆勝的整批 pre-check(`plan` 第三輸入,見「請假」)與其 422 文字 `marking::APPROVED_LEAVE_OVERWRITE`;`parse` 必須先於 enrolment DB 查詢執行——現狀無效 status 不觸發查詢,錯誤優先序若反過來,DB 故障時 422 會變 500。
`plan` 的兩個輸入集合(valid-set、approved-set)的 DB 讀取、空批次跳過查詢、逐列 upsert 迴圈都在 `attendance::records::mark_tx`(出勤寫入 owner,寫入 tx 內跑);`service::bulk_upsert_attendance` 只剩 404 → 403 → 未開始 422 → parse → begin → `records::mark_tx` → commit → 名冊。
_Avoid_: 與「出席口徑(Countable Attendance)」混同——本核決定的是「這批點名寫入是否合法」,出席口徑決定的是「哪些既有紀錄計入出勤統計分母/分子」,寫入前驗證與讀取聚合是不同層。

**請假(Leave)**:
`leave_requests` 表——會員對已報名課程某一特定場次的請假申請,由該課教練或 admin 審核(契約 §3.20)。
核准是**投影**動作:`leave::service::decide_tx`(`decide_leave_request` 的寫入 seam)同一交易內雙寫——更新假單為 `approved`,並 `attendance::records::project_approved_leave_tx` 把該場次出勤投影成 `leave` 列(`marked_by` = 核准者)——`attendance::records` 是 `attendance_records` 的唯一 runtime 寫入者(module 公開、函式收窄,比照 `auth::session`;seed 照 ADR-0010 bypass;測試 fixture `seed_leave_request` 的 Approved/Rejected 先插 pending 再走 `decide_tx`,所以 Approved 假單必帶出勤 `leave` 投影,Pending/Cancelled 仍直接 INSERT)。**核准恆勝**:晚核准是營運常態,核准覆寫既有 `present`/`absent` 是合法裁決,`decide` 無時間閘、簽章不動;反方向由守衛擋下——**點名不可覆寫已核准請假**,雙層防護整套住在 `records::mark_tx`:`attendance::marking::plan` 第三輸入(該場次已核准請假成員集合,私有讀取 `find_approved_leave_enrolment_ids_tx` 於寫入 tx 內查得)做整批 422 pre-check,私有 `upsert_tx` 的 `ON CONFLICT … WHERE` 三分支守衛關閉 pre-check 的 TOCTOU 殘餘窗——守衛擋下回報 0 列,`mark_tx` 轉成同一整批 422(真併發見證:`service_attendance.rs::approval_committed_mid_batch_rolls_back_whole_batch`)。
口頭請假(`PUT "leave"` 無核准單)不受守衛約束、可自由覆寫。
座位面:請假釋出座位、補課佔座(見「座位(Seats)」詞條與 §3.20 名額公式——只讀 `leave_requests`,不讀投影出的出勤列)。
兩個 known gap(approved 無撤銷的臨時出席死角、guard `EXISTS` 快照落後的極窄並發窗,可重點 `leave` 復原)與甲案(derive)落選理由見 ADR-0008。
_Avoid_: 把「請假投影出的 `leave` 出勤列」(有核准單背書、受守衛保護不可被點名覆寫)與「口頭請假的 `leave` 出勤列」(無單、可自由覆寫)當成同一種;把「核准恆勝」誤讀成雙向覆寫(只有 decide→attendance 恆勝,點名→已核准 leave 恆敗,方向刻意不對稱)。

**請假規則(Leave Rules)**:
`leave::rules::{parse_decision, check_decidable, check_makeup_source, check_makeup_target}`,純函式(`attendance::marking` 的姊妹),`PATCH /leave-requests/{id}` 與 `POST /leave-requests/{id}/makeup` 決策核的純核 owner:`parse_decision` 把 `status` 字串轉 `LeaveStatus`,只收 `approved`/`rejected`(422)——刻意不用 `LeaveStatus::from_str`,後者的合法值還包含 `pending`/`cancelled`;`check_decidable` 檢查一筆已讀出的決策 context 是否仍可裁決(非 `pending` 409 `DECIDE_NOT_PENDING`;核准時若該假單的報名已取消——`enrolment_active = false`——409「報名已取消,無法核准請假」,**駁回不受此限**,因為駁回不發還任何東西,取消報名不影響其合法性——B5 起這個 409 只是併發 backstop,見下方**併發**);`check_makeup_source` 檢查已鎖定的假單是否可預約補課(非 `approved` 409、已有 `makeup_session_id` 409、報名已取消 409「報名已取消,無法預約補課」——三者順序固定,狀態先於已約先於報名);`check_makeup_target` 檢查已鎖定的目標場次(收 `&CourseSession`,即 `seats::SessionLock::session()`;跨課程 422、補課場次即請假場次本身 422、已開始 422 或 DST 模稜兩可 400,經 `studio_clock::require_not_started`)。
「報名是否仍有效」的口徑本身由 `repository::find_decision_context`/`find_for_makeup_tx` 各自以 `EXISTS (SELECT 1 FROM active_enrolments …)` 查得,回傳的 `LeaveDecisionContext`/`LeaveRequestForMakeup` 都刻意保留與 `enrolments` 的原始 JOIN(不改用 `active_enrolments` 當主表)——已取消的報名仍要能讓假單本身查得到(才能回 409 而非 404)。
`check_makeup_source`/`check_makeup_target` 分兩支是因為中間夾著「鎖目標場次」(`seats::lock_session_tx`)這個會 404 的 DB 讀取,`book_makeup` 的呼叫順序本身是錯誤序的一部分,不可調換:假單 404 → 本人 403 → `check_makeup_source` 409 → 鎖目標場次 404 → `check_makeup_target` 422/400 → `seats::require_room_tx` 座位 409 → 寫入(優先序釘在 `tests/service_leave.rs`;規則本身的各分支由 `rules.rs` 單元測試覆蓋,HTTP 層每種狀態碼只留一個代表)。
取消的 `WHERE status = 'pending'` guard(`repository::cancel_if_pending_tx`)已是單一 SQL 判斷,故意不收進本核——再包一層是死碼。
`repository::find_session_context` 只剩 `POST /leave-requests` 一個 caller,收窄為只收 `&PgPool`——makeup 端的目標場次改由場次列鎖一次讀出,不再另查。**併發**:兩個決策讀取(`find_decision_context`/`find_for_makeup_tx`)都是「讀後才寫」——讀到 `enrolment_active` 快照後,呼叫端的退課請求才 commit,是既有的「先核准再退課」合法狀態(座位公式本就只看 `active_enrolments` 現況,不回溯核准/補課當下的報名狀態),故刻意不額外加鎖收斂這道窄窗,與座位面「approved 無撤銷」的 known gap 同族。**報名取消連帶待審假單(B5,ADR-0008 2026-09-27 Addendum)**:`enrolments::service` 取消報名(自助 `cancel_enrolment` 或退款 `cancel_by_order_tx`)時,同 tx 經 `leave::service::cancel_pending_for_enrolments_tx` 把該報名的 `pending` 假單轉成 `cancelled`(已核准假單與補課不動,gap 1 不重開;不發通知)。
所以取消之後的審核(核准或駁回)都走 not-pending 409 `DECIDE_NOT_PENDING`,`check_decidable` 的報名 409 降為 backstop,只擋取消 commit 之後才插入的 pending 列與上線前遺留資料——後者駁回仍可清掉。
防死鎖:`enrolments::repository::find_by_id_tx` 取 `FOR NO KEY UPDATE`(不是 `FOR UPDATE`),與核准出勤 INSERT 經 FK 取的 `KEY SHARE` 相容,取消不會與進行中的核准成環。
_Avoid_: 與「請假(Leave)」詞條混同——本詞條是寫入前的純驗證核,「請假(Leave)」詞條講的是核准後的投影/守衛語意,兩者分屬不同層。

**請假讀取投影(Leave Read Projection)**:
`leave::repository::VIEW_COLUMNS`/`VIEW_JOINS` 兩個 const 加 `model::LeaveRequestView` 一個 row 型別,是「請假單讀取要長什麼樣子」的單一 owner——`find_my_leave_requests`/`find_admin_list` 兩個讀取端與 `insert`/`decide_tx`/`set_makeup_session_tx` 三個寫入端全部經由這兩個 const 組出同一份投影;寫入端的手法是 data-modifying CTE(`WITH lr AS (INSERT/UPDATE ... RETURNING *) SELECT {VIEW_COLUMNS} FROM lr {VIEW_JOINS}`)——裸 `RETURNING *` 只吐 `leave_requests` 自己的欄位,CTE 把它重新接上 `VIEW_JOINS` 拿到 course/session/makeup 顯示欄,寫入回應與 `/me` 讀到的是同一段投影邏輯,不是手抄的回聲。
`AdminLeaveRequestRow { #[sqlx(flatten)] view, user_id, user_name }` 是本 repo 首例 `#[sqlx(flatten)]`;`dto::LeaveRequestResponse` 的 `From<LeaveRequestView>` 是這份投影展開成會員回應的唯一站,`AdminLeaveRequestResponse` 刻意不用 `#[serde(flatten)]`——它的 wire key 順序由自己顯式宣告的欄位順序決定,不隨 `LeaveRequestView` 的欄位順序走。
`repository::find_decision_context`/`find_for_makeup_tx`(見「請假規則」詞條)刻意不併入這份投影——兩者保留原始 `enrolments` JOIN,是「已取消報名仍要能查到假單本身(回 409 而非 404)」的決策讀取,與本投影的顯示查找目的不同、不可互相取代。
契約 §3.20 的回應欄位/形狀不動,只是把「回應長什麼樣」的算法從三個寫入端各自手抄改成單一投影。
_Avoid_: 把這裡跟「請假規則(Leave Rules)」的 `find_decision_context`/`find_for_makeup_tx` 搞混——那兩個決策讀取刻意不進本詞條的投影。

**讀取投影慣例(Read Projection Idiom)**:
「某資源的回應長什麼樣子」由 repository 內一對私有 const 單一擁有——`*_VIEW_COLUMNS`(select list,欄位以固定別名前綴)+ `*_VIEW_JOINS`(假設該別名的列來源已在 scope 的 JOIN 子句)——配一個 `FromRow` row 型別;每個讀取端與每個「回應即該列」的寫入端都用 `format!` 組出同一段投影。
寫入端手法是 data-modifying CTE:`WITH x AS (INSERT/UPDATE ... RETURNING *) SELECT {COLS} FROM x {JOINS}`——一句話完成寫入並回傳投影列,不另開交易讀回、不留「剛插入就消失」的 `Internal` 防禦分支、不手抄回應欄位。
CTE 的列來源一律是 `RETURNING`,**絕不讀 view**:同一語句的子查詢看的是語句開始前的快照,view 裡還沒有正在寫的那一列。
讀取端各自保留自己的列來源(表或 view),只共用欄位與 JOIN。
回應不需要投影的狀態轉移(`DELETE` 類,無回應體)回傳裸 table row 即可。
實例:`leave`(`VIEW_COLUMNS`/`VIEW_JOINS`,原型,見「請假讀取投影」)、`certificates`(`REPORT_CARD_VIEW_COLUMNS`/`_JOINS` 服務 `insert_report_card`/`find_my_report_cards`;`CERTIFICATE_VIEW_COLUMNS`/`_JOINS` 服務 `insert_certificate`/`find_my_certificates`,證書 `LEFT JOIN courses` 因 `course_id` 可為 NULL)、`waitlist`(`VIEW_COLUMNS`/`VIEW_JOINS` 服務 `insert`、`find_by_user_with_course`(讀 `waitlist_entries` 表)、`find_by_course_waiting`(讀 `waiting_entries` view);`cancel_if_waiting_tx` 回裸 `WaitlistEntry`)。
每個實例由 HTTP 層逐欄 pin 錨定:寫入回應 `serde_json::Value` == 對應 `/me` 列(候補另比 admin 列表列)——`tests/http_leave.rs::*_response_matches_me_row`、`tests/http_certificates.rs::create_{report_card,certificate}_response_matches_me_row`、`tests/http_waitlist.rs::join_response_matches_me_and_admin_rows`。
_Avoid_: 寫入後同 tx 再 `find_*_row_tx` 讀回、從請求欄位 + 另查的顯示欄手拼回應(會與 `/me` 漂移);在寫入 CTE 裡讀 view。

**場次物化(Session Materialization)**:
「先物化、再讀取」呼叫順序 invariant 的單一 owner:`sessions::calendar::materialize_range(db, today, ids, from, to)` 回傳 `MaterializedRange` witness(欄位私有,僅該函式能建構;唯讀存取 `course_ids()`/`from_date()`/`to_date()`),每個 early-return 路徑也回傳 witness。**物化只往前**(ADR-0011):只插入 `[max(from, today), to]`,witness 仍帶請求的 `[from, to]`——過去日期只讀已存在的列,讀取端不必分辨;seed 回填過去改走具名的 `calendar::backfill_for_seed`(runtime 不可呼叫)。
讀取端(`sessions::find_sessions_in`、`reports::venue_usage`/`upcoming_session_count`)改收 `&MaterializedRange`,不再各自靠 doc 前置條件維繫呼叫順序。
witness 只擔保「此範圍已物化」,**不**擔保每個讀取端都按 `course_ids` 過濾——`venue_usage` 只用其日期窗(全場館聚合由查詢本身表達),`find_sessions_in`/`upcoming_session_count`/`find_today_sessions_in`/`coach_today_and_pending` 才綁 `course_ids`(`coach_today_and_pending` 以前另用 `coach_id` JOIN 重述教練範圍、不讀 witness 的 `course_ids()`,現改綁 `day.course_ids()`,與 `/sessions/today` 同一份範圍)。
日期軸再分一支:`sessions::find_today_sessions_in`/`reports::coach_today_and_pending` 額外要求單日(`TodaySessionRow` 無日期欄、「今天」本身無多日語意),這個前提由姊妹型別 `MaterializedDay` 在建構點一次成立——`materialize_today` 是唯一建構點,內部呼叫 `materialize_range(db, today, ids, today, today)` 重用同一套冪等/早退邏輯,兩個消費端改收 `&MaterializedDay`,原本各自的 `mat.from_date() == mat.to_date()` debug_assert 已退役。
物化只加不減(`ON CONFLICT DO NOTHING` 的單向 INSERT);週課表變動後的反向清理由 `sessions::calendar::replace_weekly_schedule_tx` 承接——`courses::service::update_course` 在同一 tx 內呼叫,換完 slot 後接著對該課程 `session_date > today` 的未來場次同步 `end_time` 與 `venue`,並刪除不再對應任何 slot、且無請假單(`session_id`/`makeup_session_id`,任何狀態)或點名紀錄引用的孤兒場次,今天以前(含今天)的場次不動,也不在此預先物化新時段。**場次記住場地**:`materialize_range` 物化時把 slot 的 `venue` 快照進 `course_sessions.venue`(migration `20260927000001`,既有列以當時的 DOW + `start_time` 對應規則 backfill,部署當下讀取結果不變);之後只有上述 reconcile 會對未來場次改寫它。
讀取端(`find_today_sessions_in`、`reports::venue_usage`)直接讀 `cs.venue`,不再回頭 join `course_schedule_slots`——slot 事後改開課時間不會讓今天/過去場次的場地變 NULL,改場地也不會把過去場次改名。**幻影場次(已收掉,ADR-0011)**:以前 `admin_report` 物化整個本月、含已過去的日期,月中改開課時間後本月過去日期會多出新時間的「幻影場次」(那天實際沒上)。
物化只往前之後不再產生;DB 裡既有的幻影列無從分辨,不清。
代價:某天變成過去之前若從未被任何讀取端物化過,就不計入 `venue_usage`(這種日子本來就不可能點名)。
_Avoid_: 把 witness 當作 course 範圍過濾的保證(它只保證「已物化」)、materialize_range 呼叫順序仍是文件慣例(已收進型別系統)

**有效報名(Active Enrolments)**:
`active_enrolments` view(migration `20260711000001`)——「目前占用座位的有效報名」口徑的單一 owner:`WHERE status = 'active'` 篩選下沉至此,~22 個 READ 站點(`courses::seats` 的座位 COUNT、`courses`/`sessions` repository 的 `enrolled_count` 顯示子查詢、`attendance`/`leave` 的 active-enrolment 查找、`reports` 的會員/課程/教練統計,橫跨 7 個 module)換底至此 view,不再各自手寫 `status = 'active'` 判斷。
刻意排除兩類站點,不下沉進本 view:(1) reports 的 3 個「報名事件」站點(`kpis` 的 new_enrolments_this/_last、`funnel` 的 new_enrolments)——量的是「這個月發生了幾次報名動作」,是事件流口徑而非占位口徑,即使二元 enum 下今天結果等價;(2) `enrolments` 寫側的狀態轉移語句(INSERT/UPDATE)——寫側不能讀自己正在寫的 view。
`enrolments::repository` 的 `find_by_id_tx`(取消路徑,`FOR NO KEY UPDATE`)/`find_owner`,以及各處「歷史列表」JOIN(/me 報名列表、certificates、leave、reports 的 `countable_attendance` JOIN 等)同樣刻意不換底,因為它們需要看到 cancelled 列(double-cancel 409 判斷、出席/證書/請假歷史等皆賴此)。
報名一旦離開本 view(取消),它的待審假單同 tx 轉成 `cancelled`(見「請假規則」詞條);已核准假單與補課不動。
_Avoid_: 現役報名、未取消報名

**候補(Waitlist)**:
`waitlist_entries` 表——課程滿班時的**諮詢名單(advisory list)**,依加入序呈現(`GET /waitlist?course_id=`,admin only,舊到新,見 `waitlist::service::list_for_course`)。
名額釋出(取消報名)不觸發任何自動遞補或通知;遞補一律由 admin 依名單順序人工聯絡,由候補者自行完成結帳——報名唯一入口是結帳(ADR-0002),系統不存在「保留座位給候補第一名」的模型(見 ADR-0006)。
repo 現行「queue order」用語(同一支 doc comment)指的是這份名單的**列序**,與本詞條「不自動化」的定案並不衝突——列序本身仍有意義(人工聯絡依序進行),只是不會被系統自動出隊消費。
「等待中(waiting)」這個口徑本身現由 `waiting_entries` view(migration `20260803000002`,`WHERE status = 'waiting'`)單一收斂——10 個原本各自手寫這個篩選的 READ 站點(`courses::repository` 的 7 個 `waitlist_count` 顯示子查詢、`waitlist::repository` 的 `exists_waiting`/`find_by_course_waiting`、`reports::repository::course_reports`)換底至此。
寫側狀態轉移(`insert`/`cancel_if_waiting_tx`)與無 waiting 謂詞的讀側(`find_by_id_tx` 取消路徑用的 `FOR UPDATE` 行鎖、`find_by_user_with_course` 的 `/waitlist/me` 歷史列表)刻意不下沉——前者不能讀自己正在寫的 view(`insert` 的讀取投影 CTE 因此以 `RETURNING` 為來源,見「讀取投影慣例」),後者語意上需要看到非 waiting 列。
回應形狀由 `waitlist::repository::VIEW_COLUMNS`/`VIEW_JOINS` 單一擁有,加入回應、`/waitlist/me`、admin 列表三者同形;`cancel_if_waiting_tx` 回裸 `WaitlistEntry`。
此 view 純粹是讀取收斂,不帶任何寫入、觸發或狀態轉移邏輯,不引入本詞條上述已定案排除的自動遞補。
_Avoid_: 遞補佇列(「佇列」暗示自動出隊消費,與人工遞補定案相悖;僅避自動化暗示,不避「依序」語意本身)、waiting list promotion(`Promotion` 在本系統另指 `notifications`/`posts` 的行銷促銷分類,語意不同)

**時鐘 seam(Clock Seam)**:
`utils::clock`——handler 在請求開始經 `state.clock.now()` 取樣一次,以 `now: DateTime<Utc>` 參數往下傳入 service;牆鐘語意的 service 不再自行呼叫 `Utc::now()`；非牆鐘語意站點(auth token 效期、posts 發佈時戳)為記錄在案的 carve-out。
`subscriptions` 的 entitlement 到期計算原本也列在這份 carve-out 名單裡,現已收斂進 seam——`entitlement::plan`(見「權益授予」詞條)改收 `now` 參數,不再自行讀鐘;這個 `now` 經 `grant_from_purchase_tx` 透傳,溯源仍是 checkout 自己的 handler-取樣值,故從 carve-out 名單移除。
`utils::studio_clock` 的純函式(`today`/`has_started`/…)本身不變,一樣收 `now` 參數——這層只是把「由誰取樣」從 service 上移到 handler 一層。
牆鐘語意「且」需要 studio 時區的一批 service(attendance、schedule、bookings、sessions、orders::checkout、`courses::update_course`、leave、reports 三支)不再各自收裸 `now: DateTime<Utc>`,而是收單一 `at: StudioNow`(見「工作室時鐘」詞條)——同一顆 seam,只是把「取樣哪個值」從一顆 `now` 換成一顆 `StudioNow`(tz 隨附,不必再各自呼叫 `studio_tz(server)` 或讀 `server.studio_timezone`)。
seam 往下延伸到 SQL(ADR-0017):業務時間(會被讀取拿去分桶、或拿去跟取樣時刻比較的欄位)由 repository 收 `now` 並綁進 SQL,不寫 `NOW()`;稽核時間(`updated_at` 等)刻意留給 `NOW()`,哪些欄位算稽核時間由 `tests/business_time.rs` 的 `AUDIT_COLUMNS` 持有——源碼掃描,DB 時鐘寫進清單外的欄位就失敗(ADR-0017 Addendum 2026-10-11)。
_Avoid_: 把 `studio_clock` 也算進這層 seam(它的函式簽章未變,只是呼叫端現在傳的是 handler 取樣值)

**場景時刻(Scene Instant)**:
測試裡一個場景的「現在」——`tests/common` 的 `scene_now()` = `studio_now_utc(Utc::now().trunc_subsecs(6))`:時區是測試 config 的 UTC,`now` 截到微秒(Postgres `timestamptz` 只存到微秒,截過的時刻寫進 DB 再讀回來仍相等,不必每支測試各自重新發現 `trunc_subsecs(6)`)。
`TestApp` 開場就把時鐘 seam 的測試替身 `MockClock` 釘在 `scene_now().now`;`MockClock` 一律釘住、從不讀系統時鐘,只在測試 `set`/`advance` 時移動——同一測試的每個請求取樣到同一個 `now`,`TestApp` 種的使用者(`seed_user_with_roles`/`seed_admin`/`seed_coach_user`)在 `app.studio_now().now` 加入,預期日期取 `app.today()`,測試本體與 handler 不再各讀各的牆鐘(台北跨午夜不再偶發失敗)。沒有 `TestApp` 的 service 測試用 `today_utc()`(= `scene_now().today()`)。
`MockClock` 只管 handler 經 `AppState::clock` 取樣的時刻:短期狀態與存取快取的 TTL(in-memory adapter 用 `Instant`)、JWT 效期(`Utc::now`)都不讀它。
釘住的代價:同一測試裡連續請求寫出的業務時間相同,要靠時間分出先後的斷言(依時間排序的列表、「會往前走」的時戳)在請求之間 `app.clock.advance(…)`。
_Avoid_: 在 `TestApp` 測試本體以 `Utc::now()` 推預期日期(用 `app.today()`)、手寫 `trunc_subsecs(6)`(用 `scene_now()`)

**週課表(Weekly Schedule)**:
`course_schedule_slots` 表的型別與讀取歸 `courses`(`courses::model::CourseScheduleSlot`、`courses::repository::find_slots_by_course`),**寫入歸 `sessions::calendar`**:`set_initial_schedule_tx`(`create_course`)與 `replace_weekly_schedule_tx`(`update_course`:換 slot + 對齊未來場次)是這張表僅有的兩個 runtime 寫入點,輸入列 `calendar::SlotRow` 由 `courses::service` 解析驗證後傳入(seed 直寫 slot 仍 bypass,ADR-0010)。
courses 的 create/update/get 是唯一消費端。
`sessions::calendar` 以原生 SQL 讀這張表做物化(`materialize_range`,連 `venue` 一起快照進場次)與未來場次對齊,`sessions::repository` 讀它做我的課表(`find_my_weekly_schedule`)——皆不碰 courses 的 Rust 型別;slot↔場次的對應規則(同課程、`day_of_week` = 場次日期的 DOW、同 `start_time`)只寫一次,是 calendar 的 `SLOT_OF_SESSION` 片段,物化候選與對齊的 UPDATE/DELETE 逐字套用。
今日場次(`find_today_sessions_in`)與 `reports::venue_usage` 已改讀場次自身的 `venue` 快照,不再讀這張表(見「場次物化」詞條)。
這些讀取是記錄在案的跨模組讀表慣例(與 `find_all_course_ids` 直接讀 `courses` 表同款)。
`PATCH /courses/{id}` 帶 `schedule_slots` 時,`replace_weekly_schedule_tx` 換完新一批 slots 後,同一 tx 內接著把既有未來場次對齊新週課表(見「場次物化」詞條);`create_course` 的 `set_initial_schedule_tx` 沒有這一步——新課還沒有任何場次可對齊。
_Avoid_: 把 `time_slots`(場租,見『場租(Venue Rental)』詞條)也稱作 schedule——兩者是完全不同的表。

**時段狀態(Slot Status)**:
`schedule::model::SlotStatus::derive`;依 `booked`/`capacity`/`is_closed` 純函式即時推導的四態(`available`/`limited`/`full`/`closed`),讀取時計算、不落地儲存——比照「場次狀態」詞條的裁決,`time_slots` 表已無 `status` 欄(migration `20260717000001` 收掉欄位與背後的 `slot_status` enum 型別)。`is_closed` 是管理意圖旗標(`PATCH /schedule/slots/{id}`,admin only),優先於 booked/capacity 的判斷;gate 於 `bookings::occupancy::occupy_slot_tx` 鎖列那條 statement 的 WHERE 子句(`AND is_closed = false`)——closed 時段無法再被新預約佔位,但既有預約仍可正常取消(`cancel_occupying_booking_tx` 不設 gate)。`booked` 本身也是讀時計數(`occupying_bookings`,見「場租佔位」、ADR-0015),所以四態的三個輸入中只有 `capacity`/`is_closed` 落地。
_Avoid_: 把 `bookings.status`(`BookingStatus`,`confirmed`/`cancelled`/`completed`/`no_show`,仍落地儲存的預約狀態機)與本詞條混為一談——兩者是不同表、不同語意的「狀態」。

**退款(Refund)**:
把計入營收的訂單轉到 cancelled 或 refunded 時,整單撤銷結帳留下的點數、庫存、報名與訂閱;兩個終態是同一套補償的兩個標籤(ADR-0007)。owner 是 `orders::refund`。
_Avoid_: 沖銷(那只是收回已賺點數的單一方向)、退貨(本系統沒有實體退貨流程)、刪單(訂單從不刪除)

**對話配對(Conversation Pairing)**:
`messages::pairing::resolve_pair → (member_id, coach_id)`,純函式(`orders::pricing`/`orders::fulfilment` 的姊妹),`POST /conversations` 的 member/coach 配對與自我拒斥純核 owner。自我拒斥在 `service::resolve_member_coach` 與純核各查一次——service 端先查、擋在 DB round trip 之前(自我請求必 422、不多打一次 DB),純核內重複同一檢查只是讓函式自洽,不是兩套優先序。雙角色(coach 且 member)caller 恆落 coach 側(分支順序:caller-is-coach 先判),故雙角色×雙角色的 A→B 與 B→A 是鏡像對(`(B,A)`/`(A,B)`)而非同一對;兩方向仍共享同一 conversation,由 DB 端無序 unique index(`LEAST`/`GREATEST`)保證,非本核職責。唯一 DB 依賴(`permissions_repository::find_role_names_by_user` 取對方角色)與 get-or-create/unique-violation race 收斂留在 `service`。
_Avoid_: 與 participants 授權(`authorize_participant`,`GET/POST .../messages` 等端點「呼叫者是否為此對話成員」的檢查)混同——那是既存對話的存取控制,配對是「建立/取得哪一個對話」的角色判斷,發生在對話是否存在確定之前。

**授權閘門(Authorization Gate)**:
route 層的角色檢查:admin、staff(admin 或 coach)、coach(不含 admin)三個閘門各掛在一個 router 區塊上,owner 是 `middleware::require_*`;只要求登入、不看角色的端點用 `extractors::auth::LoginRequired`。角色是封閉值域(ADR-0014)。
_Avoid_: 逐 handler 首行的角色檢查儀式、把 coach 閘門當成 staff 閘門的子集、把三個閘門做成同一個參數化 factory

**Session 生命週期(Session Lifecycle)**:
一個登入 session 由一支 refresh token 代表:簽發、輪替、單一登出、整族結束、過期清理。owner 是 `auth::session`,refresh token 表只有它寫。
_Avoid_: 在 session 模組之外直寫 refresh token、另造第二份簽發流程

**憑證檢查(Credential Check)**:
「email + 密碼 → 已驗證的帳號或 401」的判定,包含登入失敗計數與鎖定。owner 是 `auth::credentials`。
_Avoid_: 在 credentials 之外計或清登入失敗、信任呼叫端算好的密碼比對結果

**恆定成本(Constant Cost)**:
登入失敗不論帳號存不存在,都付一次 Argon2 驗證的成本,回應時間因此不洩漏帳號是否存在;只有已鎖的 email 走快速路徑。owner 是 `utils::password::verify_password`。
_Avoid_: 為查無帳號或只綁 Google 的帳號提早 return、略過 Argon2

**帳號存取(Account Access)**:
「這個 user id 現在能不能動作、帶哪些角色」的答案,`AuthUser` extractor 經快取讀它。owner 是 `auth::access`;改變答案的寫入都回 witness,commit 之後 flush。
_Avoid_: 在 access 之外改 is_active 或拼快取 key、把 assume_uncached 用在既有帳號上

**短期狀態(Ephemeral State)**:
帶 TTL 的字串與計數:路由限流桶、登入失敗與忘記密碼計數、OTP、密碼重設 token。經 `utils::ephemeral::EphemeralStore` seam 存取;key、門檻、TTL 與故障時放行或拒絕,各歸自己的 owner。
_Avoid_: 在 owner 之外拼 key、把策略放進 adapter、把短期狀態併進 AccessCache

**健康探針(Health Probe)**:
GET /health 依賴答案的 seam:Redis 通不通、開機時 Kafka producer 有沒有建成。owner 是 `health`;逾時、回應 body 與狀態碼歸 handler,adapter 不做決定。
_Avoid_: 讓 adapter 決定 healthy 或 degraded、為了 /health 把 Redis 連線放回 AppState

**AppState 組裝(AppState Assembly)**:
`AppState::new` 是 AppState 的唯一建構點:需要連線的 seam(Infra)沒有預設,呼叫端必須選 adapter;建構時不做 IO 的 seam(Overrides)留空即用正式 adapter。owner 是 `state`。
_Avoid_: 在 new 之外手寫 AppState literal、把需要連線的 seam 放進 Overrides

**上架可見性(Listing Visibility)**:
公開瀏覽只看得到上架中的商品、課程、場館與教練;已下架的明細與不存在一樣回 404。教練自己的範圍不受影響(ADR-0012)。owner 是各模組 repository 的 `find_active_by_*`。
_Avoid_: 把購物車的 400 或結帳的 422 當成這條規則沒收乾淨(三處刻意分流)

**結帳快照(Checkout Snapshot)**:
結帳在交易內鎖住並讀出的購物車內容,已下架的行也照樣帶出,要通過下架檢查(整批 422)才算可購買。讀取歸 `cart::repository`,下架檢查歸 `orders::fulfilment`。
_Avoid_: 把下架的 422 與空購物車的 400 混為一談(全部下架的購物車仍非空)

**結帳冪等(Checkout Idempotency)**:
帶 `Idempotency-Key` 的結帳以(使用者, key)為範圍去重:同一使用者重送同一把 key,拿回第一次那張訂單,不另建單。owner 是 `orders::idempotency`。
_Avoid_: 全域唯一的 key(範圍是使用者加 key)、以為任何錯誤都會重播

**帳號誕生(Account Birth)**:
新帳號誕生的那一步:建 users 列、授予 member 角色、排入 user_registered 事件。owner 是 `auth::provisioning::create_account`;Google 首登與 seed 刻意不經它,理由在該 module doc。
_Avoid_: 在登入路徑上冪等地重授 member、把 Google 首登或 seed 算成它的呼叫端

**Google 身分(Google Identity)**:
一個 Google 授權碼對應、已通過全部驗證的 Google 帳號(sub、email、name、picture)。owner 是 `utils::google_oauth`;之後的連結決策與 session 簽發是登入規則,不屬本詞條。
_Avoid_: 在 adapter 之外解析或驗證 id_token、讓 service 讀 claims

**studio 時間軸 SQL 攣生(Studio Timeline SQL Twin)**:
`reports::repository` 16 個查詢站點裡,「先把 UTC 的 `now` 換成 studio 牆鐘、再截斷/轉型」這條算式曾各自內嵌(`studio_month_anchor` 這個「本月 anchor」idiom 11 次、`studio_today` 這個「今天」idiom 5 次),與 `utils::studio_clock::{month_key, today}` 是同一條規則的兩份手抄副本(Rust 一份、SQL 16 處各一份)。
migration `20260803000001` 把 SQL 側收斂成兩個 STABLE SQL function(`studio_month_anchor(now, tz)`/`studio_today(now, tz)`),16 個站點改呼叫這兩個 function,不再各自內嵌;STABLE 而非 IMMUTABLE 是因為 tzdata 本身可變,不是因為讀了當下時間(`now`/`tz` 仍是呼叫端參數)。
Rust 與 SQL 兩份定義沒有被合併成一份(語言邊界擺在那裡,合不了),而是靠 cross-test 錨定彼此:`tests/service_reports.rs` 的 `month_key_matches_sql_studio_month_anchor`/`today_matches_sql_studio_today` 對同一組跨時區樣本(Asia/Taipei 月界前後一秒、America/New_York 逆向跨月、UTC)分別跑 Rust 版與 SQL 版,斷言逐一相等——與 `orders::model` 的 `revenue_predicate_matches_revenue_statuses_array`(錨定 `is_revenue()` 對 `REVENUE_STATUSES`)同一手法。
窗長(11 個月/30 天/90 天……)不在這兩個 function 之內,維持 Rust/呼叫端常數。
10 個收 `(now, tz_name)` 的 repository 函式(`revenue_trend`/`member_stats`/`kpis`/`income_by_source`/`payment_split`/`age_distribution`/`coach_reports`/`retention`/`funnel`/`weekday_load`)簽章改收單一 `at: StudioNow`(`[months]` window 參數仍獨立),內部 `.bind(at.now).bind(at.tz.name())`——只是呼叫慣例的容器化,兩個 STABLE SQL function 與 16 個查詢站點的攣生關係、cross-test 錨定手法皆不變。
_Avoid_: 誤以為這兩個 function 把 Rust 版也取代掉了(兩份手抄依然並存,只是現在互相錨定,不是各自維持同步的信任假設)。

**報表組裝(Report Assembly)**:
`reports::assembly::assemble_admin_report`(`AdminReportInputs → AdminReportResponse`)是 `GET /reports/admin` 的欄位塑形/推導純核——`service::admin_report` 原本 13 個循序 repository 查詢之後緊接的每一條 ratio/position-index/filter/split 規則,原本 inline 寫在 service 裡,現在收進這裡:zero IO、owned output、`#[cfg(test)]` 錨測試,與 `orders::pricing`/`orders::fulfilment`/`subscriptions::entitlement` 同一紀律,是這個純核姊妹家族的第六個成員(pricing → fulfilment → marking::plan → pairing::resolve_pair → entitlement::plan → assembly::assemble_admin_report)。
`current_month_key` 刻意是獨立參數而非 `AdminReportInputs` 的欄位——「`StudioNow` 不進純核」:它衍生自呼叫端取樣的 `at: StudioNow`(其 `now` 分量),擺在 struct 外才能讓 `AdminReportInputs` 每個欄位都單純是 repository 查詢結果,不混入 clock/tz 語意。
`safe_ratio`(count-over-count、分母 0 回 `None` 而非 NaN/Infinity)是三種比率計算與 `category_split` 共用的同一個零安全 helper。
收入來源值域(`course`/`ticket`/`membership`/`course_package`/`merchandise`/`venue_rental`)的單一 owner 是 `reports::model::IncomeSource`:`ALL` 即顯示序(`repository::income_by_source` 以 `unnest($6::text[]) WITH ORDINALITY` 綁它做零填補與排序)、`from_product_type` 窮舉 `ProductType`、`is_order_line()` 決定 `category_split` 是否納入(場租不是訂單明細);DTO 的 `source` 仍是 `String`。
_Avoid_: 把 `current_month_key` 塞回 `AdminReportInputs` 的欄位(那會讓這個純輸入 struct 混入一個衍生自 clock 的值,污染「每個欄位都是查詢結果」的不變量)。
_Avoid_: 把報表區段(`kpis`/`revenue_breakdown`/…各 section)「只宣告一次」收斂掉——區段目前在三處各宣告一次:`AdminReportResponse`(wire 形狀,`dto.rs`)、`AdminReportInputs`(每個欄位是一個 repository 查詢結果)、`assemble_admin_report` 的組裝處(把輸入塑形成輸出)。三者各自是不同的東西(輸出契約/查詢結果/塑形規則),缺一個區段時編譯器會在另外兩處逼出錯誤,這份重複是刻意維持的;用巨集或共用型別把它壓成一處,會把「查詢結果」與「wire 形狀」綁死,讓純核無法獨立演進。2026-10-03 架構檢視提過、裁決維持現狀(Controller 裁決 10),未來檢視勿重提。

**權益授予(Entitlement Grant)**:
買會籍或票券時,在寫入當下算出買家得到什麼:堂數額度、到期日、兩者皆有,或無期無額;之後算不算有效另由讀時推導(ADR-0003)。owner 是 `subscriptions::entitlement`。
_Avoid_: 開通、訂閱狀態(那是讀時推導,不是授予)

**唯一鍵衝突映射(Unique-Conflict Mapping)**:
DB 唯一或排他約束違例翻成 409 的那一層:判準是約束本身,不是寫入前的 SELECT 預檢。owner 是 `AppError::conflict_on_unique` 這一組函式。
_Avoid_: 在新的唯一性檢查前補 SELECT 預檢(TOCTOU)、假設 coaches 的 slug 也大小寫不敏感

**跨模組讀表(Cross-Module Table Reads)**:
模組直接 SELECT 或 JOIN 另一模組的表,不繞經對方的 repository:讀開放、寫歸戶,寫入仍只經該表的 owner。讀開放不代表可以拿另一模組的現況重建歷史事實,那種值要在寫入時快照(見「場次物化」)。
_Avoid_: 為了跨模組讀再包一層轉手 repository 或 wrapper(ADR-0005)

**執行環境(AppEnv)**:
這個 process 以哪個部署層級執行(APP_ENV,如 development、production),讀取與比對都大小寫不敏感。owner 是 `config::AppEnv`。
_Avoid_: 與 config crate 的 `config::Environment` 混同(那是設定來源,與部署層級無關)

**事件派送(Outbox Dispatch)**:
把 outbox 裡的事件送上 Kafka:認領、在交易外發送、用短交易記帳;失敗列退避重試,沒有終止狀態。owner 是 `kafka::outbox::drain_once`,重試政策見 ADR-0013。
_Avoid_: 在交易裡 await publish、替 outbox 加放棄或死信狀態(outbox 是事件唯一的來源)

**消費迴圈(Consumer Loop)**:
audit consumer 的主迴圈:一次解決一則訊息,暫時性失敗對同一則原地有限重試,解決之後才 commit offset。owner 是 `kafka::consumer`,重試政策見 ADR-0013。
_Avoid_: 暫時性失敗先跳過、讀下一則(後續 commit 會越過失敗那則,等於丟掉)、退避中收到 shutdown 時 commit

**Wire enum 值域(Wire Enum Domain)**:
一個 wire enum 允許的值:由各型別的 `ALL` 與 `as_str` 單一擁有,FromStr、422 的允許值文字與跨語言一致性測試都從它推導;大小寫政策逐型別固定。
_Avoid_: 把 leave 的核決動詞解析器,或 SQL 綁定用的狀態陣列(如「營收狀態集」),當成 wire enum 值域
