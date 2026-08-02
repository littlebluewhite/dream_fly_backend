//! End-to-end flow tests — multi-endpoint user journeys that drive the full
//! router across module boundaries. These tests are the safety net that
//! catches cross-cutting regressions (e.g. when a cart-item schema change
//! subtly breaks checkout but leaves the unit tests green).

mod common;

use chrono::{Duration, NaiveTime, Utc};
use common::fixtures::{
    seed_coach, seed_coupon, seed_course, seed_course_session, seed_enrolment, seed_time_slot_full,
};
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

/// Register → login → fetch self → update profile → logout.
#[sqlx::test]
async fn e2e_user_onboarding(db: PgPool) {
    let app = spawn_test_app(db).await;

    // Register
    let reg = app
        .post("/api/v1/auth/register")
        .json(&json!({
            "email": "flow@example.com",
            "name": "Flow",
            "password": "Password!234",
        }))
        .await;
    assert_eq!(reg.status_code(), 200, "register body={}", reg.text());
    let reg_body: serde_json::Value = reg.json();
    let access = reg_body["access_token"].as_str().unwrap().to_string();
    let refresh = reg_body["refresh_token"].as_str().unwrap().to_string();

    // Fetch self
    let me = app
        .get("/api/v1/users/me")
        .authorization_bearer(&access)
        .await;
    assert_eq!(me.status_code(), 200);
    assert_eq!(me.json::<serde_json::Value>()["email"], "flow@example.com");

    // Update profile
    let upd = app
        .patch("/api/v1/users/me")
        .authorization_bearer(&access)
        .json(&json!({ "name": "Flow Final" }))
        .await;
    assert_eq!(upd.status_code(), 200);
    assert_eq!(upd.json::<serde_json::Value>()["name"], "Flow Final");

    // Logout
    let out = app
        .post("/api/v1/auth/logout")
        .json(&json!({ "refresh_token": refresh }))
        .await;
    assert_eq!(out.status_code(), 200);

    // Login again with fresh password
    let login = app
        .post("/api/v1/auth/login")
        .json(&json!({
            "email": "flow@example.com",
            "password": "Password!234",
        }))
        .await;
    assert_eq!(login.status_code(), 200);
}

/// Admin creates catalog → member browses → member books slot.
#[sqlx::test]
async fn e2e_booking_flow(db: PgPool) {
    let app = spawn_test_app(db).await;

    // Admin seeds a venue (via POST) and a slot (via DB — slot creation
    // needs a date/time combo the booking service accepts).
    let (_admin, admin_token) = app.seed_admin().await;
    let _venue_resp = app
        .post("/api/v1/venues")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "name": "Main Hall",
            "description": "Primary training hall",
        }))
        .await;
    let slot_id = seed_time_slot_full(&app.db, None, None, 3).await;

    // Member lists schedule availability for tomorrow + 2d.
    let date = (chrono::Utc::now() + chrono::Duration::days(2)).date_naive();
    let avail = app
        .get(&format!("/api/v1/schedule/availability?date={date}"))
        .await;
    assert_eq!(avail.status_code(), 200);
    assert!(
        avail
            .json::<serde_json::Value>()
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"].as_str().unwrap() == slot_id.to_string())
    );

    // Register member + book slot.
    let member = app
        .register_member("booker@example.com", "Password!234")
        .await;
    let booking = app
        .post("/api/v1/bookings")
        .authorization_bearer(&member.access_token)
        .json(&json!({ "time_slot_id": slot_id }))
        .await;
    assert_eq!(booking.status_code(), 200, "booking body={}", booking.text());
    let booking_body: serde_json::Value = booking.json();
    let booking_id = booking_body["id"].as_str().unwrap().to_string();

    // Member's bookings list includes it.
    let my = app
        .get("/api/v1/bookings/me")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(my.status_code(), 200);
    assert!(
        my.json::<serde_json::Value>()["bookings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"].as_str().unwrap() == booking_id)
    );
}

