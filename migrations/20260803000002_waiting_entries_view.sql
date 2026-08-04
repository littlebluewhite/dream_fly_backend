-- =============================================================================
-- Phase 9: 候補「等待中」口徑下沉為 view 單一真相。
--
-- Before this migration, 10 個 READ 站點(courses repository 的 7 個
-- `waitlist_count` 顯示子查詢——含兩支 scoped finder `find_active_by_slug`/
-- `find_active_by_id` 各自的逐字拷貝、waitlist repository 的
-- `exists_waiting`/`find_by_course_waiting`、reports 的 `course_reports`
-- 統計)各自手寫 `status = 'waiting'` 篩選,判斷「這筆候補目前算不算數」
-- ——同一口徑有 10 份獨立 SQL 拷貝,全靠人肉保持一致。
--
-- ADR-0006 定案候補為人工遞補的諮詢名單(advisory list):名額釋出後,遞
-- 補由 admin 依 `GET /waitlist?course_id=` 名單人工聯絡候補者,系統不做
-- 任何自動遞補、不自動通知。`waitlist_status` enum 目前僅 `{waiting,
-- cancelled}` 兩值;本 view 同時是未來若要替「已人工聯絡」補一個第三態
-- (例如 `contacted`,標記 admin 已聯絡但候補者尚未完成結帳或取消)預留
-- 的單一收斂點——今天二元狀態下,篩選結果與「是否落在 view 內」完全等
-- 價,不代表已有實際行為差異。**本 view 純粹是讀取收斂,不帶任何寫入、
-- 觸發或狀態轉移邏輯,不引入 ADR-0006 明文排除的自動遞補。**
--
-- 刻意排除、不下沉進本 view 的站點:
-- (1) 寫側狀態轉移:`insert`(加入候補)的 INSERT 字面值、
--     `cancel_if_waiting_tx`(取消候補)UPDATE 的 `status = 'waiting'` 守
--     衛——寫側不能讀自己正在寫的 view。
-- (2) 無 waiting 謂詞的讀側:`find_by_id_tx`(取消路徑用的 `FOR UPDATE`
--     行鎖,不論狀態都要鎖到,供 404 判斷)、`find_by_user_with_course`
--     (會員 `/waitlist/me` 歷史列表,刻意含 cancelled 列)——語意上需要
--     看到非 waiting 列,不能換底。
--
-- 欄位顯式列出(不用 `SELECT *`),對齊 `20260704000001:137-143` 的
-- `waitlist_entries` 表定義(6 欄;全 repo 零後續 `ALTER TABLE
-- waitlist_entries`)。原表的 partial unique index(`uniq_waitlist_waiting`,
-- 謂詞為 `WHERE status = 'waiting'`)在 view 展開後仍維持適用資格(謂詞與
-- view 定義一致,inline 展開不阻斷 planner 匹配)。
-- =============================================================================

CREATE VIEW waiting_entries AS
SELECT id, user_id, course_id, status,
       created_at, updated_at
  FROM waitlist_entries
 WHERE status = 'waiting'::waitlist_status;
