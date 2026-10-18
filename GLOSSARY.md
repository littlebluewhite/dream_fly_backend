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
工作室牆鐘的時間語意:場次與時段的日期、時間都是工作室時區的牆鐘值,「今天」是工作室當地的日曆日(契約 §3.18 裁決 2)。取樣一次的時刻配上時區是 `StudioNow`,owner 是 `utils::studio_clock`。
_Avoid_: 伺服器時間、拿 UTC 日期當「今天」、沒說是誰的「本地時間」

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
報表查詢算工作室「今天」與「本月」用的兩個 SQL function,是工作室時鐘同一條規則在 SQL 端的副本,與 Rust 版並存、由交叉測試釘成相等。規則寫在 `utils::studio_clock`。
_Avoid_: 以為 SQL function 取代了 Rust 版(兩份並存,靠測試互相釘住)

**報表組裝(Report Assembly)**:
admin 報表從查詢結果塑形成回應的純核:比率、排序、篩選與拆分都在這裡,不碰 IO、不讀時鐘。owner 是 `reports::assembly`。
_Avoid_: 把衍生自時鐘的值塞進查詢結果的輸入、把報表區段的三處宣告壓成一處(刻意維持)

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
