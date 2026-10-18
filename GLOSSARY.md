# Dream Fly

Dream Fly 工作室預約與商務後端的領域語言。於 notifications 接縫架構審查(2026-06)首次建立,後續解析的術語陸續補入。

## Language

**Notification**:
持久化到 `notifications` 表、只顯示給單一使用者的 in-app 訊息,在某個領域動作 commit 之後以 best-effort 寫入;絕不阻擋或回滾觸發它的動作。「commit 之後」由 `PendingNotification`(`#[must_use]`,`.deliver(db)` 唯一 IO 入口)提醒,時機本身仍是位置慣例——是否收進型別系統已裁決不做,封存於 ADR-0009,重開條件見該 ADR。目前所有交付站點皆遵循「tx 內完成領域寫入 → commit → deliver」標準形,無例外站點(單一語句寫入如 `certificates::create_certificate` 的 CTE insert 自動 commit,等同此形)。通知文案需要的欄位取自寫入端回傳的讀取投影列(如證書通知標題取 `row.title`,見「讀取投影慣例」),不另查、不回頭讀請求。
_Avoid_: alert, push(本系統無外部推播通道), message

**Event**:
描述業務事實(如 order created、user registered)的 outbox/Kafka 紀錄,在業務交易內寫入,供 audit 與外部整合。非使用者可見——Event 不是 Notification。
_Avoid_: notification, message

**工作室時鐘(Studio Clock)**:
工作室牆鐘的時間語意:場次與時段的日期、時間都是工作室時區的牆鐘值,「今天」是工作室當地的日曆日(契約 §3.18 裁決 2)。取樣一次的時刻配上時區是 `StudioNow`,owner 是 `utils::studio_clock`。
_Avoid_: 伺服器時間、拿 UTC 日期當「今天」、沒說是誰的「本地時間」

**課程教練所有權(Course-Coach Ownership)**:
`coaches::service::resolve/require_course_coach`;三態政策=所有權 gate 403 / 範圍列表空集合 / 儀表板 404。
所有權 gate 有**第二形狀**——教練—學員**關係** gate(契約 §3.22「教過此生,active 或 cancelled 皆算」),刻意 inline 於 `certificates::service::create_certificate`(src/modules/certificates/service.rs:64-74:`coaches::service::resolve` + `user_has_enrolment_with_coach` EXISTS 查詢),**不歸戶 coaches 模組**——單一呼叫端,依 ADR-0005 判準,為它建姊妹 helper 是淺模組;出現第二個「教練發給自己學員」類端點時再歸戶。
對照:單課 gate `require_course_coach`(coaches/service.rs:45-65)已歸戶,`create_report_card`(certificates/service.rs:28)是其消費端。
與「教練範圍」是不同維度:這裡問的是單一資源的教練 gate(這個人是不是這堂課的教練),範圍列表本身要不要含已下架課程另見該詞條與 ADR-0012。
_Avoid_: 把這條關係 gate 與單課 gate `require_course_coach` 混同

**教練範圍(Coach Scope)**:
「教練名下的課程/學員」= `courses.coach_id` 指向該教練的**全部**課程,不論 `is_active`;範圍下的學員 = 這些
課程 `active_enrolments` 去重後的 distinct 使用者集合。
七個讀取端(`GET /coaches/me/students`、`GET /reports/coach` 的 `student_count`/`today_sessions`/`pending_attendance`/`attendance_rate_30d`、`GET /reports/admin` 的 `coaches[].course_count`/`student_count`、教練身分呼叫 `GET /leave-requests`、教練身分呼叫 `GET /sessions/today`(`sessions::service::today_sessions`))一律同一口徑,由跨面交叉測試(`tests/service_reports.rs::coach_scope_includes_delisted_courses_on_every_surface`,另錨定 `attendance_rate_30d`、admin `coaches[].attendance_rate`/`revenue_cents_12m`)錨定,不靠共用 view/SQL 片段。**Rust 一次解析**:`coaches::service::resolve_scope(db, auth) -> Option<CoachScope>`(欄位私有,唯一建構點;`coaches::repository::find_course_ids_by_coach` 是全站唯一寫 `courses.coach_id = $1` 的語句)把名下課程 id 集合解析一次,教練身分的讀取端——`attendance::find_my_students`、`reports::coach_attendance_in_range`、`leave` admin 列表 filter(`$3::uuid[] IS NULL OR c.id = ANY($3)`)、`sessions::today_sessions` 教練分支、`reports::coach_report`(解析一次傳到底,`materialize_today` 與其餘讀取共用同一份)——只收 `&CoachScope` 並綁 `= ANY($n)`。
admin 側 `reports::coach_reports`(以 `co.id` 逐教練的相關子查詢)、證書關係 gate、請假 admin 分支不在此列。
`attendance::repository` 曾在其中兩處多帶 `AND c.is_active = true`,是漂移點,已刪(ADR-0012)。
與「課
程教練所有權」不同維度(那是單資源 403 gate,這裡是名下範圍列表);與「上架可見性」也不同維度(下架只影響
訪客/會員瀏覽端看不看得到、買不買得到,不影響教練自己或 admin 代管視角看到的範圍)。
_Avoid_: 把某堂課下架當成把它從教練名下移除的手段(要移除得取消報名或改派 `courses.coach_id`);替單一讀取
端加回 `is_active` 過濾去「修正」跟其他讀取端對不上的數字(七處本就該同口徑)。

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
admin 報表的留存段:近 6 個工作室月裡,會員當月有出席就算活躍,再分成首次活躍與回訪。量的是有沒有回來上課,不是續買或續卡(ADR-0003)。owner 是 `reports::repository`。
_Avoid_: 續訂率、回訪率、churn(本系統不計流失率)

**漏斗(Funnel)**:
admin 報表的漏斗段:近 90 個工作室天的試上預約數與新報名數,只有這兩個原始計數,不造中間段、不算轉換率。owner 是 `reports::repository`。
_Avoid_: 轉換率(後端沒有這個欄位)、行銷漏斗(這裡只是兩個計數)

**場租(Venue Rental)**:
出租場地時段的產品線:使用者預訂某個場地時段,是與課程報名分開的另一個營收來源。只有單一場館,沒有分校維度(ADR-0004)。owner 是 `bookings`。
_Avoid_: 包場、分校營收、campusRevenue、訂場(那是動作,這裡指產品線)

**場租佔位(Venue-Rental Occupancy)**:
一筆場租預約佔不佔它時段的一個座位:只有已取消的不佔。佔用數讀取時計數、不落地(ADR-0015)。owner 是 `bookings::occupancy`。
_Avoid_: 與課程的「座位」混為一談(不同產品線)、把佔用數存回時段欄位、在 SQL 另寫一份佔位狀態清單

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
想試上一堂課的意向登記:洽詢表裡類型為 trial 的一列,結構化欄位存成不逐欄驗證的 JSON。不是場地預訂。owner 是 `contact`。
_Avoid_: 試聽(課程用語是「上課」)、trial booking(容易被誤會成場租預約)、試聽預約