/// Member adds to cart → checkout → clears cart → sees order in my_orders.
#[sqlx::test]
async fn e2e_shopping_flow(db: PgPool) {
    let app = spawn_test_app(db).await;

    // Admin creates a product.
    let (_admin, admin_token) = app.seed_admin().await;
    let product: serde_json::Value = app
        .post("/api/v1/products")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "name": "Membership",
            "product_type": "membership",
            "price_cents": 12000,
        }))
        .await
        .json();
    let product_id = product["id"].as_str().unwrap().to_string();

    // Register + add to cart + checkout.
    let member = app.register_member("shopper@example.com", "Password!234").await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&member.access_token)
        .json(&json!({ "item_type": "product", "item_id": product_id, "quantity": 1 }))
        .await;

    let order = app
        .post("/api/v1/orders")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(order.status_code(), 200, "order body={}", order.text());
    let order_body: serde_json::Value = order.json();
    assert_eq!(order_body["total_cents"], 12000);

    // Cart is now empty.
    let cart = app
        .get("/api/v1/cart")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(cart.json::<serde_json::Value>()["items"].as_array().unwrap().len(), 0);

    // my_orders contains it.
    let my = app
        .get("/api/v1/orders/me")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(my.status_code(), 200);
    assert!(
        my.json::<serde_json::Value>()["orders"]
            .as_array()
            .unwrap()
            .len()
            >= 1
    );
}

/// Forgot password → capture token from MockEmailClient → reset → login.
#[sqlx::test]
async fn e2e_password_reset_flow(db: PgPool) {
    // Use a unique email so the per-account forgot-password Redis counter
    // (3 requests per hour per email) does not interfere when this test
    // runs back-to-back against the same Redis.
    let app = spawn_test_app(db).await;
    let email = format!("pw-{}@example.com", uuid::Uuid::now_v7());
    app.register_member(&email, "OldPassword!234").await;

    // Trigger forgot.
    let f = app
        .post("/api/v1/auth/password/forgot")
        .json(&json!({ "email": email }))
        .await;
    assert_eq!(f.status_code(), 200);

    // Recover the token from the mock email client.
    app.drain_background().await;
    let sent = app.email.sent();
    assert_eq!(sent.len(), 1);
    let token = sent[0].token.clone();

    // Reset
    let reset = app
        .post("/api/v1/auth/password/reset")
        .json(&json!({
            "token": token,
            "new_password": "NewPassword!234",
        }))
        .await;
    assert_eq!(reset.status_code(), 200, "reset body={}", reset.text());

    // Old password fails.
    let old = app
        .post("/api/v1/auth/login")
        .json(&json!({
            "email": email,
            "password": "OldPassword!234",
        }))
        .await;
    assert_eq!(old.status_code(), 401);

    // New password works.
    let new = app
        .post("/api/v1/auth/login")
        .json(&json!({
            "email": email,
            "password": "NewPassword!234",
        }))
        .await;
    assert_eq!(new.status_code(), 200);
}

/// Admin post lifecycle: admin create → member read → admin delete → 404.
#[sqlx::test]
async fn e2e_post_lifecycle(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin, token) = app.seed_admin().await;

    let created: serde_json::Value = app
        .post("/api/v1/posts")
        .authorization_bearer(&token)
        .json(&json!({
            "title": "Grand Opening",
            "content": "We are opening!",
            "category": "announcement",
        }))
        .await
        .json();
    let post_id = created["id"].as_str().unwrap().to_string();

    // Draft posts are hidden from the public `GET /posts/:id` path
    // (service::get_published_by_slug_or_id filters on `status='published'`),
    // so an anon read of a freshly-created draft returns 404.
    let read = app
        .get(&format!("/api/v1/posts/{post_id}"))
        .await;
    assert_eq!(read.status_code(), 404);

    let del = app
        .delete(&format!("/api/v1/posts/{post_id}"))
        .authorization_bearer(&token)
        .await;
    assert_eq!(del.status_code(), 204);

    let gone = app.get(&format!("/api/v1/posts/{post_id}")).await;
    assert_eq!(gone.status_code(), 404);
}

