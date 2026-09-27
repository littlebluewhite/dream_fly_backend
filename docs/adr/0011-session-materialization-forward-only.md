# ADR-0011: 場次物化只往前

## Context

場次(`course_sessions`)是讀取端按需把週課表(`course_schedule_slots`)展開出來的:讀取前先呼叫
`materialize_range`,為範圍內每個「星期對得上 slot」的日期補一列。展開依據永遠是**當下**的週課表。

這在未來日期上沒有問題——課表改了,`update_course` 會在同一 tx 內對齊未來場次。但對過去日期,當下的週課表
不代表那天實際上了什麼課:

- `reports::service::admin_report` 為了 `venue_usage` 會物化**整個本月**,本月已過去的日子也在內。月中改開
  課時間後,本月過去日期會多出新時間的「幻影場次」——那天實際沒上。改前 `venue_usage` 經 slot join 只算到
  幻影、真場次反而對不回 slot 被藏掉;場地快照上線後兩者都算,仍是錯的。
- `GET /courses/{id}/sessions` 帶過去的 `from` 時,同樣會用今天的課表回填過去日期。前端目前從不送 `from`,
  但端點契約允許。

CONTEXT「場次物化」詞條把這條列為已知缺口,根治方向記為「物化不回填過去日期」。

## Decision

**場次物化只往前**:`sessions::calendar::materialize_range(db, today, course_ids, from, to)` 只插入
`[max(from, today), to]`(`today` 為 studio 當地日期),區間為空就早退。

- **過去只讀既有列**。witness(`MaterializedRange`)仍帶**請求的** `[from, to]`,讀取端不變;過去日期讀到
  的是當時已被物化過的列。admin 月報照傳整月,由 calendar 夾取。
- **單日版改名** `materialize_today(db, course_ids, today)`,是 `MaterializedDay` 的唯一建構點。
- **seed 走具名回填** `calendar::backfill_for_seed(db, course_ids, from, to)`,不回 witness。它以當前週課表
  回填過去,正是 runtime 禁止的幻影來源;只對「課表從未變過」的全新開發資料集成立,runtime 不可呼叫。
- **既有幻影列不清**。DB 裡已被舊行為造出的幻影場次,和真正上過課的場次在資料上無從分辨。
- 422(`to < from`)與 60 天跨距上限不變。

## 落選方案

- **過去的 `from` 回 422**:只擋得住 `GET /courses/{id}/sessions`,擋不住 admin 月報本身要讀本月過去日期;
  而且讀既有的過去場次是正當需求(例如查已點過名的場次),不該一律拒絕。
- **slot 歷史表**(記錄每個 slot 的生效區間,過去日期依當時課表物化):能讓過去也「正確回填」,但要新表、
  migration、改寫 `update_course` 與 reconcile,為一個報表欄位付出不成比例的複雜度;而且過去沒被物化的日
  子本來就不可能點名,回填了也沒有出勤資料可掛。
- **清理幻影列**:無從分辨哪些列是幻影(見 Decision 最後一點),清錯會連帶刪掉真實場次(點名紀錄
  `ON DELETE CASCADE`)。

## Consequences

- `venue_usage` 的本月口徑變成「本月已存在的過去場次 + 今天起到月底的場次」,不再造幻影。
- `GET /courses/{id}/sessions` 帶過去的 `from`:只回已存在的列,不新建。
- **代價(未被讀過的過去日子)**:某天變成過去之前,若從未被任何讀取端(課程場次列表、今日場次、教練/會員
  報表、admin 月報)物化過,之後就不計入 `venue_usage`。這種日子本來就不可能點名——點名要先有場次列。
- **代價(當天改課表)**:reconcile 只對齊 `session_date > today` 的場次;當天改開課時間後,今天已物化的舊
  場次保留,新時間的場次也會被物化,兩場並存。這是既有邊界,本 ADR 不改。
- 寫入集中在 `sessions::calendar`(`course_sessions` 與 `course_schedule_slots` 的唯一 runtime 寫入者);
  seed 直寫 slot 與出勤仍 bypass(ADR-0010)。
