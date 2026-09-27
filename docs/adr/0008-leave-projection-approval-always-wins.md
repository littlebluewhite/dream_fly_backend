# ADR-0008: 請假投影——核准恆勝、點名不可覆寫已核准請假(乙案:雙層 guard)

## Context

`attendance_records` 有兩個寫入者。核准請假(`leave::service::decide_leave_request`)在
一個交易內雙寫:更新假單為 `approved`,並 `upsert_attendance_tx` 寫入該場次的 `leave` 出勤列
(`marked_by` = 核准者)。批次點名(`attendance::service::bulk_upsert_attendance`,
`PUT /sessions/{id}/attendance`)走 `require_started` 時間閘 → `marking::parse`/`plan` →
逐列 `upsert_attendance_tx`。在本卡之前,`upsert_attendance_tx` 的
`ON CONFLICT DO UPDATE` **無任何守衛**,兩個方向都能任意覆寫對方寫下的列。

這裡有一段空白:兩個寫入者相撞時該讓誰贏,從未被裁決。具體而言,核准請假寫下 `leave` 列之後,
一次批次點名可以把它靜默覆寫回 `present`/`absent`——「請假被核准了卻在出勤上顯示出席」是個沒
有人決定過對錯的狀態。反方向也一樣沒有測試:先點 `present` 再核准、已核准後被批次覆寫,正反兩
向皆無見證。

三個讀取 `leave` 出勤列的地方——名冊(`attendance::repository::find_roster`)、
`coach_today_and_pending` 的場次層 `NOT EXISTS`、會員出勤時間軸——以及只讀 `leave_requests`
的座位公式(`courses::seats`,契約 §3.20),在本卡的任一方案下都不需要改動;這段空白純粹是
「兩個寫入者的相撞規則」與其防護。

## Decision

### 1. 投影方案 = 乙案(點名 guard),保留 decide 同一交易雙寫

`decide_leave_request` 的同 tx 雙寫**原封保留**。「點名不可覆寫已核准請假」這條規則收進
`attendance::marking::plan` 的純核(新增第三輸入——該場次「已核准請假的 enrolment 集合」)加上
`upsert_attendance_tx` 的寫入點守衛,而不是移除 decide 的出勤寫入、改由讀取端推導(甲案,見決策
6)。

### 2. 核准恆勝、不加時間閘;`decide` 簽章不動

晚核准是營運常態(教練隔天才批假是正常的),因此:

- **核准覆寫 `present`/`absent` 是合法裁決**,不是「輸掉的競態」。`decide` 不加任何時間閘,簽章
  不動——核准一律寫入 `leave`,不論該場次是否已開始、也不論該生此刻出勤被點成什麼。
- 守衛只擋**反方向**:批次點名覆寫一筆已核准請假的 `leave` 列。
- 見證:`http_leave.rs::decide_approve_overwrites_existing_present_attendance`(已點
  `present` → 核准 → 覆寫成 `leave`,即使場次已於昨日開始)。

### 3. 雙層防護:純核 pre-check(整批 422)+ 寫入點 guard(關閉 TOCTOU)

**第一層——`marking::plan` 純核 pre-check**:`plan` 收第三輸入(本批成員中持有 `approved` 假單
者的集合,由 `repository::find_approved_leave_enrolment_ids_tx` 查得)。批內任一成員被點
`present`/`absent` 且落在此集合 → **整批 422、零寫入**(沿用既有的「成員資格不符即整批拒絕」
全有全無語意)。點 `leave` 給集合內成員 → 通過(冪等重寫)。此查詢放在**寫入 tx 內**、`plan`
呼叫隨之入 tx:`plan` 是純函式,`Err` 經 `?` 在任何 upsert 之前短路,tx 回滾即零寫入,「整批
422、零寫入」契約不變。錯誤閘門順序維持現狀:session 404 → coach 403 → 未開始 422 → parse 422
→ 成員資格/approved-guard 422(後兩者同在 `plan` 內)。

**為什麼純 pre-check 不夠**:它有競態窗——批次讀完 approved-set 之後、upsert 之前,一筆核准恰
好 commit(decide 已在其 tx 內雙寫 `leave` 列),批次會把它覆寫回 `present`,「核准恆勝」被打
穿。