/// Register → add a course + a subscription product to cart → validate a
/// coupon → checkout (coupon applied) → the resulting enrolment,
/// subscription, and earned points show up in `/enrolments/me`,
/// `/subscriptions/me`, `/points/me`.
#[sqlx::test]
async fn e2e_checkout_with_course_and_subscription_artifacts(db: PgPool) {
    let app = spawn_test_app(db).await;

    // Admin creates a course (via DB fixture — course creation isn't the
    // point of this flow) and a subscription-eligible product (via the real
    // admin API).
    let (_admin, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "E2E Tumbling", None).await;
    let product: serde_json::Value = app
        .post("/api/v1/products")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "name": "E2E Membership",
            "product_type": "membership",
            "price_cents": 12000,
        }))
        .await
        .json();
    let product_id = product["id"].as_str().unwrap().to_string();

    seed_coupon(&app.db, "E2ECOUPON", 1000, true, None).await;

    // Register + add the course and the product to cart.
    let member = app
        .register_member("e2e-shopper@example.com", "Password!234")
        .await;
    app.post("/api/v1/cart/items")
        .authorization_bearer(&member.access_token)
        .json(&json!({ "item_type": "course", "item_id": course_id }))
        .await
        .assert_status_ok();
    app.post("/api/v1/cart/items")
        .authorization_bearer(&member.access_token)
        .json(&json!({ "item_type": "product", "item_id": product_id, "quantity": 1 }))
        .await
        .assert_status_ok();

    // Validate the coupon before checkout.
    let validate = app
        .get("/api/v1/coupons/E2ECOUPON/validate")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(validate.status_code(), 200);
    assert_eq!(validate.json::<serde_json::Value>()["discount_cents"], 1000);

    // Checkout with the coupon applied.
    let order = app
        .post("/api/v1/orders")
        .authorization_bearer(&member.access_token)
        .json(&json!({ "coupon_code": "E2ECOUPON" }))
        .await;
    assert_eq!(order.status_code(), 200, "order body={}", order.text());
    let order_body: serde_json::Value = order.json();
    // subtotal: course (50000, per seed_course fixture) + product (12000) - discount (1000).
    assert_eq!(order_body["total_cents"], 61_000);
    assert_eq!(order_body["discount_cents"], 1000);
    assert_eq!(order_body["coupon_code"], "E2ECOUPON");
    assert_eq!(order_body["enrolments"].as_array().unwrap().len(), 1);
    assert_eq!(order_body["subscriptions"].as_array().unwrap().len(), 1);
    let points_earned = order_body["points_earned"].as_i64().unwrap();
    assert!(points_earned > 0);

    // GET /enrolments/me reflects the new course enrolment.
    let enrolments = app
        .get("/api/v1/enrolments/me")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(enrolments.status_code(), 200);
    assert!(
        enrolments
            .json::<serde_json::Value>()
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["course_id"].as_str().unwrap() == course_id.to_string())
    );

    // GET /subscriptions/me reflects the new subscription.
    let subscriptions = app
        .get("/api/v1/subscriptions/me")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(subscriptions.status_code(), 200);
    assert!(
        subscriptions
            .json::<serde_json::Value>()
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["product_id"].as_str().unwrap() == product_id)
    );

    // GET /points/me reflects the points earned from checkout.
    let points = app
        .get("/api/v1/points/me")
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(points.status_code(), 200);
    assert_eq!(points.json::<serde_json::Value>()["balance"], points_earned);
}