**系統設定(Settings)**:
admin 可讀寫的全域 key-value 設定,給後台的系統設定頁用。不是會員自己的偏好,也不是伺服器啟動期的組態。owner 是 `settings`。
_Avoid_: 偏好設定(那是每位會員自己的 preferences)、組態/config(那是啟動期的 AppConfig)、設定檔(這是資料表)

**場次狀態(Session Status)**:
場次依工作室牆鐘即時推導的三態:upcoming、ongoing、done。讀取時計算、不落地。owner 是 `sessions::model::SessionStatus`。
_Avoid_: state、live/done

**座位(Seats)**:
課程或場次還有沒有位子:課程看名額與有效報名數,場次另計請假釋出與補課佔用(契約 §3.20)。owner 是 `courses::seats`。
_Avoid_: capacity(那是場租時段的欄位)、quota

**出席口徑(Countable Attendance)**:
出席統計計入的出勤紀錄:出席與缺席計入分母、出席是分子,請假不計。owner 是 view `countable_attendance`。
_Avoid_: 出勤率(那是算出來的比率,不是口徑本身)、attendance_records(那是底表,口徑在 view)

**點名計畫(Marking Plan)**:
批次點名寫入前的純驗證:狀態能否解析、名單是否都是本課的有效報名、有沒有覆寫已核准的請假,任一不過就整批 422。owner 是 `attendance::marking`。
_Avoid_: 與「出席口徑」混同(那是讀取統計的口徑,這裡是寫入前的驗證)

**請假(Leave)**:
會員對已報名課程某一場次的請假申請,由該課教練或 admin 審核。核准時把該場次出勤投影成請假,而且核准恆勝:點名不能覆寫已核准的請假(ADR-0008)。owner 是 `leave::service`。
_Avoid_: 把核准投影出的請假出勤與口頭請假當成同一種、把核准恆勝讀成雙向覆寫

**請假規則(Leave Rules)**:
審核請假與預約補課前的純判斷:這次審核是否合法、假單能不能約補課、補課場次合不合格,不碰資料庫。owner 是 `leave::rules`。
_Avoid_: 與「請假」混同(那是核准之後的投影與守衛)

**請假讀取投影(Leave Read Projection)**:
請假單回應的單一形狀:會員與教練的列表讀取,和建立、審核、約補課的寫入回應,都組出同一份投影。owner 是 `leave::repository`,做法見「讀取投影慣例」。
_Avoid_: 把審核與補課用的決策讀取當成這份投影(它們刻意不進)

**讀取投影慣例(Read Projection Idiom)**:
某資源的回應長什麼樣,由 repository 裡一對欄位與 JOIN 片段單一擁有,讀取端與「回應即該列」的寫入端都組出同一份投影。原型與規則在 `leave::repository`。
_Avoid_: 寫入後再讀一次、從請求欄位手拼回應、在寫入的 CTE 裡讀 view

**場次物化(Session Materialization)**:
把週課表展開成實際的場次列,只往前、不回填過去(ADR-0011)。讀場次之前要先物化,由物化回傳的見證型別擔保。owner 是 `sessions::calendar`。
_Avoid_: 把見證當成課程範圍過濾的保證(它只保證已物化)

**有效報名(Active Enrolments)**:
目前佔用座位的報名(狀態 active)。報名事件數、以及要看到已取消報名的歷史讀取,都不是這個口徑。owner 是 view `active_enrolments`。
_Avoid_: 現役報名、未取消報名

**候補(Waitlist)**:
課程滿班時依加入序排列的諮詢名單。名額釋出時不自動遞補、不通知,由 admin 依序人工聯絡,候補者自己去結帳(ADR-0006)。owner 是 `waitlist`。
_Avoid_: 遞補佇列(暗示自動出隊)、waiting list promotion(Promotion 在本系統指行銷促銷)

**時鐘 seam(Clock Seam)**:
handler 在請求開始取樣一次的「現在」,當參數往下傳給 service;牆鐘語意的業務邏輯不自己讀鐘,業務時間連 SQL 也綁這個值(ADR-0017)。owner 是 `utils::clock`。
_Avoid_: 把工作室時鐘也算進這層 seam(它只收呼叫端傳入的值)

**場景時刻(Scene Instant)**:
測試裡一個場景的「現在」:截到微秒的 UTC 時刻。TestApp 開場就把時鐘釘在這裡,測試本體與 handler 讀同一個現在。owner 是 `common::scene_now`。
_Avoid_: 在 TestApp 測試本體用系統時鐘推預期日期(用 app.today())、手寫微秒截斷

**週課表(Weekly Schedule)**:
課程每週固定的上課時段(星期幾、幾點、場地),場次由它物化而來。型別與讀取歸 `courses`,寫入歸 `sessions::calendar`。
_Avoid_: 把場租時段也叫 schedule(那是另一張表)

**時段狀態(Slot Status)**:
場租時段依佔用數、容量與關閉旗標即時推導的四態:available、limited、full、closed。讀取時計算、不落地。owner 是 `schedule::model::SlotStatus`。
_Avoid_: 與預約狀態 BookingStatus 混為一談(那是落地的預約狀態機)

**退款(Refund)**:
把計入營收的訂單轉到 cancelled 或 refunded 時,整單撤銷結帳留下的點數、庫存、報名與訂閱;兩個終態是同一套補償的兩個標籤(ADR-0007)。owner 是 `orders::refund`。
_Avoid_: 沖銷(那只是收回已賺點數的單一方向)、退貨(本系統沒有實體退貨流程)、刪單(訂單從不刪除)

**對話配對(Conversation Pairing)**:
建立或取得對話時,判斷雙方誰是會員側、誰是教練側,並拒絕跟自己開對話的純核。owner 是 `messages::pairing`。
_Avoid_: 與對話成員授權混同(那是既有對話的存取控制)

