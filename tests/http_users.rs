//! HTTP integration tests for `/users/*` endpoints.

mod common;

use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn me_without_token_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/users/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn me_with_token_returns_own_profile(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("me@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["email"], "me@example.com");
    assert_eq!(body["id"].as_str().unwrap(), user.user_id.to_string());
    assert_eq!(body["roles"], json!(["member"]));
}

#[sqlx::test]
async fn me_as_admin_returns_admin_role(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/users/me")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let roles = body["roles"].as_array().expect("roles array");
    assert!(roles.contains(&json!("admin")));
}

/// Task 18: `UserResponse` gained `points_balance` (frontend admin members page
/// needs it). `register_member`'s underlying INSERT relies on the commerce
/// migration's `DEFAULT 0`, so this bumps it to a known non-zero value first —
/// proving the field is actually read off the row, not just defaulting to 0.
#[sqlx::test]
async fn me_includes_points_balance(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("points@example.com", "Password!234").await;
    sqlx::query("UPDATE users SET points_balance = $2 WHERE id = $1")
        .bind(user.user_id)
        .bind(500_i64)
        .execute(&app.db)
        .await
        .expect("bump points_balance");

    let resp = app
        .get("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["points_balance"], 500);
}

#[sqlx::test]
async fn update_me_changes_name(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("update@example.com", "Password!234").await;

    let resp = app
        .patch("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "Brand New Name" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["name"], "Brand New Name");
    // The frontend refreshes its local user state from this response, so
    // roles must survive a profile update — not get washed to [].
    assert_eq!(body["roles"], json!(["member"]));
}

#[sqlx::test]
async fn update_me_rejects_short_name(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("val@example.com", "Password!234").await;

    let resp = app
        .patch("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "X" }))
        .await;
    assert_eq!(resp.status_code(), 422);
}

#[sqlx::test]
async fn list_users_as_admin_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    // Seed a few users + an admin.
    app.register_member("u1@example.com", "Password!234").await;
    app.register_member("u2@example.com", "Password!234").await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/users")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["users"].as_array().unwrap().len() >= 3);
}

#[sqlx::test]
async fn list_users_as_admin_includes_roles_per_user(db: PgPool) {
    let app = spawn_test_app(db).await;
    let member = app.register_member("listroles@example.com", "Password!234").await;
    let (admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/users")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let users = body["users"].as_array().expect("users array");

    let member_entry = users
        .iter()
        .find(|u| u["id"] == member.user_id.to_string())
        .expect("member present in list");
    assert_eq!(member_entry["roles"], json!(["member"]));

    let admin_entry = users
        .iter()
        .find(|u| u["id"] == admin_id.to_string())
        .expect("admin present in list");
    let admin_roles = admin_entry["roles"].as_array().expect("roles array");
    assert!(admin_roles.contains(&json!("admin")));
}

/// Task 18: the admin members page needs `points_balance` on each row of
/// `GET /users`, not just `GET /users/me`.
#[sqlx::test]
async fn list_users_as_admin_includes_points_balance(db: PgPool) {
    let app = spawn_test_app(db).await;
    let member = app.register_member("listpoints@example.com", "Password!234").await;
    sqlx::query("UPDATE users SET points_balance = $2 WHERE id = $1")
        .bind(member.user_id)
        .bind(300_i64)
        .execute(&app.db)
        .await
        .expect("bump points_balance");
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/users")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let users = body["users"].as_array().expect("users array");
    let entry = users
        .iter()
        .find(|u| u["id"] == member.user_id.to_string())
        .expect("member present in list");
    assert_eq!(entry["points_balance"], 300);
}

#[sqlx::test]
async fn list_users_as_member_returns_forbidden(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("mem@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/users")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn get_user_by_id_as_admin_returns_profile(db: PgPool) {
    let app = spawn_test_app(db).await;
    let target = app.register_member("target@example.com", "Password!234").await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get(&format!("/api/v1/users/{}", target.user_id))
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["email"], "target@example.com");
}

#[sqlx::test]
async fn get_user_by_id_as_member_returns_403(db: PgPool) {
    // `/users/:id` is admin-only — a plain member reading another user's
    // full profile by UUID must be rejected before the DB is consulted.
    let app = spawn_test_app(db).await;
    let target = app.register_member("victim@example.com", "Password!234").await;
    let caller = app.register_member("peeper@example.com", "Password!234").await;

    let resp = app
        .get(&format!("/api/v1/users/{}", target.user_id))
        .authorization_bearer(&caller.access_token)
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn get_user_nonexistent_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let ghost = Uuid::now_v7();
    let resp = app
        .get(&format!("/api/v1/users/{ghost}"))
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 404);
}

// ---------------------------------------------------------------------
// Task 7: `POST /users` / `PATCH /users/{id}` (admin member management)
// ---------------------------------------------------------------------

#[sqlx::test]
async fn admin_create_user_succeeds_and_new_credentials_can_login(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/users")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "email": "newmember@example.com",
            "name": "New Member",
            "phone": "0912345678",
            "password": "Password!234",
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["email"], "newmember@example.com");
    assert_eq!(body["name"], "New Member");
    assert_eq!(body["phone"], "0912345678");
    assert_eq!(body["is_active"], true);
    assert_eq!(body["roles"], json!(["member"]));

    // Full round-trip: the freshly-created credentials must actually work.
    let login_resp = app
        .post("/api/v1/auth/login")
        .json(&json!({
            "email": "newmember@example.com",
            "password": "Password!234",
        }))
        .await;
    assert_eq!(login_resp.status_code(), 200, "body={}", login_resp.text());
    let login_body: serde_json::Value = login_resp.json();
    assert_eq!(login_body["user"]["email"], "newmember@example.com");
    assert!(login_body["access_token"].as_str().is_some());
}

#[sqlx::test]
async fn admin_create_user_duplicate_email_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let payload = json!({
        "email": "dupe@example.com",
        "name": "First Caller",
        "password": "Password!234",
    });

    let first = app
        .post("/api/v1/users")
        .authorization_bearer(&admin_token)
        .json(&payload)
        .await;
    assert_eq!(first.status_code(), 200, "body={}", first.text());

    let second = app
        .post("/api/v1/users")
        .authorization_bearer(&admin_token)
        .json(&payload)
        .await;
    assert_eq!(second.status_code(), 409, "body={}", second.text());
    let body: serde_json::Value = second.json();
    assert_eq!(body["error"], "Email 已被使用");
}

#[sqlx::test]
async fn admin_create_user_short_password_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/users")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "email": "shortpass@example.com",
            "name": "Short Pass",
            "password": "short1",
        }))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn admin_create_user_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let caller = app.register_member("caller@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/users")
        .authorization_bearer(&caller.access_token)
        .json(&json!({
            "email": "victim@example.com",
            "name": "Victim",
            "password": "Password!234",
        }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

#[sqlx::test]
async fn admin_update_user_partial_updates_name_only(db: PgPool) {
    let app = spawn_test_app(db).await;
    let target = app.register_member("patchme@example.com", "Password!234").await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .patch(&format!("/api/v1/users/{}", target.user_id))
        .authorization_bearer(&admin_token)
        .json(&json!({ "name": "Renamed By Admin" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["name"], "Renamed By Admin");
    // Untouched fields survive the partial update.
    assert_eq!(body["email"], "patchme@example.com");
}

/// Task 7: `admin_update`'s phone-change path mirrors `PATCH /users/me`'s
/// existing invariant — a real phone change resets `phone_verified` to
/// `false`, since an admin-set number is exactly as unverified as a
/// self-service one until OTP confirms it.
#[sqlx::test]
async fn admin_update_user_phone_change_resets_phone_verified(db: PgPool) {
    let app = spawn_test_app(db).await;
    let target = app.register_member("verifiedphone@example.com", "Password!234").await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    // Arrange: pretend the user already verified an old phone number.
    sqlx::query("UPDATE users SET phone = $2, phone_verified = true WHERE id = $1")
        .bind(target.user_id)
        .bind("0911111111")
        .execute(&app.db)
        .await
        .expect("seed verified phone");

    let resp = app
        .patch(&format!("/api/v1/users/{}", target.user_id))
        .authorization_bearer(&admin_token)
        .json(&json!({ "phone": "0922222222" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["phone"], "0922222222");
    assert_eq!(body["phone_verified"], false);
}

/// Task 7: `email`/`roles`/`password` are not fields on `UpdateUserRequest`
/// — proves a body that includes them anyway leaves all three untouched
/// (email/roles read back unchanged; the OLD password still authenticates).
#[sqlx::test]
async fn admin_update_user_ignores_email_roles_and_password_fields(db: PgPool) {
    let app = spawn_test_app(db).await;
    let target = app.register_member("immutable@example.com", "Password!234").await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .patch(&format!("/api/v1/users/{}", target.user_id))
        .authorization_bearer(&admin_token)
        .json(&json!({
            "name": "Still Renamed",
            "email": "hacked@example.com",
            "roles": ["admin"],
            "password": "NewPassword!234",
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["name"], "Still Renamed");
    assert_eq!(body["email"], "immutable@example.com");
    assert_eq!(body["roles"], json!(["member"]));

    // The old password must still work — proof the `password` key was
    // silently ignored rather than applied.
    let login_resp = app
        .post("/api/v1/auth/login")
        .json(&json!({ "email": "immutable@example.com", "password": "Password!234" }))
        .await;
    assert_eq!(login_resp.status_code(), 200, "body={}", login_resp.text());
}

#[sqlx::test]
async fn admin_update_user_no_fields_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let target = app.register_member("emptyupdate@example.com", "Password!234").await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .patch(&format!("/api/v1/users/{}", target.user_id))
        .authorization_bearer(&admin_token)
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["error"], "至少提供一個欄位");
}

#[sqlx::test]
async fn admin_update_user_nonexistent_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let ghost = Uuid::now_v7();
    let resp = app
        .patch(&format!("/api/v1/users/{ghost}"))
        .authorization_bearer(&admin_token)
        .json(&json!({ "name": "Ghost" }))
        .await;
    assert_eq!(resp.status_code(), 404);
}

/// Task 7's key deactivation test. Deliberately re-authenticates via
/// `/auth/login` (which reads `is_active` straight from the DB) rather than
/// replaying `target.access_token` against a protected route — the latter
/// depends on the `AuthUser` extractor's 60s `user_active` Redis cache TTL
/// and would be a flaky/misleading way to prove the deactivation took effect.
#[sqlx::test]
async fn admin_deactivate_user_then_login_is_rejected(db: PgPool) {
    let app = spawn_test_app(db).await;
    let target = app.register_member("deactivate@example.com", "Password!234").await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    // Sanity check: login succeeds before deactivation.
    let pre = app
        .post("/api/v1/auth/login")
        .json(&json!({ "email": "deactivate@example.com", "password": "Password!234" }))
        .await;
    assert_eq!(pre.status_code(), 200, "body={}", pre.text());

    let patch_resp = app
        .patch(&format!("/api/v1/users/{}", target.user_id))
        .authorization_bearer(&admin_token)
        .json(&json!({ "is_active": false }))
        .await;
    assert_eq!(patch_resp.status_code(), 200, "body={}", patch_resp.text());
    let body: serde_json::Value = patch_resp.json();
    assert_eq!(body["is_active"], false);

    let login_resp = app
        .post("/api/v1/auth/login")
        .json(&json!({ "email": "deactivate@example.com", "password": "Password!234" }))
        .await;
    assert_eq!(login_resp.status_code(), 401, "body={}", login_resp.text());
}

#[sqlx::test]
async fn admin_update_user_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let target = app.register_member("target2@example.com", "Password!234").await;
    let caller = app.register_member("caller2@example.com", "Password!234").await;

    let resp = app
        .patch(&format!("/api/v1/users/{}", target.user_id))
        .authorization_bearer(&caller.access_token)
        .json(&json!({ "name": "Nope" }))
        .await;
    assert_eq!(resp.status_code(), 403);
}

// ---------------------------------------------------------------------
// Task B7: `users.preferences` JSONB (mobile settings toggles)
// ---------------------------------------------------------------------

#[sqlx::test]
async fn update_me_sets_preferences_and_get_me_returns_it(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("prefs@example.com", "Password!234").await;

    let patch_resp = app
        .patch("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "preferences": { "class_reminder": true, "dark": false } }))
        .await;
    assert_eq!(patch_resp.status_code(), 200, "body={}", patch_resp.text());
    let patch_body: serde_json::Value = patch_resp.json();
    assert_eq!(
        patch_body["preferences"],
        json!({ "class_reminder": true, "dark": false })
    );

    let get_resp = app
        .get("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(get_resp.status_code(), 200, "body={}", get_resp.text());
    let get_body: serde_json::Value = get_resp.json();
    assert_eq!(
        get_body["preferences"],
        json!({ "class_reminder": true, "dark": false })
    );
}

/// Regression: a `PATCH /users/me` body that omits `preferences` entirely
/// must leave the previously-stored value untouched (same COALESCE
/// semantics as `name`/`phone`/`avatar_url`).
#[sqlx::test]
async fn update_me_without_preferences_field_leaves_existing_value_unchanged(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("prefs-keep@example.com", "Password!234").await;

    app.patch("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "preferences": { "promo": true } }))
        .await;

    let resp = app
        .patch("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "name": "Renamed, No Prefs In Body" }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["name"], "Renamed, No Prefs In Body");
    assert_eq!(body["preferences"], json!({ "promo": true }));
}

/// Contract: `preferences` is a **whole-object overwrite**, not a deep
/// merge — sending a new object must drop keys from the old one.
#[sqlx::test]
async fn update_me_preferences_full_overwrite_drops_old_keys(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("prefs-overwrite@example.com", "Password!234").await;

    app.patch("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "preferences": { "class_reminder": true, "coach_msg": true } }))
        .await;

    let resp = app
        .patch("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .json(&json!({ "preferences": { "dark": true } }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    // Only the new object survives — no merge with `class_reminder`/`coach_msg`.
    assert_eq!(body["preferences"], json!({ "dark": true }));
}

/// A user who has never touched `preferences` must see `null`, not a 500.
#[sqlx::test]
async fn me_returns_null_preferences_when_never_set(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("prefs-unset@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/users/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["preferences"].is_null());
}