**第二層——`upsert_attendance_tx` 寫入點 guard**:`ON CONFLICT DO UPDATE` 加
`WHERE EXCLUDED.status = 'leave' OR attendance_records.status <> 'leave' OR NOT EXISTS
(SELECT 1 FROM leave_requests lr WHERE lr.enrolment_id = attendance_records.enrolment_id
AND lr.session_id = attendance_records.session_id AND lr.status = 'approved')`。三個 OR 分支
任一為真即允許寫入:

- `EXCLUDED.status = 'leave'`:寫 `leave` 恆通過——decide 的核准 upsert 永遠贏(決策 2),點名
  重寫 `leave` 冪等通過。見證:決策 2 的測試、`attendance_put_leave_over_approved_leave_is_idempotent`。
- `attendance_records.status <> 'leave'`:現值不是 `leave`,正常 `present`/`absent` 互覆寫不受影
  響。見證:既有 `attendance_put_is_idempotent_and_overwrites_on_second_call`(present↔absent↔leave
  互覆寫,現值非 leave 時走此分支)。
- `NOT EXISTS (approved leave)`:現值是 `leave` 但**無核准單**(口頭請假),仍可被覆寫(決策 4)。
  見證:`service_attendance.rs::upsert_guard_allows_present_over_verbal_leave`。

「有 `approved` 單且現值 `leave`」時三分支皆假 → 不更新、零列受影響但不報錯。**零列是 guard
唯一的產生者**(無衝突的 INSERT 與放行的 DO UPDATE 都回報 1 列),因此 `rows_affected` 就是
guard 的訊號:批次點名的 service 迴圈以 `rows_affected == 0` 識別「pre-check 之後才 commit 的
核准」,轉成與第一層**相同的整批 422** 並回滾——競態窗內外的對外行為一致,「整批 422、零寫入」
在窗內同樣成立;`decide` 寫 `leave` 恆過第一分支,無需檢查。`EXISTS` 子查詢走
`uniq_leave_requests_active` partial index(enrolment_id 前導、`approved` 落在其
`WHERE status IN ('pending','approved')` 謂詞內),無需新 migration。決定性見證:
`service_attendance.rs::upsert_guard_blocks_present_over_approved_leave`(approved+leave 列上
直接以 `present` 呼叫 upsert → 列維持 `leave` 且回報零列;競態的可測代理,不需真並發)。

### 4. 口頭請假(`PUT "leave"` 無核准單)完整保留

不經核准單、直接 `PUT "leave"` 的口頭請假,寫入與覆寫行為**完全不變**:`plan` 的 approved-set
只含有核准單者,口頭請假成員不在內,可直接被點 `leave`、也可被改點 `present`/`absent`;寫入點
guard 的 `NOT EXISTS` 分支同樣放行。見證:`plan_present_for_non_approved_member_is_unaffected`、
`upsert_guard_allows_present_over_verbal_leave`。

### 5. 記錄兩個 known gaps

1. **approved 無撤銷路徑(臨時出席死角)**:`leave_requests` 的 `approved` 狀態沒有任何撤銷轉移
   (無 `approved → cancelled` 邊)。因此「請假獲准卻臨時出席」的學員**無法**被改點 `present`
   ——pre-check 與寫入點 guard 都會擋下,而系統沒有把假單退回的路徑。比照 ADR-0006「候補無聯絡
   欄位」的記錄模式:這是本決策換來的、記錄在案的營運死角,不是 bug。要解需要新增
   `approved → cancelled` 狀態邊(附帶「撤銷核准是否連帶清掉補課」的產品決定),不在本卡範圍。
2. **guard 殘餘極窄並發窗(`EXISTS` 快照落後)**:理論殘餘窗只剩「兩條寫入語句同刻並發於首筆
   列、`EXISTS` 子查詢的快照落在一筆剛 commit 的核准之前」的極窄情境——此時該筆核准的 `leave`
   仍可能被覆寫成 `present`。READ COMMITTED 下每條語句取新快照已把窗壓到極窄,但未完全消除(徹底
   關閉需 SERIALIZABLE 或讓批次路徑對 `leave_requests` 列顯式取鎖,代價高於本階段所需)。復原方
   式:再次點 `leave` 手動蓋回。記錄在案、可人工復原,本卡不消除。

