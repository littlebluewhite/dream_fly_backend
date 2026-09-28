# ADR-0012: 教練範圍含已下架課程

## Context

「教練名下的課程/學員」這個範圍在後端有五處讀取端(`GET /coaches/me/students`、`GET /reports/coach` 的
`student_count`/`today_sessions`/`pending_attendance`、`GET /reports/admin` 的 `coaches[].course_count`/
`student_count`、`GET /leave-requests` 的教練身分),但只有 `attendance::repository` 的兩處(`find_my_students`、
`count_my_students`)在謂詞裡多寫了一段 `AND c.is_active = true`;其餘五處(`sessions::repository::
find_course_ids_by_coach`、`reports::repository::coach_today_and_pending`/`coach_reports`、
`leave::repository::find_admin_list`/`count_admin_list` 的 `coach_scope` 分支)自始就只用 `c.coach_id = $1`,
從未篩過 `is_active`。這造成兩種口徑在同一個教練身上會給出不同的數字:一堂課被下架(停售)的當下,教練儀表
板的 `student_count` 掉了,但 admin 報表的 `coaches[].student_count`、名下的待審假單清單卻沒變——兩端顯示
的「這位教練有幾個學生」對不上。

`sessions::service.rs:44` 的既有裁決是「下架＝停售不是停課」:被下架的課程仍持續上課、仍需要教練點名、學
員仍在其中——下架只是關掉新報名的入口,不是把課程從教練名下抹除。`attendance::repository` 這兩處篩選違反
的正是這個裁決,是目前唯一的漂移點,不是設計如此。

`count_my_students` 的 doc comment 本身已自承是 `find_my_students` 的「COUNT 攣生」,兩者謂詞用同一份英文
描述維護一致——這條深化要處理的是砍掉這對攣生,不是新增一個 view 或 SQL 片段去對齊它們。

## Decision

**教練範圍 = `courses.coach_id` 指向該教練的全部課程,不論 `is_active`;教練範圍下的學員 = 這些課程的
`active_enrolments` 去重後的 distinct 使用者集合。**所有教練面(coach 自己看到的 `/coaches/me/students`、
`/reports/coach`;admin 看到的 `/reports/admin` 的 `coaches[]`;教練身分呼叫 `/leave-requests` 看到的名單)
一律採同一口徑——不是"預設含、個別端點特例排除",而是沒有任何一處篩 `is_active`。

- **實作**:刪除 `attendance::repository::find_my_students`/`count_my_students` 兩處的
  `AND c.is_active = true`,讓這兩處與另外五處本就一致的謂詞(`c.coach_id = $1`)對齊,而不是反過來替另外
  五處加上過濾。
- **`certificates` 關係 gate 另計**:`certificates::service::create_certificate` 用的是「教過此生」關係 gate
  (契約 §3.22,active 或 cancelled 皆算),與本 ADR 的「教練名下課程範圍」是兩個不同的所有權形狀(見 CONTEXT
  「課程教練所有權」詞條),不受本裁決影響。
- **公開上架可見性不受影響**:`products`/`courses`/`venues`/`coaches` 的公開列表/明細端點(CONTEXT「上架可見
  性」詞條)仍然只回 `is_active = true` 的課程——本 ADR 只改教練自己(以及 admin 代管視角)看到的範圍,不改
  訪客/會員瀏覽端看到的範圍。這是刻意的兩層語意:「這堂課還能不能被看到、被買」與「這堂課還算不算這位教
  練名下的課」,本來就是兩個獨立問題,下架只回答前者。

## 落選方案

- **在另外五處全面加上 `is_active` 過濾**:方向相反,會讓下架課程的教練瞬間看不到自己還在教、還有學員在裡
  面的課程,點名跟批假都做不了——與「下架＝停售不是停課」正面衝突。
- **新增一個 view 或共用 SQL 片段去對齊六個站點**:六個站點的謂詞形狀本就不同(有的直接 `courses.coach_id`,
  有的透過 JOIN,有的是 `find_admin_list` 的 `Option<Uuid>` scope 參數),抽一個 view 或片段換來的一致性只
  是表面的——真正需要對齊的謂詞就是 `c.coach_id = $1` 這一個條件,不值得為它蓋一層間接。
- **保留 `count_my_students`、只改它的 SQL**:不能解決「兩支函式各自維護一份謂詞,將來又漂移」的根因;深化
  方向本就是砍掉這對攣生(`count_my_students` 的 doc comment 自承的技術債),不是把債務原地展延。

## Consequences

- 若要讓某位教練不再看到某堂課的學員/場次(例如整堂課要移交給別的教練),正確做法是取消該課程學員的報名或
  把 `courses.coach_id` 改派給別的教練——**不是**下架課程;下架只影響上架可見性,不影響教練範圍。
- `attendance::repository::count_my_students` 已刪除,`reports::service::coach_report` 的 `student_count`
  改由 `find_my_students(db, coach.id).await?.len()` 推導(名冊以 `u.id` GROUP BY,列數即 COUNT DISTINCT,見
  下方 4b commit),行為零變更、少維護一份 SQL。
- 六個讀取端的一致性由跨面交叉測試錨定(`tests/service_reports.rs::
  coach_scope_includes_delisted_courses_on_every_surface`),不是靠共用 view/片段——任何一處日後不小心加回
  `is_active` 過濾,這條測試會立刻紅。
