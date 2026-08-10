-- =============================================================================
-- Phase 7:公開文章「已發布」口徑下沉為 view 單一真相。
--
-- Before this migration, 4 個 READ 站點(posts repository 的
-- `find_published`、`count_published`、`find_published_by_slug`、
-- `find_published_by_id`)各自手寫 `status = 'published'` 篩選,判斷「這篇
-- 文章目前算不算公開可見」——同一口徑有 4 份獨立 SQL 拷貝,全靠人肉保持
-- 一致。
--
-- 刻意排除、不下沉進本 view 的站點——這也回答了「為何那些站點不換底」:
-- (1) any-status 讀側:`find_by_slug`/`find_by_id`,供
--     `posts::service::get_by_slug_or_id`(作者本人/管理端檢視,含 draft/
--     archived)與 `update_post` 的所有權前置讀(`find_by_id`,更新時要看
--     得到當下狀態,不論是不是 published)使用——語意上需要看到非
--     published 列,不能換底。
-- (2) 寫側:`create` 的 INSERT 欄位字面值、`update` 的 `RETURNING`、
--     `delete`——寫側不能讀自己正在寫的 view,且 RETURNING 本就要回傳剛
--     寫入的列(可能仍是 draft)。
--
-- `post_status` enum 只有 `{draft, published, archived}` 三態,語意底定,
-- 不像候補/報名 view 有「未來可能加第三態」的預留動機——本 view 純粹是
-- 既有二態(published / 非 published)口徑的讀取收斂。
--
-- 欄位顯式列出(不用 `SELECT *`),對齊 `20260410000001:518-531` 的
-- `posts` 表定義(12 欄;全 repo 零後續 `ALTER TABLE posts`)。原表的
-- partial index(`idx_posts_published_feed`,謂詞為
-- `WHERE status = 'published'`)在 view 展開後仍維持適用資格(謂詞與 view
-- 定義一致,inline 展開不阻斷 planner 匹配)。
-- =============================================================================

CREATE VIEW published_posts AS
SELECT id, author_id, title, slug, content, excerpt, category, status,
       cover_image, published_at, created_at, updated_at
  FROM posts
 WHERE status = 'published'::post_status;