### 6. 落選方案:甲案(derive)

甲案 = `decide` 移除 attendance 寫入,讀取端由 `approved` 假單即時推導 `leave`。落選,兩個具體
連鎖:

- `coach_today_and_pending` 的場次層 `pending_attendance`(任一狀態 EXISTS)口徑會產生**可觀察的
  行為變更**——`leave` 列不再實體存在,該查詢需改寫,對外語意跟著變。
- `PUT "leave"` 口頭請假(決策 4)在 derive 模型下**無處落地**(沒有假單可推導),必須封掉這條路
  ——一條產品連鎖。

若未來重開請假投影設計,以本 ADR 為起點。

## Consequences

- 本卡刻意的 wire 變更**僅一項**:批次點名新增 approved-guard 422(批內含已核准請假成員之
  `present`/`absent` → 整批 422)。名冊形狀、`decide` 行為、口頭請假逐字不變。
- 兩個 known gap 是接受的持續成本:approved 無撤銷 → 臨時出席的學員出勤停在 `leave`(需人工在
  營運層處理);guard 殘餘窗 → 極罕見情況下核准可能被覆寫,以重點 `leave` 復原。任一要收斂都需
  重開本 ADR(前者加狀態邊 + 產品決定,後者升隔離級別或加鎖)。
- 「核准恆勝」與「點名不可覆寫已核准請假」自此是一對明文互補的規則:同一筆 `leave` 列,decide
  寫得進(核准恆勝)、批次點名蓋不掉(guard),方向不對稱是刻意的。

## Addendum(2026-09-27):出勤寫入 owner 收進 `attendance::records`

`attendance_records` 的 runtime 寫入自此只有一個 owner:`attendance::records`(module 公開、函式收
窄,比照 `auth::session`)。本 ADR 各決策的語意不變,只是住址換了:

- **兩個寫入者 → 一個 owner 的兩個入口**:批次點名走 `pub(super) records::mark_tx`(空批次跳過查
  詢 → valid-set 讀 → approved-set 讀 → `marking::plan` → 逐列 upsert;0 列 → 同一 422);核准投
  影走 `pub(crate) records::project_approved_leave_tx`(`decide_leave_request` 同 tx 雙寫原封
  保留,決策 1)。`upsert_attendance_tx` 改名為私有 `upsert_tx`,`ON CONFLICT … WHERE` 三分支守衛
  SQL 逐字搬入;`find_active_enrolment_ids_in`、`find_approved_leave_enrolment_ids_tx` 同樣降為
  records 的私有函式。雙層防護(決策 3)整套住在 `mark_tx` 裡。
- **valid-set 讀取從 pool 移進寫入 tx**:原本成員資格查詢在 `begin` 之前走 pool。READ COMMITTED
  下每條語句本來就各取快照,移進 tx 不改錯誤序(session 404 → coach 403 → 未開始 422 → parse 422
  → 成員資格/approved-guard 422)與狀態碼;唯一差別是 DB 故障時失敗點變成 `begin`,兩者都是 500。
- **錯誤文字單一化**:「cannot overwrite an approved leave with present/absent」收成
  `marking::APPROVED_LEAVE_OVERWRITE`,pre-check 與 0 列轉換共用同一常數。
- **決策 3 的見證換成真競態**:`service_attendance.rs::approval_committed_mid_batch_rolls_back_whole_batch`
  取代原本直呼 repository 的決定性代理 `upsert_guard_blocks_present_over_approved_leave`——另一
  tx 先持有 A 的未 commit 出勤列,讓批次在讀完 approved-set(∅)後卡在 A;期間以真的
  `decide_leave_request` 核准 B 並 commit;放行後斷言整批 422、A 零列、B 維持 `leave`。這是決策 3
  「零列轉整批 422」在真並發下的第一個見證。決策 3/4 的口頭請假見證
  `upsert_guard_allows_present_over_verbal_leave` 同樣改由 service 層的
  `bulk_present_over_verbal_leave_overwrites` 取代(守衛第三分支)。