**授權閘門(Authorization Gate)**:
route 層單點角色檢查家族——`middleware::require_admin`/`require_staff`/`require_coach`,取代逐 handler 首行 `auth.require_role`/`require_any_role` 儀式。
`startup.rs` 依角色層級分三個 router 區塊(`admin_api`/`staff_api`/`coach_api`),各自 merge 對應模組的 `admin_router()`/`staff_router()`/`coach_router()` 後掛一個 `route_layer`。
三個閘門共用同一兩步 fail-closed 結構(先 401 平價的 token 驗證,再 403 平價的角色判斷,任一失敗 `next` 不執行),差異僅角色集合:`require_admin` = `Role::Admin`;`require_staff` = `Role::Admin` 或 `Role::Coach`;`require_coach` = 僅 `Role::Coach`(admin 刻意排除,不是 `require_staff` 的子集)。
角色是封閉值域 `permissions::model::Role`(`Admin`/`Coach`/`Member`/`Guest`,`ALL` + `as_str`,ADR-0014):`AuthUser::has`/`require_role`/`require_any_role` 只收 `Role`,`AuthUser.roles` 仍是 `Vec<String>`(wire 與 access cache 形狀),比較經 `as_str`;`Role::ALL` 與 `roles` 表列的一致由 `tests/wire_enums.rs::role_all_matches_roles_table` 釘住。
沒有改角色的端點(`/roles` 與從未被讀的 `permissions`/`role_permissions`/`permission_conditions` 已刪),授予只經 `permissions::repository::assign_role(conn, user, Role)`——角色列不存在回 `RowNotFound`,重複授予仍 Ok。
驗證通過者把 `AuthUser` 注入 request extensions,`extractors::auth` 的 fast path 命中即 clone 回傳,不重打帳號存取(`auth::access::resolve`,見「帳號存取」)。
Request-data-dependent 的細粒度檢查(`require_course_coach`、`is_admin()` 分支)不屬此類,留在 service;碩果僅存的 handler-level 例外只剩 `rewards::list` 的條件式 `?all=true` 閘門(依賴 query 參數,本質不可上移)。
第四個訊號 `extractors::auth::LoginRequired`(newtype 包 `AuthUser`)補的是「需登入、不看角色」這一類——不掛三個 route_layer 之一的 handler,首行若仍是裸 `AuthUser` 參數,讀者無法從簽章分辨這是「刻意只要求登入」還是「忘了寫角色檢查」;換成具名的 `_login: LoginRequired` 參數,登入閘門本身變成簽章可見的宣告,與已刪除的 37 個儀式 `_auth`/`require_role` 呼叫區隔(目前兩站:`GET /coupons/{code}/validate`、`GET /courses/{id}/sessions`)。`tests/http_authorization_gates.rs` 是這一組(三個 route_layer 閘門 + `LoginRequired`)的回歸網——每個閘門挑一個代表端點釘住「無 token / 錯角色 / 對角色」三態的 shape,不逐 handler 複查(逐 handler 角色覆蓋率留在各自 `tests/http_*.rs`);`tests/middleware_auth_extractor.rs` 覆蓋的是 extractor 本身的 token 細節(過期/偽造/停用帳號),兩份測試檔分工不重疊。
_Avoid_: 把三個閘門實作成同一個參數化 factory(刻意各自獨立函式,見 `require_staff`/`require_coach` 檔頭)、把 `require_coach` 誤認為 `require_staff` 的簡化版或子集(語意不同——後者含 admin bypass,前者不含)。

**Session 生命週期(Session Lifecycle)**:
`auth::session` 是 `refresh_tokens` 表的單一 owner 與唯一寫入者——簽發(`start`)、輪替(`rotate`)、單一登出(`end`)、整族撤銷(`end_all`)、過期清理(`purge_expired`)五個動作與這張表的全部 SQL 同檔且私有(比照 `bookings::occupancy`);`auth::service::refresh_token`/`logout` 只是一行轉呼叫(ADR-0005 的 handler→service 形狀),`reset_password` 與 `main.rs` 的每小時清理也改經此模組。
`end_all` 只動 DB、收成 `pub(super)`(auth 之外的停用一律經 `auth::access::deactivate_tx`,快取失效由它回的 `AccessDirty` 在 commit 後 flush),且是**刪列**而非標記 revoked——revoked 列會保留到過期供 reuse detection 用,若 `end_all` 只標記,改密碼/停用再啟用後使用者重新登入,舊裝置拿舊 token refresh 就會被誤判為重放,連帶殺掉新 session 並發出假的 `refresh_token_reuse` 告警;刪列後舊 token 只得一般 401。
三個呼叫站:輪替的 reuse/停用分支、`reset_password`、admin 停用帳號(`access::deactivate_tx`;`users::service::admin_update_user` 在 `is_active: false` 時與欄位 update 同一 tx 呼叫——停用即結束所有 session,重新啟用不會讓舊 refresh token 復活)。
輪替先以 `FOR SHARE` 鎖 user 列、再鎖 token 列(user-first,與停用、`reset_password` 同序):進行中的停用會讓並發的 refresh 等它 commit,再看到 `is_active = false` 而拒絕,不會漏出一支停用後仍有效的新 token。
Retention 規則:`purge_expired`(`main.rs` 每小時)只刪 `expires_at < NOW()` 的列,已 revoke 但未過期的列(只有輪替與單一登出會產生)刻意保留到過期——reuse detection 靠這些列辨認被重放的已輪替 token,列若先被刪,重放只會得到不觸發整族撤銷的一般 401;列過期後 JWT 自身的 `exp` 已先擋下(只差驗證器 5 秒 leeway),列才沒有保留價值(對應 index 為 full `idx_refresh_tokens_expires_at`,migration `20260926000002`)。
`start` 是簽發 access/refresh token 對的單一 owner——register、login、google_auth、refresh_token 四路共用同一份簽發儀式(舊 `build_auth_response` 雙胞胎已刪)。
三個 invariant 由它獨力維護:refresh token 進庫前必雜湊(`jwt::hash_token`,SHA-256,DB 外洩不直接洩漏可用憑證)、access/refresh 恆成對簽發(不存在只發一邊的中間態)、`expires_at` = 簽發當下 `now + jwt_refresh_expiration_days`(auth token 效期是「時鐘 seam(Clock Seam)」詞條記錄在案的 carve-out,直呼 `Utc::now()` 而非經 handler 取樣)。
`start` 第一行擋 `!user.is_active` → 401:停用帳號在任何簽發路徑都拿不到 session(曾經 google_auth 無此檢查);`login` 的 is_active 檢查現在收在 `credentials::judge`(見「憑證檢查」),順序仍先於清 lockout 計數與非交易的 `update_last_login`。
呼叫端負責 conn/tx 邊界——函式只收 `&mut PgConnection`:register/google_auth/refresh_token 三路在既有交易內呼叫(refresh 簽發已原子化於同一 tx,新 token 簽發與舊 token revoke 同進同出),login 則走一般 pooled 連線。
簽發路徑的 is_active 檢查刻意直讀 DB 列、不經帳號存取快取(見「帳號存取」)。
_Avoid_: 另造第二份簽發儀式(舊 `build_auth_response` 雙胞胎已刪,勿復辟)、在 `auth::session` 之外直寫 `refresh_tokens`

