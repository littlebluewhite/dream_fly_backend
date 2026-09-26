-- =============================================================================
-- course_sessions.venue:場次物化時快照時段場地。
--
-- 以前場次的場地是讀取時回頭 join `course_schedule_slots`(以
-- `(course_id, EXTRACT(DOW FROM session_date), start_time)` 對應)現查——
-- 時段一改開課時間,舊場次就對不回 slot、場地變 NULL;一改場地,連過去的
-- 場次也跟著改名。改為 `sessions::repository::materialize_range` 物化時把
-- slot 的 `venue` 寫進場次列,`reconcile_future_sessions_tx` 只對未來場次
-- (`session_date > today`)同步;讀取端直接讀 `course_sessions.venue`。
--
-- Backfill:以現行讀取規則(同一條 DOW + start_time join)回填既有場次,
-- 部署當下各讀取端回傳的場地與部署前完全相同——對不回 slot 的場次維持
-- NULL(即今天讀到的 null)。`course_schedule_slots_unique
-- (course_id, day_of_week, start_time)` 保證每列至多對到一個 slot。
-- 只有 ADD COLUMN(nullable、無 default,不重寫表)+ UPDATE,無破壞性操作。
-- =============================================================================

ALTER TABLE course_sessions ADD COLUMN venue TEXT;

UPDATE course_sessions cs
   SET venue = s.venue
  FROM course_schedule_slots s
 WHERE s.course_id = cs.course_id
   AND s.day_of_week = EXTRACT(DOW FROM cs.session_date)::smallint
   AND s.start_time = cs.start_time
   AND s.venue IS NOT NULL;
