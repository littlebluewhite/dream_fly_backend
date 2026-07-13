-- =============================================================================
-- WS-1: 有效報名口徑(目前占用座位的報名)下沉為 view 單一真相。
--
-- Before this migration, ~22 個 READ 站點(courses::seats 的座位 COUNT、
-- courses/sessions repository 的 enrolled_count 顯示子查詢、attendance/leave
-- 的 active-enrolment 查找、reports 的會員/課程/教練統計……橫跨 7 個
-- module)各自手寫 `status = 'active'` 篩選,判斷「這筆報名目前算不算數」
-- ——同一口徑有 22 份獨立 SQL 拷貝,全靠人肉保持一致。
--
-- 口徑:「目前占用座位的有效報名」,即 `status = 'active'`。刻意排除兩類
-- 站點,不下沉進本 view——這也回答了「為何那些站點不換底」:
-- (1) reports 的 3 個「報名事件」站點(`kpis` 的 new_enrolments_this/_last、
--     `funnel` 的 new_enrolments):用 `status <> 'cancelled'` + `created_at`
--     分桶,量的是「這個月發生了幾次報名動作」,是事件流口徑,不是「現在
--     還占不占位」——與本 view 語意不同,不該共用同一份定義。
-- (2) enrolments 寫側的狀態轉移語句(INSERT 的 'active' 字面、cancel 的
--     UPDATE ... WHERE status <> 'cancelled' 守衛):寫側正在改變狀態,
--     天生不能讀自己正在寫的 view。
--
-- enum `enrolment_status` 目前僅 `{active, cancelled}` 兩值,上述排除站點
-- 今天的篩選結果與「是否落在 view 內」完全等價——這只是替未來第三狀態
-- (例如 waitlisted/expired)預留分岔點的 future-proofing,不代表今天已有
-- 實際行為差異。
--
-- 欄位顯式列出(不用 `SELECT *`),對齊 `20260704000001:119-127` 的
-- `enrolments` 表定義(8 欄;全 repo 零後續 `ALTER TABLE enrolments`)。原表
-- 兩個 partial index(`uniq_enrolments_active`/`idx_enrolments_course_active`,
-- 謂詞皆為 `WHERE status = 'active'`)在 view 展開後仍維持適用資格(謂詞與
-- view 定義一致,inline 展開不阻斷 planner 匹配);實際是否選用仍依 planner
-- 統計決定,必要時可對代表性查詢 `EXPLAIN` 抽查。
-- =============================================================================

CREATE VIEW active_enrolments AS
SELECT id, user_id, course_id, order_id, status,
       enrolled_at, created_at, updated_at
  FROM enrolments
 WHERE status = 'active'::enrolment_status;