**憑證檢查(Credential Check)**:
`auth::credentials::verify(db, store, email, password) -> Result<User, AppError>` 是「email + 密碼 → 已驗證 `User` 或 401」的單一 owner;`email` 須由呼叫端(`service::login`)先經 `model::normalize_email`。內部收成「鎖定檢查 → 查帳號 → Argon2 驗證 → `judge`(純函式)→ 收尾」四步,`judge(user: Option<User>, matched: bool) -> Verdict`(`Verdict::Accept(User)` / `Verdict::Reject { counts: bool }`)把「查到什麼、密碼對不對」收斂成一個判決,不信任呼叫端算出的 `matched`——沒有 `password_hash` 的帳號即使 `matched` 誤傳 `true` 仍然 `Reject`。七種情境(k = `login_fail:{email}`,由 `auth::rate_limit` 私有維護)的 store 呼叫與計數行為釘在 `credentials::tests::verify_counts_each_401_branch_exactly_once`:

| 情境 | store 呼叫 | 計數/清除 | 結果 |
|---|---|---|---|
| 查無 email | `Get`、`Incr` | 計入失敗 | 401 |
| 只綁 Google(無 hash) | `Get`、`Incr` | 計入失敗 | 401 |
| 密碼錯 | `Get`、`Incr` | 計入失敗 | 401 |
| 停用 + 密碼錯 | `Get`、`Incr` | 計入失敗 | 401 |
| 停用 + 密碼對 | `Get` | 不計、不清 | 401 |
| 已鎖(`login_fail:{email} >= 10`) | `Get` | 不查 DB、不驗證 | 401 |
| 成功 | `Get`、`Del` | 清計數 | 200 |