- seed 的 `insert_attendance_bulk`、fixtures 的 `seed_attendance` 照 ADR-0010 繼續 bypass。

本檔其餘敘述維持決策當下狀態。

## Addendum(2026-09-27):報名取消連帶取消待審假單;核准 409 降為 backstop

**使用者裁決**:報名取消時,同一報名底下的**待審**假單同 tx 轉成 `cancelled`;已核准假單與已約補
課不動。這延續「只擋核准、不擋拒絕」的初衷——假單不會卡在教練的待審清單裡。**決策 5 的 gap 1
不重開**:`approved` 仍無撤銷路徑,本變更只碰 `pending`。

- **owner**:「報名取消」的單一 owner 是 `enrolments::service`。`cancel_enrolment`(自助/admin)在
  `cancel_if_active_tx` 之後、同 tx 呼叫 `leave::service::cancel_pending_for_enrolments_tx`(ADR-0005
  轉手,repository 同名函式:`WHERE enrolment_id = ANY($1) AND status = 'pending'`,謂詞與
  `cancel_if_pending_tx` 相同);`cancel_by_order_tx`(退款補償)改以 `RETURNING id` 只把真的翻轉
  的報名交給 leave,對外仍回 `u64`,orders 不動。錯誤序 404 → 403 → 409 不變;不發通知。
- **讀取端守衛**:`rules::check_decidable` 的「報名已取消,無法核准請假」409 降為**併發 backstop**
  ——取消之後的審核改走 not-pending 409「僅待審核假單可審核」(駁回也是,原本 200)。backstop 仍擋
  兩種列:取消 commit 之後才插入的 pending 列(`create_leave_request` 讀 `active_enrolments` 與
  INSERT 之間的窄窗),以及上線前遺留的資料(不做資料遷移;教練仍可駁回清掉)。
  `check_makeup_source` 的報名 409、座位公式的 active 過濾原樣保留。
- **死鎖論證**:decide 不鎖假單列,真正的環在報名列。核准 tx 先 UPDATE 假單列(持有列鎖),再經
  `records::project_approved_leave_tx` INSERT 出勤——`attendance_records.enrolment_id` 的 FK 對報名
  列取 `FOR KEY SHARE`。取消 tx 以前用 `find_by_id_tx` 的 `FOR UPDATE` 鎖報名列;只要取消路徑也要
  動假單,兩邊就各持一把、各等對方:取消等假單列,核准的 INSERT 等報名列 → "deadlock detected"。
  修法:`find_by_id_tx` 改取 `FOR NO KEY UPDATE`(同 ADR-0007 商品預鎖的手法)。它與 `KEY SHARE`
  相容,核准的 INSERT 直接通過並 commit;取消隨後重新評估假單列,看到已是 `approved` → 不動(合法的
  「先核准再退課」狀態)。兩個取消之間仍互斥(`NO KEY UPDATE` 彼此衝突);`cancel_if_active_tx` 只改
  非 key 欄位,本身也只取 `NO KEY UPDATE` 級。退款路徑(UPDATE 報名 → UPDATE 假單)同理不成環。
  副作用:取消進行中時,參照該報名的 FK INSERT 不再被擋到 commit。
- **見證**:`service_enrolments.rs::cancel_enrolment_vs_in_flight_approval_does_not_deadlock`(T1 以
  raw SQL 核准並持有假單列 → 取消卡住(輪詢 `pg_stat_activity` 確認在等鎖)→ T1 INSERT 出勤、
  commit → 兩邊皆 Ok、報名 cancelled、假單仍 approved;改鎖前單跑得到 40P01 "deadlock detected")、
  `http_enrolments.rs::cancel_enrolment_cancels_only_its_pending_leave_requests`、
  `service_orders.rs::refund_cancels_pending_leaves_of_order_enrolments`,以及 `http_leave.rs` 的
  `decide_{approve,reject}_after_enrolment_cancel_is_409_not_pending` 與兩支 backstop
  `decide_approve_pending_on_cancelled_enrolment_returns_409`、
  `decide_reject_pending_on_cancelled_enrolment_succeeds`。

本檔其餘敘述維持決策當下狀態。
