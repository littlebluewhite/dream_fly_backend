-- =============================================================================
-- C2: 出席口徑(present/absent 計入分母、leave 排除)下沉為 view 單一真相。
--
-- Before this migration, reports 的 7 條查詢(`kpis`/`coach_reports`/
-- `attendance_distribution`/`retention`/`weekday_load`/
-- `coach_attendance_in_range`/`member_attendance`)各自手寫同一條規則
-- (`status = 'present'::attendance_status` 或
-- `status IN ('present','absent')`)——同一口徑有 7 份獨立 SQL 拷貝,全靠人
-- 肉保持一致。
--
-- 語意:view 的成員資格(`WHERE status IN (present, absent)`)= 計入分母,
-- leave 被排除在外;`is_present` 欄 = 分子。因此在 view 內
-- `NOT is_present ⇔ status = 'absent'`(leave 已經不在 view 裡,不會被誤
-- 算成 absent)。欄位顯式列出(不用 `SELECT *`),BI/psql 直查此 view 即
-- 得口徑,不必重新實作規則。
-- =============================================================================

CREATE VIEW countable_attendance AS
SELECT id, session_id, enrolment_id, status,
       (status = 'present'::attendance_status) AS is_present,
       marked_by, marked_at, created_at
  FROM attendance_records
 WHERE status IN ('present'::attendance_status, 'absent'::attendance_status);