**恆定成本(bug #1 修正,見「全輪可見的行為變更」1)**:`utils::password::verify_password(password, hash: Option<String>)` 沒有跳過驗證的分支——`hash: None`(查無帳號、只綁 Google)一律對一支懶初始化的假雜湊(`DUMMY_HASH`,同一個私有 `argon2()` 生成,`tokio::sync::OnceCell` 只算一次)驗一次,並恆回 `Ok(false)`,回應時間因此不再洩漏「這個帳號存不存在」。
決定性單測:`absent_hash_never_verifies`(即使猜中假雜湊背後的明文仍是 `false`)、`dummy_hash_carries_pinned_params`(解析假雜湊 PHC,algorithm/version/m/t/p 與 `argon2().params()` 一致)。
wall-clock 回歸釘在 `tests/service_auth.rs::login_miss_costs_one_argon2_verification`(三條路徑各熱身 1 次、量 3 次取最小值,4 倍餘裕、連跑 20 次確認不偽紅)。**鎖定分支刻意是唯一的快速路徑**——已鎖的 email 不查 DB、不跑 Argon2——已鎖的 key 不能被拿來反覆觸發 Argon2 放大 CPU 成本,只洩漏「這個 key 被鎖了」,不洩漏「這個帳號存不存在」(不存在的 email 一樣會被鎖進同一個計數器)。
停用帳號密碼驗證正確時不計入失敗計數:帳號本身沒問題,計了只會冤枉地把它鎖死。
_Avoid_: 在 `credentials` 之外呼叫 `rate_limit::record_login_failure`/`clear_login_failures`(這兩支的唯一呼叫端見「短期狀態」)、為「查無帳號」或「只綁 Google」提早 return 略過 Argon2(bug #1 的成因,回應時間會洩漏帳號是否存在)。

**帳號存取(Account Access)**:
`auth::access` 是「這個 user id 現在能不能動作、帶哪些角色」的單一 owner。
讀:`resolve(db, cache, user_id) → Option<Vec<String>>`(`None` = 停用或不存在),是 `AuthUser` extractor 在 fast path 未命中時唯一的依賴(extractor 只剩 fast path + bearer + JWT + `AuthUser`/`LoginRequired`)。
快取的 key 格式(`user_active:{id}`/`user_roles:{id}`)、TTL(is_active 60s、角色 900s)、空角色 sentinel `"\0"`、快取錯誤一律當 miss 回 DB(fail-open——快取故障不會把所有人鎖在門外)全部私有於本模組;角色的 DB fallback 重用 `permissions::repository::find_role_names_by_user`(與 `UserResponse.roles` 同一條查詢,extractor 的抄本已刪)。
寫:每個改變答案的寫入都回 `#[must_use] AccessDirty` witness——permissions repository 的 `assign_role`(唯一的 `user_roles` 寫入)、`deactivate_tx`(`is_active = false` + `session::end_all`,user 列先鎖)、`reactivate_tx`;commit **之後** `flush(cache)` 一次 DEL 兩鍵(commit 前 flush 會讓 DEL 與 commit 之間的請求把舊值填回)。
`assume_uncached()` 是可 grep 的具名 no-op,給「列在本 tx 內剛建立、不可能有快取」的站點:`provisioning::create_account`、測試 fixture;seed 則是刻意放行的開發限定例外——重跑 seed 時目標使用者未必是本 tx 新建,快取可能已存在殘留值,只是容忍它活到 TTL 到期(60s/900s)自我修正,而非真的「不可能有快取」。
google_auth 的 Create 分支**保留** flush——`ON CONFLICT (google_id)` upsert 可能落在併發首登已 commit(甚至已被讀進快取)的列。
快取本身是 `AccessCache` seam(只存字串、回 `Result`,策略全在 access):正式環境 `RedisAccessCache`(`AppState.access_cache`,由 `Infra::redis` 建構,見「AppState 組裝」),HTTP 測試 harness 接 `InMemoryAccessCache`(`tests/common/mocks.rs`,`Instant` 惰性到期);`tests/service_access.rs` 對兩個 adapter 跑同一組情境,並以 `FailingAccessCache` 釘住 fail-open。
限流、OTP、reset token 這類短期狀態走並列的 `EphemeralStore` seam,不經此 seam(見「短期狀態」)。
已知殘留 race(read-then-refill):並發的 `resolve` 可能在 `flush` 的 DEL 之前讀到 DB 舊值、之後才 `SET EX` 寫回,舊答案活到 TTL 滿(停用最多 60s、角色最多 900s);witness 只關掉「根本沒人 invalidate」這一類 bug,不關這個窗口。`reset_password` 不 flush 是正確的:它只換密碼 + `end_all`,`is_active` 與角色都沒變,快取的答案仍然正確(既有 access token 的存活由 JWT `exp` 決定,不歸本模組)。簽發檢查(`credentials::judge` 的 is_active、`session::start` 首行、`rotate` 以 `FOR SHARE` 讀的 user 列)刻意直讀 DB 列而不經 `resolve`——簽發要的是當下 committed 的真值與 user-first 鎖序,不是可容忍 60s 陳舊的快取答案。
_Avoid_: 在 access 之外拼 `user_roles:`/`user_active:` key 或直接 DEL(測試也改走 `resolve`)、把 `assume_uncached` 用在既有帳號上、在 access 之外直接 `UPDATE users SET is_active`(停用必經 `deactivate_tx`,否則 session 不會結束、也不會產生 witness)。

**短期狀態(Ephemeral State)**:
帶 TTL 的字串與計數,一律經 `utils::ephemeral::EphemeralStore` seam(`incr_with_ttl`/`get`/`set_ex`/`del`/`getdel`;只存字串、回 `Result`,incr 的 TTL 只在建立 key 那一次設定——Redis 版是一支 Lua script,原子)。
port 與正式 adapter `RedisEphemeralStore` 同檔(比照 `utils::email`);放在 `utils` 而非 auth,是因為 middleware 不能依賴 auth。
五類 key 各歸一個 owner,key 格式、門檻、TTL、儲存故障時放行還是拒絕,全部私有於 owner,adapter 不做決定:路由限流桶 `rate_limit:global:{ip}`(300/60s)與 `rate_limit:auth:{ip}`(10/60s)歸 `middleware::rate_limit`,**fail-closed**(500 `{"error":"internal server error"}`);登入失敗計數 `login_fail:{email}`(10 次鎖 900s、成功即清)歸 `auth::rate_limit`,**fail-open**(讀不到當 0,計數與清除靜默放棄——儲存故障不能讓所有人登不進來),三支函式(`login_locked_out`/`record_login_failure`/`clear_login_failures`)唯一呼叫端是 `auth::credentials`(見「憑證檢查」);忘記密碼計數 `forgot_rate:{email}`(每小時 3 封,超過靜默吞掉)同歸 `auth::rate_limit`,**fail-open**(計數失敗當作未超量);OTP 的碼 `otp:{user_id}`(300s)、請求數 `otp_rate:{user_id}`(每小時 3 次)、嘗試數 `otp_attempts:{user_id}`(第 6 次起作廢碼,重寄才重來)歸 `auth::otp`,**fail-closed**(INCR/SET/GET/DEL 任一失敗都 500,不寄簡訊——簡訊要錢、嘗試數要擋暴力);密碼重設 token `password_reset:{token}` 與索引 `password_reset_current:{user_id}`(900s、GETDEL 單次消費、重寄作廢前一個)歸 `auth::reset_tokens`,**fail-closed**。
後兩條疊起來的結果:儲存故障時忘記密碼對已知 email 回 500(計數放行、簽發失敗),未知 email 在碰儲存之前就回 200。
與 `AccessCache` 並列而不合併:access 用不到 incr/getdel,合併只會加寬它的 interface;兩個 Redis adapter 與健康探針共用同一個 `ConnectionManager`(`Infra::redis` 各包一份,見「AppState 組裝」)。
測試:`tests/service_ephemeral.rs` 以 `both_adapters!` 對 Redis 與 `InMemoryEphemeralStore`(`tests/common/mocks.rs`,`Instant` 惰性到期)跑同一組情境;`FailingEphemeralStore` 在 `service_auth.rs`/`middleware_rate_limit.rs` 釘住各 owner 的 open/closed;HTTP harness 每個 `TestApp` 各一份 in-memory store(`spawn_test_app_with_store` 可換),HTTP 測試不再經過 Redis 的限流/OTP/reset 路徑。
_Avoid_: 在 owner 之外拼這些 key 或直接呼叫 store、把門檻或 fail-open/closed 放進 adapter、把短期狀態併進 `AccessCache`(或反過來)、在 handler/service 裡繞過 seam 直接對 Redis 下指令(`AppState` 已不持有 Redis 連線)。

**健康探針(Health Probe)**:
`health::HealthProbe` 是 `GET /health` 依賴答案的 seam:`ping_redis()`(Redis PING 得不得通)與 `kafka_connected()`(開機時 Kafka producer 是否建成——靜態值,執行中不重新探測)。
port、正式 adapter `RedisHealthProbe` 與 handler `health::health_check` 同檔;500ms 逾時、回應 body(`{"status":"healthy"|"degraded","services":{"database","redis","kafka"}}`)、200/503 全歸 handler,adapter 不做決定。
DB 不是 seam:handler 直接對 `AppState::db` 跑 `SELECT 1`,所以「DB 故障 → 503」沒有測試。
測試:HTTP harness 預設接 `StaticHealthProbe::up()`(`tests/common/mocks.rs`),`spawn_test_app_with_health` 可換 `redis_down()` 或自訂 `{ redis_up, kafka_connected }`;`tests/http_health.rs` 釘三種 body,並以 `common::test_redis()` 直打 `RedisHealthProbe`——HTTP 測試因此不需要 Redis。
_Avoid_: 讓 adapter 自己設逾時或決定 healthy/degraded、在 `AppState` 上放回 Redis 連線或 Kafka producer 只為了給 `/health` 讀。

**AppState 組裝(AppState Assembly)**:
`AppState::new(db, config, infra: Infra, o: Overrides) -> anyhow::Result<AppState>` 是 `AppState` 的唯一建構點,`main.rs` 與 HTTP 測試 harness 共用。
兩個參數的分界是「建構時要不要連線」:`Infra { access_cache, ephemeral, health }` 是需要連線的 seam,**沒有預設**——正式環境用 `Infra::redis(conn, kafka_connected)`(三者共用一條 `ConnectionManager`),harness 自己填 in-memory adapter 與 `StaticHealthProbe`;新增 Redis-backed seam 時,編譯器逼每個呼叫端選 adapter。
`Overrides { email, clock, google_identity }` 是建構時不做 IO 的 seam,`#[derive(Default)]`,`None` = 正式 adapter(`EmailClient::new`,錯誤包成 `failed to build email client — check APP__EMAIL__* settings`;`SystemClock`;`GoogleOAuthClient`)。
`new` 另外自己建:共用 reqwest client(整體 5s／連線 3s／idle 30s)、`SmsClient`(一律真實,測試靠 `SmsConfig::twilio_base_url` 導向 wiremock)、`TaskTracker::new()`。
tracker 規則只寫在 `AppState::background_tasks` 欄位 doc 一處:要 drain 的人(`main` 的 teardown、harness 的 `drain_background`)在 `startup::build_router` 前取 `state.background_tasks.clone()`,不另開 tracker(另開會等一個空 tracker——假綠);常駐迴圈(refresh-token 清理、Kafka consumer/outbox)不得掛上去(`wait()` 永不返回)。
Kafka producer 不進 `AppState`:`main` 的區域變數,只給 outbox dispatcher 與 `Infra::redis` 的旗標。
_Avoid_: 在 `new` 之外手寫 `AppState { .. }` literal、為測試再開第二個 `TaskTracker`、把需要連線的 seam 放進 `Overrides`(那會讓它有「預設正式 adapter」而在測試裡偷偷連 Redis)。

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
`auth::provisioning::create_account` 是「`INSERT users` + 指派 `member` 角色 + 排入 `user_registered` outbox 事件」三步驟的單一 owner。
四個帳號誕生呼叫端中兩個換線於此:`auth::service::register`、`users::service::create_user`(`POST /users`,admin 代建)——後者是新裁決:admin 代建帳號現在與自助註冊共用同一 owner,因此也一併排入 `user_registered` 事件(此前無此行為),但不簽發 session、不發歡迎通知,因為帳號不是使用者本人的註冊動作。
另兩個誕生點刻意不納入這個 owner,理由記錄在該模組文件而非留給呼叫端猜:`auth::repository::create_or_update_google_user_tx`(google OAuth 的 Create 分支)靠 `ON CONFLICT (google_id) DO UPDATE` 擋首次登入併發,換成裸 INSERT 會讓這個 race 從安全 upsert 退化成唯一鍵違例 500;`bin/seed/dataset/accounts.rs` 的 `upsert_account` 是冪等開發種子,若也走 `create_account` 會讓每次重跑 seed 都各自生一筆 `user_registered` 事件,這個副作用不該存在。
seed 端的帳號誕生是 `bin/seed/dataset/accounts.rs`:`upsert_account(db, &SeedAccount { email, name, password_hash, birth_date: Option<NaiveDate>, points, joined_at }, now) -> Uuid` 是 seed 唯一一份 `INSERT INTO users`(admin、測試會員、4 位教練、24 位報表會員共用),冪等於 `email`;新帳號的 `created_at` 是歷史加入時間 `joined_at`、不是執行時刻,開戶給點的規則見「給點」;角色不歸它,由 `run` 的 `assign_role` 逐一指派。介面測試在同一檔案,只建帳號、不跑整份資料集:`new_account_is_born_at_joined_at`、`opening_points_write_one_admin_adjust`、`zero_points_write_no_ledger`、`rerun_after_the_app_moved_the_balance_grants_nothing`。
`member` 角色只在誕生時授予一次:google_auth 依 `linking::LinkPlan::grant_member` 決定,只有 Create 為真——Link(把 Google 綁上既有帳號)與 Refresh(既有 Google 使用者再次登入)都不碰角色,因此 admin 移除的 `member` 不會在下次 Google 登入復活,Link 到 seed 的 admin 也不會被順手加上 `member`(此規則生效前已被誤授的 `member` 不回收)。
`create_account` 直接回 `User`:`member` 授予的 witness 以 `assume_uncached` 消化——列在同一 tx 內剛建立,不可能有快取,故 `register`/`users::create_user` 不 flush、也不收 Redis 參數;google Create 分支則保留 flush(見「帳號存取」)。
測試 fixture 也走同一條誕生路徑:`common::seed_member` 直接呼叫 `create_account`(email 因此一律小寫正規化、帶 `user_registered` 事件),`TestApp::seed_user_with_roles(email, extra: &[Role])` 在其上逐一 `assign_role`——所有 fixture 使用者都以 `member` 出生、與 production 一致,「只有 coach / 只有 admin」不是可建構的狀態;`seed_coach` 呼叫 `coaches::service::create_coach`(`TestApp::seed_coach_for` 傳 `app.access_cache`),coach 角色只授一次。
_Avoid_: 把 google_auth 的 Create 分支或 seed 的 upsert 也算進 `create_account` 的呼叫端(兩者刻意不歸戶,理由見上,不是遺漏)、在登入路徑上「冪等地」重授 `member`(冪等不等於無害——它會覆蓋 admin 的移除決定)。

**Google 身分(Google Identity)**:
`utils::google_oauth::GoogleIdentity`(`sub`/`email`/`name`/`picture`)是「這個 Google 授權碼屬於哪個已驗證的 Google 帳號」的答案,只在全部驗證通過後才建構——授權碼換權杖、id_token RS256 簽章對 Google JWKS、`iss`/`aud`/`exp`、`email_verified`。
`GoogleIdentityProvider::verify_code(code)` 是 port,與正式 adapter `GoogleOAuthClient` 同檔(比照 `utils::email`):code exchange(含其錯誤字串:送出失敗/回應解析失敗 500、非 2xx 一律 400「Google authentication failed」)、JWKS 快取(一小時 TTL、kid miss 強制重抓一次)、`verify_google_id_token` 與 claims 全部私有,呼叫端看不到 id_token。
`AppState.google_identity: Arc<dyn GoogleIdentityProvider>`(`AppState::new` 在 `Overrides.google_identity` 為 `None` 時以 `GoogleOAuthClient::new(&config.auth, http_client)` 建構,JWKS 快取由 adapter 自持,每個 app 實例各一份)。
`auth::service::google_auth` 只收 `&dyn GoogleIdentityProvider`,拿到 `GoogleIdentity` 之後才開 tx——之後的 email 正規化、`name` 缺省回填 email、連結決策(`auth::linking`)、`member` 授予、session 簽發都是登入規則,不屬本詞條。
測試分三層:`tests/google_identity.rs` 以 `wiremock` 直打 adapter(`common::google` 簽 id_token、掛 token/JWKS 端點;token/JWKS URL 仍是 `AuthConfig` 的 config seam);`tests/service_auth.rs` 以 `FakeGoogleIdentity`(`tests/common/mocks.rs`)跑登入規則;`tests/http_auth.rs` 只留一個 `google_auth_route_end_to_end` 釘 route 接線。
_Avoid_: 在 adapter 之外解析或驗證 id_token、讓 service 讀 claims(`email_verified`/`aud` 等檢查是 adapter 的責任,`GoogleIdentity` 存在即代表已通過)、把登入規則測試寫回 HTTP 層(規則層用 fake,adapter 邊界用 wiremock)。

**studio 時間軸 SQL 攣生(Studio Timeline SQL Twin)**:
報表查詢算工作室「今天」與「本月」用的兩個 SQL function,是工作室時鐘同一條規則在 SQL 端的副本,與 Rust 版並存、由交叉測試釘成相等。規則寫在 `utils::studio_clock`。
_Avoid_: 以為 SQL function 取代了 Rust 版(兩份並存,靠測試互相釘住)

**報表組裝(Report Assembly)**:
admin 報表從查詢結果塑形成回應的純核:比率、排序、篩選與拆分都在這裡,不碰 IO、不讀時鐘。owner 是 `reports::assembly`。
_Avoid_: 把衍生自時鐘的值塞進查詢結果的輸入、把報表區段的三處宣告壓成一處(刻意維持)

**權益授予(Entitlement Grant)**:
買會籍或票券時,在寫入當下算出買家得到什麼:堂數額度、到期日、兩者皆有,或無期無額;之後算不算有效另由讀時推導(ADR-0003)。owner 是 `subscriptions::entitlement`。
_Avoid_: 開通、訂閱狀態(那是讀時推導,不是授予)

**唯一鍵衝突映射(Unique-Conflict Mapping)**:
`AppError::conflict_on_unique`/`conflict_on_constraint`/`conflict_on_exclusion`(`src/error/mod.rs`)是「DB 唯一/排他約束違例 → 409」翻譯的單一 owner——判斷真相是 constraint 本身(SQLSTATE `23505`/`23P01`,`conflict_on_constraint` 額外比對違反的具名約束),不是應用層預檢:先 `SELECT` 查是否已存在、查無再 `INSERT` 的預檢寫法已禁用(Phase 7 的 slug 衝突改吃 DB constraint,刪 SELECT 預檢——`courses`/`products`/`posts`/`venues` 四表皆同),因為預檢與寫入之間永遠有 TOCTOU 窗口,constraint 違例才是唯一不可能漏判的判準。十餘個模組共用同一組函式(email、coupon code、slug、教練班表時段重疊……),各自只傳自己的錯誤文案字串,映射邏輯不重複。
五個帶 `slug` 欄位的表分成兩種唯一性口徑,是這個 owner 底下的一個具體案例:`venues`/`products`/`posts`/`courses` 四表以 `LOWER(slug)` 功能性 unique index(`uq_<table>_slug_lower`)實作,與各自 `find_by_slug` 查詢的 `WHERE LOWER(slug) = LOWER($1)` 對稱——大小寫不敏感貫穿唯一性檢查與查找兩側,四表的 create/update 皆由 `conflict_on_unique`/`conflict_on_constraint` 接在 repository 呼叫後轉譯 409。`coaches` 是唯一例外:`coaches_slug_key` 是欄位級 `UNIQUE`(大小寫敏感),且 coaches 從未對外提供 slug-based 查找端點(明細端點走 UUID)——`slug` 目前只是展示用欄位,尚未真正需要案例不敏感語意。此不對稱是已知、記錄在案的落差,非 slug 衝突慣例統一(Phase 7)的修復範圍。
_Avoid_: 假設 coaches 的 slug 衝突判斷與其他四表同樣大小寫不敏感(目前不是)、在任何新唯一性檢查前補一道 SELECT 預檢(繞過這個 owner 等於重新打開 TOCTOU 窗口)。

**跨模組讀表(Cross-Module Table Reads)**:
模組直接 `SELECT`/`JOIN` 另一模組的表,不必先繞經對方模組的 repository/service,是本庫行之有年的常態——witness 與 owner 協定治理的是「寫」,不是「讀」:讀開放、寫歸戶。
「週課表(Weekly Schedule)」詞條的 `sessions::calendar`/`sessions::repository` 直讀 `course_schedule_slots`/`courses` 已先點名此例——`sessions::calendar` 是兩張場次表的寫入 owner,但只擁有寫入與 witness,讀取端(`find_sessions_in`、`reports::venue_usage` 等)照本詞條留在各自模組;現碼另有四處可查:`attendance::records` 的私有讀取 `find_approved_leave_enrolment_ids_tx` 直讀 `leave_requests`(leave 的表;records 是出勤寫入 owner,這條讀取是 `mark_tx` 的私有步驟,不對外);`courses::seats::require_room_tx` 的座位公式同樣直讀 `leave_requests`;`cart::repository::find_cart_items_for_checkout_tx`(orders 結帳流程呼叫)直讀 `products`/`courses` 取價格與名稱,本身不再鎖這兩張表——那些列已由「訂單鎖協定」在各自 owner 鎖好(`products::service::lock_products_tx`/`courses::seats::lock_courses_tx`),真正扣庫存/佔座也轉交各自 owner `products::service::reserve_stock_tx`/`enrolments::service::enrol_batch_from_purchase_tx`(經 `courses::seats::course_seats_tx`);`reports::repository::income_by_source` 一條查詢直讀 `orders`/`order_items`/`products`/`bookings`/`time_slots` 五表聚合月營收。
與「給點」的 `apply_delta_tx`、「場租佔位」的 `bookings::occupancy`、「座位」的 `lock_`/`FOR UPDATE` 協定等既有詞條對照:那些詞條收斂的是「誰能動這張表」,本詞條收斂的是「誰都能看這張表」,分界只在寫、不在讀。
讀開放不代表該拿另一模組的**現況**去重建**歷史事實**:場次在哪個場地上課,以前是讀取時回頭 join `course_schedule_slots` 現查,slot 一改就連過去場次一起變;現改為物化時快照進 `course_sessions.venue`,`sessions::find_today_sessions_in`/`reports::venue_usage` 讀快照、不再跨表 join(見「場次物化」)——與 `bookings.price_cents` 從 `time_slots` 快照同一取捨。
_Avoid_: 為了跨模組讀而新增一層轉手 repository/wrapper——ADR-0005 已裁定 CRUD 轉手 service 層維持現狀不收攏,讀端再包一層只是加寬介面、沒加深功能,換來一顆更淺的模組,不是解法。

**執行環境(AppEnv)**:
`config::AppEnv` 是 `APP_ENV` 讀取與比對的單一 owner——`AppConfig::load()`、`main.rs` 的 production 守衛與 log 格式判斷、`bin/seed/main.rs` 的 production 拒跑檢查,原本四處各自讀取、比對語意不一(混雜大小寫敏感的 `==`/`!=`),現在共用同一個 newtype。
`is_development()`/`is_production()` 兩謂詞刻意獨立、不可合併:`AppConfig::load()` 自己的檢查是「非 dev 都跑」(fail-closed,`!is_development()`),`validate_production_config`(原 main.rs 守衛,現搬進本模組、可單元測試)是「僅 production 跑」(`is_production()`),語意不同。
兩謂詞皆 `eq_ignore_ascii_case`(大小寫不敏感);`config/{env}.toml` overlay 檔名則用 `raw()` 保留原始字串(大小寫照舊)。
因為「非 dev」嚴格涵蓋「production」,`main()` 啟動路徑上 `AppConfig::load()` 先跑、`?` 短路,`validate_production_config` 的 placeholder secret 分支在該路徑下實際不可達(production 淨效果不變——兩者都拒啟動,只是拒絕文案來源不同);此分支保留是刻意的 defense-in-depth,`validate_production_config` 必須對任意呼叫者(不只 `main()`,含未經 `load()` 的手工建構 `AppConfig`)自足有效。
_Avoid_: 與 `config` crate 自帶的 `config::Environment`(`env_source()` 用的 config 來源,讀 `APP__*` 環境變數到設定樹)混同——兩者名字容易撞,語意完全無關,新型別故意不叫 `Environment`。

**事件派送(Outbox Dispatch)**:
`kafka::outbox::drain_once(db, publisher)` 是把 `events_outbox` 送上 Kafka 的單一入口(`start_dispatcher` 每 500ms 呼叫一次;介面不變,測試直接驅動它配 `EventPublisher` fake),自己管時間預算,分三步、任何一步都不跨 Kafka round-trip 持有交易:(1)**認領**——一條 autocommit `UPDATE events_outbox SET next_attempt_at = NOW() + LEASE WHERE id IN (SELECT … WHERE published_at IS NULL AND next_attempt_at <= NOW() ORDER BY next_attempt_at LIMIT 100 FOR UPDATE SKIP LOCKED) RETURNING …`,`LEASE` = 60s;租約期間併發的 drain(水平擴充或下一個 tick)都看不到這列,Rust 端再依 `created_at` 排序;(2)**發送**——交易外,私有 `publish_batch` 以 `tokio::time::timeout_at` 套 `PUBLISH_BUDGET` = 15s(`pub(crate)`,producer 的 `message.timeout.ms` 取同一個值);逾時的那列算失敗並停止這批,後面沒輪到的列不發也不記帳,留著租約等過期後重領;(3)**記帳**——一個短交易:成功 → `published_at = NOW()`;失敗 → `attempts + 1`、`last_error`、`next_attempt_at = NOW() + retry_delay(n)`,`retry_delay(n) = min(2^n 秒, 1 小時)`。
掃描用 partial index `idx_events_outbox_due ON (next_attempt_at) WHERE published_at IS NULL`。
毒訊息政策:**沒有終止狀態**,一直重試、退避上限 1 小時、每次失敗 `tracing::error!`;不清理已發送列。
沒有 per-key 順序保證(失敗列退避時,同 `kafka_key` 的後續列照發)。
與消費端原地有限重試的不對稱見 ADR-0013。
_Avoid_: 在交易裡 await publish(publish 一慢超過 `idle_in_transaction_session_timeout`,Postgres 砍連線,記帳落不了地——舊 `drain_once` 的形狀);失敗列不設 `next_attempt_at` 就讓下一個 tick 立刻重發(熱迴圈);替 outbox 加「放棄/死信」狀態(outbox 是事件唯一的來源,放棄即遺失)。

**消費迴圈(Consumer Loop)**:
`kafka::consumer::run`(private)是 audit consumer 的主迴圈,只經兩個模組私有 port 與外界互動:`MessageSource`(`next()` 依序給出 owned `SourceMessage { topic, partition, offset, payload }`,`None` = source 結束;`commit(&msg)`)與 `AuditHandler`(`handle(payload)`)。
正式 adapter 是 `KafkaSource(StreamConsumer)`(`recv()`,stream 錯誤 log 後跳過;commit 用 `TopicPartitionList::add_partition_offset(.., Offset::Offset(offset + 1))`——Kafka 的 committed offset 指「下一筆要讀的」,少了 `+1` 重啟後會重送這筆)與 `PgPool`(轉給 `handle_audit_event`);測試用 `ScriptedSource`/`ScriptedHandler` 在 `#[tokio::test(start_paused = true)]` 下記 `H<offset>`/`C<offset>` 事件序列,不碰 DB 也不真睡。
每則訊息的命運由 `resolve()` 單一決定,回 `Resolution { Commit, Shutdown }`:空 payload、非 UTF-8、Ok、Poison → `Commit`;`Transient` → 對**同一則**原地重試,退避 1/2/4/8 s,呼叫滿 `MAX_ATTEMPTS = 5` 次仍失敗 → error log + `Commit`;退避期間收到 shutdown → `Shutdown`,不 commit 直接結束(重啟後重送)。
訊息一次只解決一則,所以絕不會有後續 offset 越過尚未解決的訊息被 commit;代價是單一迴圈服務所有 partition,原地重試期間全部暫停,且 Postgres 中斷超過約 15 s 時中斷期間消費到的每則都會被丟棄(需從 Kafka 或 `events_outbox` 重放)。
shutdown 只在兩處檢查——讀下一則之前(`biased`,優先於新訊息)與退避 sleep 期間;handler 呼叫本身不與 shutdown 競速,跑到一半的寫入會完成。
`start_consumer` 只負責 subscribe 後組裝 `run(KafkaSource(consumer), db, shutdown_rx)`,公開簽章不變。
資源對映 `domain_resource` 是 `spec_for_event_type(event_type).map(...)` 的單純包裝,未建模 event_type(含同 prefix 子型別如未來的 `order_refunded`)一律落 `data.resource` 讀取路徑(預設 `"audit"`)。
_Avoid_: 把 Transient 當成「先不 commit、讀下一則,之後會重送」——同一 partition 的後續訊息一 commit,offset 就越過了失敗那則,等於默默丟掉(舊迴圈的 `[H0,H1,C1]`);重試必須原地做在同一則上。也別讓 handler 呼叫與 shutdown `select!` 競速,或在退避中收到 shutdown 時 commit。

**Wire enum 值域(Wire Enum Domain)**:
9 個 Postgres enum(attendance_status、cart_item_type、inquiry_status、course_level、leave_status、order_status、post_category、post_status、product_type)加上 `InquiryType`(`contact_inquiries.inquiry_type` 是 VARCHAR,值域只在 Rust `ALL`,不在 `wire_enums.rs` 錨定範圍)各自的 Rust 對映型別,值域單一 owner 是 `pub const ALL: [Self; N]`(順序與既有錯誤文字列舉順序一致);`FromStr` 與允許值文字皆由此推導,不再各自手抄一份標籤清單。
大小寫政策分兩類,逐型別固定、不可自行假設:`CourseLevel`、`PostCategory`、`PostStatus`、`InquiryStatus` 四者大小寫不敏感(`FromStr` 內先 `to_lowercase()` 再比對 `ALL`,註記「既有 wire 政策」);其餘(`ProductType`、`LeaveStatus`、`CartItemType`、`OrderStatus`、`AttendanceStatus`、`InquiryType`)大小寫敏感,不符即 422。
解析時機單一歸戶:service 邊界呼叫一次 `s.parse::<X>()`,repository 一律收已解析好的 enum 值、直接綁定(`.bind(value)`,不綁字串、SQL 不再 `$N::enum_name` 轉型),不做「解析後又轉字串」的往返。
允許值 422 文字在 service.rs 由 `X::ALL.map(|v| v.as_str()).join(", ")` 組出(`contact` 沿用既有訊息風格改用 `"/"` 分隔),courses/posts/contact 三處各自的多個呼叫點共用同一個私有 helper 函式,避免同一份允許值清單在同檔內重複手抄。
跨語言值域一致性由 `tests/wire_enums.rs` 錨定:比對 `X::ALL` 的 `as_str()` 序列與 `SELECT unnest(enum_range(NULL::x))::text` 的序列「集合且順序」相等,同 `revenue_predicate_matches_revenue_statuses_array` 手法。
_Avoid_: 把 `leave::rules::parse_decision` 誤認為屬於本詞條的一員——那是決策動詞(approve/reject)的獨立子集解析器,值域與本詞條的 10 個 wire enum 無關,不歸戶於此;把 `OrderStatus::REVENUE_STATUSES`、`bookings::model::VENUE_REVENUE_STATUSES` 等 SQL 綁定用途的字串陣列常數誤認為本詞條管轄的值域(那些是各自謂詞 owner 的 SQL 綁定攣生面,見「營收狀態集」「場租」兩詞條,不是 wire 解析的允許值清單)。