/// Member leave request → coach approval (attendance projects to `leave`,
/// ADR-0008 「核准恆勝」) → makeup booking (seat ledger moves on both the
/// original and target session, contract §3.20 名額公式) → the coach's later
/// attempt to batch-mark that same, now-approved-leave member `present` is
/// rejected whole (ADR-0008 approved-guard, 422) — the one deliberate wire
/// change the ADR introduces.
#[sqlx::test]
async fn e2e_leave_makeup_projection_flow(db: PgPool) {
    let app = spawn_test_app(db).await;

    // Coach + course + two future sessions (the original session to take
    // leave from, and a later makeup target) — course/session creation isn't
    // the point of this flow, so seeded via fixtures like the other e2e
    // journeys' catalog setup.
    let (coach_user_id, coach_token) =
        app.seed_user_with_roles("e2e-leave-coach@example.com", &["coach"]).await;
    let coach_id = seed_coach(&app.db, coach_user_id, "E2E Leave Coach").await;
    let course_id = seed_course(&app.db, "E2E Leave Course", Some(coach_id)).await;
    let original_date = (Utc::now() + Duration::days(1)).date_naive();
    let original_session_id = seed_course_session(
        &app.db,
        course_id,
        original_date,
        NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
    )
    .await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session_id = seed_course_session(
        &app.db,
        course_id,
        target_date,
        NaiveTime::from_hms_opt(14, 0, 0).unwrap(),
        NaiveTime::from_hms_opt(15, 0, 0).unwrap(),
    )
    .await;

    // Register the member and give them an active enrolment.
    let member = app.register_member("e2e-leave-member@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    // --- 請假: the member requests leave for the original session. ---
    let leave_resp = app
        .post("/api/v1/leave-requests")
        .authorization_bearer(&member.access_token)
        .json(&json!({"session_id": original_session_id, "reason": "感冒"}))
        .await;
    assert_eq!(leave_resp.status_code(), 200, "body={}", leave_resp.text());
    let leave_body: serde_json::Value = leave_resp.json();
    assert_eq!(leave_body["status"], "pending");
    assert_eq!(leave_body["session_id"], original_session_id.to_string());
    let leave_id = leave_body["id"].as_str().unwrap().to_string();

    // --- 核准: the course's coach approves it. ---
    let decide_resp = app
        .patch(&format!("/api/v1/leave-requests/{leave_id}"))
        .authorization_bearer(&coach_token)
        .json(&json!({"status": "approved"}))
        .await;
    assert_eq!(decide_resp.status_code(), 200, "body={}", decide_resp.text());
    assert_eq!(decide_resp.json::<serde_json::Value>()["status"], "approved");

    // 核准恆勝 (ADR-0008): the same-tx attendance write projects onto the
    // original session's roster as `leave`.
    let roster_resp = app
        .get(&format!("/api/v1/sessions/{original_session_id}/roster"))
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(roster_resp.status_code(), 200, "body={}", roster_resp.text());
    let roster_body: serde_json::Value = roster_resp.json();
    let roster_entry = roster_body
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["enrolment_id"] == enrolment_id.to_string())
        .expect("member's roster entry present");
    assert_eq!(roster_entry["attendance_status"], "leave");

    // --- 補課: the member books a makeup into the future target session. ---
    let makeup_resp = app
        .post(&format!("/api/v1/leave-requests/{leave_id}/makeup"))
        .authorization_bearer(&member.access_token)
        .json(&json!({"session_id": target_session_id}))
        .await;
    assert_eq!(makeup_resp.status_code(), 200, "body={}", makeup_resp.text());
    assert_eq!(
        makeup_resp.json::<serde_json::Value>()["makeup_session_id"],
        target_session_id.to_string()
    );

    // 座位帳兩邊 (§3.20 名額公式): the original session's approved-leave count
    // (leave releases a seat there) and the target session's makeup count
    // (makeup occupies a seat there) each moved by exactly one.
    let original_leave_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM leave_requests \
         WHERE session_id = $1 AND status = 'approved'::leave_status",
    )
    .bind(original_session_id)
    .fetch_one(&app.db)
    .await
    .expect("count approved leave for original session");
    assert_eq!(original_leave_count, 1, "original session must show one seat freed by leave");

    let target_makeup_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM leave_requests WHERE makeup_session_id = $1")
            .bind(target_session_id)
            .fetch_one(&app.db)
            .await
            .expect("count makeups booked into target session");
    assert_eq!(target_makeup_count, 1, "target session must show one seat occupied by makeup");

    // --- approved-guard (ADR-0008): advance the mock clock to the original
    // session's start so the PUT attendance time gate itself is satisfied,
    // isolating the approved-guard 422 as the one under test.
    app.clock.set(original_date.and_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap()).and_utc());
    let guard_resp = app
        .put(&format!("/api/v1/sessions/{original_session_id}/attendance"))
        .authorization_bearer(&coach_token)
        .json(&json!({"records": [{"enrolment_id": enrolment_id, "status": "present"}]}))
        .await;
    assert_eq!(guard_resp.status_code(), 422, "body={}", guard_resp.text());
    assert_eq!(
        guard_resp.json::<serde_json::Value>()["error"],
        "cannot overwrite an approved leave with present/absent"
    );

    // The rejected batch must leave the leave projection untouched.
    let roster_after_resp = app
        .get(&format!("/api/v1/sessions/{original_session_id}/roster"))
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(roster_after_resp.status_code(), 200, "body={}", roster_after_resp.text());
    let roster_after_body: serde_json::Value = roster_after_resp.json();
    let entry_after = roster_after_body
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["enrolment_id"] == enrolment_id.to_string())
        .expect("member's roster entry present");
    assert_eq!(
        entry_after["attendance_status"], "leave",
        "approved-guard must leave the projection as leave"
    );
}

