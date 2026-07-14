//! HTTP integration tests for `/coaches/*` endpoints.

mod common;

use common::fixtures::seed_coach;
use common::http::spawn_test_app;
use dream_fly_backend::extractors::auth::role_cache_key;
use redis::AsyncCommands;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn list_coaches_public_returns_empty_initially(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/coaches").await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert!(body.is_array());
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn list_coaches_includes_seeded(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("coach1@example.com", "Password!234").await;
    seed_coach(&app.db, user.user_id, "Head Coach").await;

    let resp = app.get("/api/v1/coaches").await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["title"], "Head Coach");
    // slug/photo_url weren't set — must default to null.
    assert!(body[0]["slug"].is_null());
    assert!(body[0]["photo_url"].is_null());
}

#[sqlx::test]
async fn coach_list_and_detail_expose_slug_and_photo_url(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("coach-profile@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, user.user_id, "Profile Coach").await;
    sqlx::query("UPDATE coaches SET slug = $1, photo_url = $2 WHERE id = $3")
        .bind("profile-coach")
        .bind("https://cdn.example.com/coach.jpg")
        .bind(coach_id)
        .execute(&app.db)
        .await
        .expect("set coach profile fields");

    let list_resp = app.get("/api/v1/coaches").await;
    assert_eq!(list_resp.status_code(), 200);
    let list_body: serde_json::Value = list_resp.json();
    assert_eq!(list_body[0]["slug"], "profile-coach");
    assert_eq!(list_body[0]["photo_url"], "https://cdn.example.com/coach.jpg");

    let detail_resp = app.get(&format!("/api/v1/coaches/{coach_id}")).await;
    assert_eq!(detail_resp.status_code(), 200);
    let detail_body: serde_json::Value = detail_resp.json();
    assert_eq!(detail_body["coach"]["slug"], "profile-coach");
    assert_eq!(
        detail_body["coach"]["photo_url"],
        "https://cdn.example.com/coach.jpg"
    );
}

#[sqlx::test]
async fn list_and_detail_expose_coach_name_from_users(db: PgPool) {
    // `coaches` has no name column — it's joined from `users.name`. Set a
    // distinct name (different from the seeded `title`) so the assertion
    // proves the join, not a coincidental match.
    let app = spawn_test_app(db).await;
    let user = app.register_member("coach-name@example.com", "Password!234").await;
    sqlx::query("UPDATE users SET name = $1 WHERE id = $2")
        .bind("王教練")
        .bind(user.user_id)
        .execute(&app.db)
        .await
        .expect("set user name");
    let coach_id = seed_coach(&app.db, user.user_id, "資深體操教練").await;

    let list_resp = app.get("/api/v1/coaches").await;
    assert_eq!(list_resp.status_code(), 200);
    let list_body: serde_json::Value = list_resp.json();
    assert_eq!(list_body[0]["name"], "王教練");
    assert_eq!(list_body[0]["title"], "資深體操教練");

    let detail_resp = app.get(&format!("/api/v1/coaches/{coach_id}")).await;
    assert_eq!(detail_resp.status_code(), 200);
    let detail_body: serde_json::Value = detail_resp.json();
    assert_eq!(detail_body["coach"]["name"], "王教練");
}

#[sqlx::test]
async fn get_coach_by_id_returns_detail(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("coach2@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, user.user_id, "Specialist").await;

    let resp = app.get(&format!("/api/v1/coaches/{coach_id}")).await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["coach"]["id"].as_str().unwrap(), coach_id.to_string());
    assert!(body["schedules"].is_array());
}

#[sqlx::test]
async fn get_coach_unknown_id_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .get(&format!("/api/v1/coaches/{}", Uuid::now_v7()))
        .await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn coach_schedule_get_is_public(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("coach3@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, user.user_id, "Mentor").await;

    let resp = app.get(&format!("/api/v1/coaches/{coach_id}/schedule")).await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert!(body.is_array());
}

#[sqlx::test]
async fn coach_schedule_update_requires_auth(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("coach4@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, user.user_id, "Trainer").await;

    // No auth → 401
    let resp = app
        .put(&format!("/api/v1/coaches/{coach_id}/schedule"))
        .json(&json!({ "schedules": [] }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn clock_in_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("coach5@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, user.user_id, "Coach").await;

    let resp = app
        .post(&format!("/api/v1/coaches/{coach_id}/clock-in"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn clock_records_member_without_permission_is_forbidden(db: PgPool) {
    let app = spawn_test_app(db).await;
    let coach_user = app.register_member("coach6@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, coach_user.user_id, "Coach").await;
    let other = app.register_member("nosy@example.com", "Password!234").await;

    let resp = app
        .get(&format!("/api/v1/coaches/{coach_id}/clock-records"))
        .authorization_bearer(&other.access_token)
        .await;
    // The non-owning member has neither `admin` nor `coach` role, so the
    // handler returns 403. (Auth extractor must succeed first → not 401.)
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn clock_in_by_non_owning_member_returns_403(db: PgPool) {
    // The owning user of this coach profile never does anything; a
    // stranger logged in as a plain member tries to clock-in on behalf
    // of that coach and must be refused.
    let app = spawn_test_app(db).await;
    let owner = app.register_member("owner-ci@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, owner.user_id, "Real Coach").await;
    let intruder = app.register_member("intruder@example.com", "Password!234").await;

    let resp = app
        .post(&format!("/api/v1/coaches/{coach_id}/clock-in"))
        .authorization_bearer(&intruder.access_token)
        .json(&json!({"note": "hijacked"}))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn clock_in_happy_path_then_clock_out(db: PgPool) {
    // Owner clocks in (201/200 + record body), then clocks out. A second
    // clock-in while the first is still open should return 409. This
    // combines the happy path with the conflict defense in one e2e-style
    // test so we can assert on the record payload too.
    let app = spawn_test_app(db).await;
    let user = app.register_member("owner-ok@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, user.user_id, "Owner Coach").await;

    let resp1 = app
        .post(&format!("/api/v1/coaches/{coach_id}/clock-in"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"note": "shift start"}))
        .await;
    assert_eq!(resp1.status_code(), 200, "body={}", resp1.text());
    let rec1: serde_json::Value = resp1.json();
    assert_eq!(rec1["note"], "shift start");
    assert!(rec1["clock_out"].is_null(), "open record shouldn't have clock_out");

    let resp_dup = app
        .post(&format!("/api/v1/coaches/{coach_id}/clock-in"))
        .authorization_bearer(&user.access_token)
        .json(&json!({"note": null}))
        .await;
    assert_eq!(resp_dup.status_code(), 409);

    let resp_out = app
        .post(&format!("/api/v1/coaches/{coach_id}/clock-out"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp_out.status_code(), 200);
    let rec_out: serde_json::Value = resp_out.json();
    assert!(!rec_out["clock_out"].is_null(), "clock_out must be set after clock-out");
}

#[sqlx::test]
async fn clock_out_with_no_open_record_returns_404(db: PgPool) {
    // Fresh coach, never clocked in — clock-out has no matching row.
    let app = spawn_test_app(db).await;
    let user = app.register_member("owner-404@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, user.user_id, "Fresh Coach").await;

    let resp = app
        .post(&format!("/api/v1/coaches/{coach_id}/clock-out"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn update_schedule_by_stranger_returns_403(db: PgPool) {
    // Owner uploads nothing; a different authenticated member hits PUT
    // on /schedule → require_own_coach_profile must reject with 403 (not 401,
    // because the token is valid).
    let app = spawn_test_app(db).await;
    let owner = app.register_member("owner-sched@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, owner.user_id, "Owner").await;
    let stranger = app.register_member("stranger-sched@example.com", "Password!234").await;

    let resp = app
        .put(&format!("/api/v1/coaches/{coach_id}/schedule"))
        .authorization_bearer(&stranger.access_token)
        .json(&json!({"schedules": []}))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn update_schedule_by_admin_on_other_coach_succeeds(db: PgPool) {
    // An admin should be able to edit any coach's schedule even if they
    // aren't the coach's underlying user.
    let app = spawn_test_app(db).await;
    let owner = app.register_member("owner-adminfix@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, owner.user_id, "Needs Admin").await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .put(&format!("/api/v1/coaches/{coach_id}/schedule"))
        .authorization_bearer(&admin_token)
        .json(&json!({
            "schedules": [
                {
                    "day_of_week": 1,
                    "start_time": "09:00:00",
                    "end_time": "12:00:00",
                    "is_available": true
                }
            ]
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body.as_array().unwrap().len(), 1);
}

/// Phase 1 (EXCLUDE → 409): `replace_schedules` DELETEs the coach's existing
/// rows before INSERTing the new ones (coaches/repository.rs), so seeding a
/// pre-existing row and PUTting one overlapping entry would incorrectly
/// succeed — the old row is already gone by the time the new one is
/// inserted. The overlap has to be between two entries within the same
/// request body: same `day_of_week`, overlapping time ranges, violating
/// `coach_schedules_no_overlap` (SQLSTATE 23P01). Before the
/// `IntoResponse`/`conflict_on_exclusion` fix this surfaced as a bare 500.
#[sqlx::test]
async fn update_schedule_with_overlapping_entries_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let owner = app.register_member("owner-overlap@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, owner.user_id, "Overlap Coach").await;

    let resp = app
        .put(&format!("/api/v1/coaches/{coach_id}/schedule"))
        .authorization_bearer(&owner.access_token)
        .json(&json!({
            "schedules": [
                {
                    "day_of_week": 1,
                    "start_time": "09:00:00",
                    "end_time": "12:00:00",
                    "is_available": true
                },
                {
                    "day_of_week": 1,
                    "start_time": "11:00:00",
                    "end_time": "13:00:00",
                    "is_available": true
                }
            ]
        }))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn clock_in_on_nonexistent_coach_returns_404(db: PgPool) {
    // Valid token + random coach id — require_own_coach_profile's first step is
    // `find_by_id` → NotFound, which must surface as 404 (not 403).
    let app = spawn_test_app(db).await;
    let user = app.register_member("ghost@example.com", "Password!234").await;

    let resp = app
        .post(&format!("/api/v1/coaches/{}/clock-in", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 404);
}

// ---------------------------------------------------------------------------
// POST /coaches + PATCH /coaches/{id} (Round 4 Task B2)
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn create_coach_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("bind-noauth@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/coaches")
        .json(&json!({ "user_id": user.user_id, "title": "Coach" }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_coach_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let target = app.register_member("bind-target@example.com", "Password!234").await;
    let member = app.register_member("bind-mem@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/coaches")
        .authorization_bearer(&member.access_token)
        .json(&json!({ "user_id": target.user_id, "title": "Coach" }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn create_coach_as_admin_succeeds_and_assigns_coach_role(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let target = app.register_member("bind-ok@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/coaches")
        .authorization_bearer(&token)
        .json(&json!({
            "user_id": target.user_id,
            "title": "資深體操教練",
            "bio": "10年經驗",
            "specialties": ["gymnastics", "trampoline"],
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["user_id"].as_str().unwrap(), target.user_id.to_string());
    // `register_member` always names the user "Test Member" — proves the
    // insert's correlated subquery actually joins `users.name`, not just a
    // coincidental match.
    assert_eq!(body["name"], "Test Member");
    assert_eq!(body["title"], "資深體操教練");
    assert_eq!(body["bio"], "10年經驗");
    assert_eq!(body["specialties"], json!(["gymnastics", "trampoline"]));
    // Defaults for everything the request omitted.
    assert_eq!(body["is_active"], true);
    assert_eq!(body["display_order"], 0);
    assert_eq!(body["certifications"], json!([]));
    assert!(body["slug"].is_null());
    assert!(body["photo_url"].is_null());

    // coaches row exists for the target user.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM coaches WHERE user_id = $1")
        .bind(target.user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // The target user now has the `coach` role.
    let has_role: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id \
         WHERE ur.user_id = $1 AND r.name = 'coach')",
    )
    .bind(target.user_id)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert!(has_role, "target user must have the coach role assigned");
}

#[sqlx::test]
async fn create_coach_invalidates_stale_role_cache(db: PgPool) {
    // The target user's role set may already be cached (e.g. they logged in
    // as a plain member earlier). Binding them to a coach profile must
    // invalidate that cache so their very next request sees the new role
    // instead of waiting out the 15-minute TTL.
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let target = app.register_member("bind-cache@example.com", "Password!234").await;

    let mut redis = app.redis_conn().await;
    let cache_key = role_cache_key(target.user_id);
    let _: () = redis
        .set_ex(&cache_key, "member", 900)
        .await
        .expect("seed stale cache");

    let resp = app
        .post("/api/v1/coaches")
        .authorization_bearer(&token)
        .json(&json!({ "user_id": target.user_id, "title": "Coach" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());

    let exists: bool = redis.exists(&cache_key).await.expect("exists check");
    assert!(
        !exists,
        "role cache must be invalidated after coach role assignment"
    );
}

#[sqlx::test]
async fn create_coach_duplicate_user_id_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let target = app.register_member("bind-dup@example.com", "Password!234").await;

    let first = app
        .post("/api/v1/coaches")
        .authorization_bearer(&token)
        .json(&json!({ "user_id": target.user_id, "title": "Coach" }))
        .await;
    assert_eq!(first.status_code(), 200, "body={}", first.text());

    let second = app
        .post("/api/v1/coaches")
        .authorization_bearer(&token)
        .json(&json!({ "user_id": target.user_id, "title": "Another Title" }))
        .await;
    assert_eq!(second.status_code(), 409, "body={}", second.text());
}

#[sqlx::test]
async fn create_coach_nonexistent_user_id_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/coaches")
        .authorization_bearer(&token)
        .json(&json!({ "user_id": Uuid::now_v7(), "title": "Ghost Coach" }))
        .await;
    assert_eq!(resp.status_code(), 404);
}

#[sqlx::test]
async fn create_coach_duplicate_slug_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let user_a = app.register_member("bind-slug-a@example.com", "Password!234").await;
    let user_b = app.register_member("bind-slug-b@example.com", "Password!234").await;

    let first = app
        .post("/api/v1/coaches")
        .authorization_bearer(&token)
        .json(&json!({ "user_id": user_a.user_id, "title": "Coach A", "slug": "star-coach" }))
        .await;
    assert_eq!(first.status_code(), 200, "body={}", first.text());

    let second = app
        .post("/api/v1/coaches")
        .authorization_bearer(&token)
        .json(&json!({ "user_id": user_b.user_id, "title": "Coach B", "slug": "star-coach" }))
        .await;
    assert_eq!(second.status_code(), 409, "body={}", second.text());
}

#[sqlx::test]
async fn update_coach_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("patch-noauth@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, user.user_id, "Coach").await;

    let resp = app
        .patch(&format!("/api/v1/coaches/{coach_id}"))
        .json(&json!({ "title": "New Title" }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn update_coach_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("patch-owner@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, user.user_id, "Coach").await;
    let member = app.register_member("patch-mem@example.com", "Password!234").await;

    let resp = app
        .patch(&format!("/api/v1/coaches/{coach_id}"))
        .authorization_bearer(&member.access_token)
        .json(&json!({ "title": "New Title" }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn update_coach_as_admin_partial_update_only_title_changes(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let user = app.register_member("patch-ok@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, user.user_id, "Original Title").await;

    let resp = app
        .patch(&format!("/api/v1/coaches/{coach_id}"))
        .authorization_bearer(&token)
        .json(&json!({ "title": "Updated Title" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["title"], "Updated Title");
    // Proves the update's correlated subquery still joins `users.name`
    // correctly (same trick as the insert path).
    assert_eq!(body["name"], "Test Member");
    // `seed_coach` hardcodes these — the crux of the "partial update"
    // contract is that omitted fields are untouched, not reset.
    assert_eq!(body["bio"], "Test bio");
    assert_eq!(body["experience"], "5 years");
    assert_eq!(body["specialties"], json!(["gymnastics"]));
    assert_eq!(body["is_active"], true);
}

#[sqlx::test]
async fn update_coach_clears_photo_url_to_null(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let user = app.register_member("patch-clear@example.com", "Password!234").await;
    let coach_id = seed_coach(&app.db, user.user_id, "Coach").await;
    sqlx::query("UPDATE coaches SET photo_url = 'https://cdn.example.com/old.jpg' WHERE id = $1")
        .bind(coach_id)
        .execute(&app.db)
        .await
        .expect("seed photo_url");

    let resp = app
        .patch(&format!("/api/v1/coaches/{coach_id}"))
        .authorization_bearer(&token)
        .json(&json!({ "photo_url": null }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["photo_url"].is_null());
    // `title` wasn't in the patch body, so it must remain untouched — proves
    // the explicit-null path is distinct from "field absent".
    assert_eq!(body["title"], "Coach");

    let db_value: Option<String> =
        sqlx::query_scalar("SELECT photo_url FROM coaches WHERE id = $1")
            .bind(coach_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(
        db_value.is_none(),
        "photo_url must be NULL in the DB, not just absent from JSON"
    );
}

#[sqlx::test]
async fn update_coach_duplicate_slug_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;
    let user_a = app.register_member("patch-slug-a@example.com", "Password!234").await;
    let user_b = app.register_member("patch-slug-b@example.com", "Password!234").await;
    let coach_a = seed_coach(&app.db, user_a.user_id, "Coach A").await;
    let coach_b = seed_coach(&app.db, user_b.user_id, "Coach B").await;
    sqlx::query("UPDATE coaches SET slug = 'taken-slug' WHERE id = $1")
        .bind(coach_a)
        .execute(&app.db)
        .await
        .expect("seed slug");

    let resp = app
        .patch(&format!("/api/v1/coaches/{coach_b}"))
        .authorization_bearer(&token)
        .json(&json!({ "slug": "taken-slug" }))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn update_coach_unknown_id_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let resp = app
        .patch(&format!("/api/v1/coaches/{}", Uuid::now_v7()))
        .authorization_bearer(&token)
        .json(&json!({ "title": "Ghost" }))
        .await;
    assert_eq!(resp.status_code(), 404);
}
