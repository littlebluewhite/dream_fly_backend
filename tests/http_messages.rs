//! HTTP integration tests for the messages module's endpoints:
//! `POST /conversations`, `GET /conversations/me`,
//! `GET /conversations/{id}/messages`, `POST /conversations/{id}/messages`,
//! `PATCH /conversations/{id}/read`.

mod common;

use chrono::{DateTime, Duration, Utc};
use common::fixtures::{seed_coach, seed_message};
use common::http::{TestApp, spawn_test_app};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// Create a conversation via the real `POST /conversations` endpoint and
/// return its id. Used by every test below that needs an existing
/// conversation to act on — asserts 200 along the way so a role-check
/// regression fails loudly at the fixture call site instead of later.
async fn create_conversation(app: &TestApp, caller_token: &str, target_user_id: Uuid) -> Uuid {
    let resp = app
        .post("/api/v1/conversations")
        .authorization_bearer(caller_token)
        .json(&json!({"user_id": target_user_id}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    Uuid::parse_str(body["id"].as_str().expect("id")).expect("parse conversation id")
}

// ---------------------------------------------------------------------------
// POST /conversations
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn create_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/conversations")
        .json(&json!({"user_id": Uuid::now_v7()}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_role_violation_both_members_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let member_a = app
        .register_member("msg-both-member-a@example.com", "Password!234")
        .await;
    let member_b = app
        .register_member("msg-both-member-b@example.com", "Password!234")
        .await;

    let resp = app
        .post("/api/v1/conversations")
        .authorization_bearer(&member_a.access_token)
        .json(&json!({"user_id": member_b.user_id}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn create_role_violation_admin_only_returns_422(db: PgPool) {
    // An admin with neither `coach` nor `member` fails the role check just
    // like two members do — the rule is "one coach + one member", not
    // "any authenticated user".
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let member = app
        .register_member("msg-admin-target@example.com", "Password!234")
        .await;

    let resp = app
        .post("/api/v1/conversations")
        .authorization_bearer(&admin_token)
        .json(&json!({"user_id": member.user_id}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn create_targeting_self_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-self@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "Self Coach").await;

    let resp = app
        .post("/api/v1/conversations")
        .authorization_bearer(&coach_token)
        .json(&json!({"user_id": coach_user_id}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn create_between_coach_and_member_is_order_independent_and_idempotent(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-order-coach@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "Order Coach").await;
    let member = app
        .register_member("msg-order-member@example.com", "Password!234")
        .await;

    // Caller = coach, target = member.
    let resp1 = app
        .post("/api/v1/conversations")
        .authorization_bearer(&coach_token)
        .json(&json!({"user_id": member.user_id}))
        .await;
    assert_eq!(resp1.status_code(), 200, "body={}", resp1.text());
    let body1: serde_json::Value = resp1.json();
    assert_eq!(body1["member_id"], member.user_id.to_string());
    assert_eq!(body1["coach_id"], coach_user_id.to_string());
    assert!(body1["last_message_at"].is_null());
    let conv_id_1 = body1["id"].as_str().unwrap().to_string();

    // Caller = member, target = coach — must resolve to the SAME
    // conversation (order-independence + get-or-create idempotency).
    let resp2 = app
        .post("/api/v1/conversations")
        .authorization_bearer(&member.access_token)
        .json(&json!({"user_id": coach_user_id}))
        .await;
    assert_eq!(resp2.status_code(), 200, "body={}", resp2.text());
    let body2: serde_json::Value = resp2.json();
    assert_eq!(
        body2["id"], conv_id_1,
        "must get-or-create the same conversation regardless of caller order"
    );
    assert_eq!(body2["member_id"], member.user_id.to_string());
    assert_eq!(body2["coach_id"], coach_user_id.to_string());

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM conversations WHERE member_id = $1 AND coach_id = $2",
    )
    .bind(member.user_id)
    .bind(coach_user_id)
    .fetch_one(&app.db)
    .await
    .expect("count conversations");
    assert_eq!(
        count, 1,
        "must not create a duplicate row on the second call"
    );
}

// ---------------------------------------------------------------------------
// GET /conversations/me
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn me_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/conversations/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn me_returns_peer_name_last_message_and_unread_count(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-me-coach@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "Me Coach").await;
    let member = app
        .register_member("msg-me-member@example.com", "Password!234")
        .await;
    let conv_id = create_conversation(&app, &coach_token, member.user_id).await;

    // Brand-new conversation, zero messages yet: last_message_body/
    // last_message_at must be null and unread_count zero, not an error.
    let empty_resp = app
        .get("/api/v1/conversations/me")
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(empty_resp.status_code(), 200, "body={}", empty_resp.text());
    let empty_body: serde_json::Value = empty_resp.json();
    assert!(empty_body[0]["last_message_body"].is_null());
    assert!(empty_body[0]["last_message_at"].is_null());
    assert_eq!(empty_body[0]["unread_count"], 0);

    // Member sends two messages to the coach; coach hasn't sent any yet, so
    // from the coach's perspective both are unread ("對方寄出且未讀").
    seed_message(
        &app.db,
        conv_id,
        member.user_id,
        "哈囉教練",
        None,
        Utc::now() - Duration::seconds(2),
    )
    .await;
    seed_message(
        &app.db,
        conv_id,
        member.user_id,
        "今天有課嗎？",
        None,
        Utc::now() - Duration::seconds(1),
    )
    .await;
    sqlx::query("UPDATE conversations SET last_message_at = $2 WHERE id = $1")
        .bind(conv_id)
        .bind(Utc::now() - Duration::seconds(1))
        .execute(&app.db)
        .await
        .expect("set last_message_at");

    let resp = app
        .get("/api/v1/conversations/me")
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], conv_id.to_string());
    assert_eq!(arr[0]["peer_id"], member.user_id.to_string());
    assert_eq!(arr[0]["peer_name"], "Test Member");
    assert_eq!(arr[0]["last_message_body"], "今天有課嗎？");
    assert_eq!(arr[0]["unread_count"], 2);
    assert!(arr[0]["last_message_at"].as_str().is_some());
}

#[sqlx::test]
async fn me_truncates_last_message_body_to_100_chars(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-truncate-coach@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "Truncate Coach").await;
    let member = app
        .register_member("msg-truncate-member@example.com", "Password!234")
        .await;
    let conv_id = create_conversation(&app, &coach_token, member.user_id).await;

    let long_body: String = std::iter::repeat_n('字', 150).collect();
    seed_message(
        &app.db,
        conv_id,
        member.user_id,
        &long_body,
        None,
        Utc::now(),
    )
    .await;

    let resp = app
        .get("/api/v1/conversations/me")
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let truncated = body[0]["last_message_body"]
        .as_str()
        .expect("last_message_body");
    assert_eq!(truncated.chars().count(), 100);
    assert_eq!(truncated, long_body.chars().take(100).collect::<String>());
}

// ---------------------------------------------------------------------------
// GET /conversations/{id}/messages
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn list_messages_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .get(&format!(
            "/api/v1/conversations/{}/messages",
            Uuid::now_v7()
        ))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn list_messages_conversation_not_found_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("msg-list-404@example.com", "Password!234")
        .await;
    let resp = app
        .get(&format!(
            "/api/v1/conversations/{}/messages",
            Uuid::now_v7()
        ))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn list_messages_non_participant_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-list-403-coach@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "List403 Coach").await;
    let member = app
        .register_member("msg-list-403-member@example.com", "Password!234")
        .await;
    let outsider = app
        .register_member("msg-list-403-outsider@example.com", "Password!234")
        .await;
    let conv_id = create_conversation(&app, &coach_token, member.user_id).await;

    let resp = app
        .get(&format!("/api/v1/conversations/{conv_id}/messages"))
        .authorization_bearer(&outsider.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn list_messages_returns_paginated_envelope_newest_first(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-list-page-coach@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "Page Coach").await;
    let member = app
        .register_member("msg-list-page-member@example.com", "Password!234")
        .await;
    let conv_id = create_conversation(&app, &coach_token, member.user_id).await;

    let now = Utc::now();
    seed_message(
        &app.db,
        conv_id,
        member.user_id,
        "first",
        None,
        now - Duration::seconds(3),
    )
    .await;
    seed_message(
        &app.db,
        conv_id,
        coach_user_id,
        "second",
        None,
        now - Duration::seconds(2),
    )
    .await;
    seed_message(
        &app.db,
        conv_id,
        member.user_id,
        "third",
        None,
        now - Duration::seconds(1),
    )
    .await;

    let resp = app
        .get(&format!(
            "/api/v1/conversations/{conv_id}/messages?page=1&per_page=2"
        ))
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["total"], 3);
    assert_eq!(body["page"], 1);
    assert_eq!(body["per_page"], 2);
    let page1 = body["messages"].as_array().unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0]["body"], "third");
    assert_eq!(page1[1]["body"], "second");

    let resp2 = app
        .get(&format!(
            "/api/v1/conversations/{conv_id}/messages?page=2&per_page=2"
        ))
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(resp2.status_code(), 200, "body={}", resp2.text());
    let body2: serde_json::Value = resp2.json();
    assert_eq!(body2["total"], 3);
    assert_eq!(body2["page"], 2);
    let page2 = body2["messages"].as_array().unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0]["body"], "first");
}

// ---------------------------------------------------------------------------
// POST /conversations/{id}/messages
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn send_message_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post(&format!(
            "/api/v1/conversations/{}/messages",
            Uuid::now_v7()
        ))
        .json(&json!({"body": "hi"}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn send_message_non_participant_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-send-403-coach@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "Send403 Coach").await;
    let member = app
        .register_member("msg-send-403-member@example.com", "Password!234")
        .await;
    let outsider = app
        .register_member("msg-send-403-outsider@example.com", "Password!234")
        .await;
    let conv_id = create_conversation(&app, &coach_token, member.user_id).await;

    let resp = app
        .post(&format!("/api/v1/conversations/{conv_id}/messages"))
        .authorization_bearer(&outsider.access_token)
        .json(&json!({"body": "hi"}))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn send_message_empty_body_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-send-422-coach@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "Send422 Coach").await;
    let member = app
        .register_member("msg-send-422-member@example.com", "Password!234")
        .await;
    let conv_id = create_conversation(&app, &coach_token, member.user_id).await;

    let resp = app
        .post(&format!("/api/v1/conversations/{conv_id}/messages"))
        .authorization_bearer(&coach_token)
        .json(&json!({"body": ""}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn send_message_too_long_body_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-send-toolong-coach@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "TooLong Coach").await;
    let member = app
        .register_member("msg-send-toolong-member@example.com", "Password!234")
        .await;
    let conv_id = create_conversation(&app, &coach_token, member.user_id).await;

    let too_long: String = std::iter::repeat_n('a', 2001).collect();
    let resp = app
        .post(&format!("/api/v1/conversations/{conv_id}/messages"))
        .authorization_bearer(&coach_token)
        .json(&json!({"body": too_long}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn send_message_updates_last_message_at_and_returns_message(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-send-touch-coach@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "Touch Coach").await;
    let member = app
        .register_member("msg-send-touch-member@example.com", "Password!234")
        .await;
    let conv_id = create_conversation(&app, &coach_token, member.user_id).await;

    let before: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT last_message_at FROM conversations WHERE id = $1")
            .bind(conv_id)
            .fetch_one(&app.db)
            .await
            .expect("fetch last_message_at");
    assert!(
        before.is_none(),
        "brand-new conversation has no last_message_at yet"
    );

    let resp = app
        .post(&format!("/api/v1/conversations/{conv_id}/messages"))
        .authorization_bearer(&member.access_token)
        .json(&json!({"body": "第一則訊息"}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["sender_id"], member.user_id.to_string());
    assert_eq!(body["body"], "第一則訊息");
    assert!(body["read_at"].is_null());
    assert!(body["id"].as_str().is_some());

    let after: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT last_message_at FROM conversations WHERE id = $1")
            .bind(conv_id)
            .fetch_one(&app.db)
            .await
            .expect("fetch last_message_at");
    assert!(
        after.is_some(),
        "last_message_at must be set after sending a message"
    );

    // Sending again must advance it further.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let resp2 = app
        .post(&format!("/api/v1/conversations/{conv_id}/messages"))
        .authorization_bearer(&coach_token)
        .json(&json!({"body": "回覆"}))
        .await;
    assert_eq!(resp2.status_code(), 200, "body={}", resp2.text());

    let after2: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT last_message_at FROM conversations WHERE id = $1")
            .bind(conv_id)
            .fetch_one(&app.db)
            .await
            .expect("fetch last_message_at");
    assert!(
        after2.expect("after2 set") > after.expect("after set"),
        "last_message_at must advance on each new message"
    );
}

// ---------------------------------------------------------------------------
// PATCH /conversations/{id}/read
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn mark_read_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .patch(&format!("/api/v1/conversations/{}/read", Uuid::now_v7()))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn mark_read_conversation_not_found_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("msg-read-404@example.com", "Password!234")
        .await;
    let resp = app
        .patch(&format!("/api/v1/conversations/{}/read", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn mark_read_non_participant_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-read-403-coach@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "Read403 Coach").await;
    let member = app
        .register_member("msg-read-403-member@example.com", "Password!234")
        .await;
    let outsider = app
        .register_member("msg-read-403-outsider@example.com", "Password!234")
        .await;
    let conv_id = create_conversation(&app, &coach_token, member.user_id).await;

    let resp = app
        .patch(&format!("/api/v1/conversations/{conv_id}/read"))
        .authorization_bearer(&outsider.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn mark_read_zeroes_unread_and_does_not_mark_own_messages(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) = app
        .seed_user_with_roles("msg-read-coach@example.com", &["coach"])
        .await;
    seed_coach(&app.db, coach_user_id, "Read Coach").await;
    let member = app
        .register_member("msg-read-member@example.com", "Password!234")
        .await;
    let conv_id = create_conversation(&app, &coach_token, member.user_id).await;

    let now = Utc::now();
    let msg1 = seed_message(
        &app.db,
        conv_id,
        member.user_id,
        "M1",
        None,
        now - Duration::seconds(3),
    )
    .await;
    let msg2 = seed_message(
        &app.db,
        conv_id,
        member.user_id,
        "M2",
        None,
        now - Duration::seconds(2),
    )
    .await;
    let msg3 = seed_message(
        &app.db,
        conv_id,
        coach_user_id,
        "C1",
        None,
        now - Duration::seconds(1),
    )
    .await;

    // Before any mark-read: coach sees 2 unread (M1, M2 sent by member);
    // member sees 1 unread (C1 sent by coach).
    let coach_view = app
        .get("/api/v1/conversations/me")
        .authorization_bearer(&coach_token)
        .await;
    let coach_body: serde_json::Value = coach_view.json();
    assert_eq!(coach_body[0]["unread_count"], 2);

    let member_view = app
        .get("/api/v1/conversations/me")
        .authorization_bearer(&member.access_token)
        .await;
    let member_body: serde_json::Value = member_view.json();
    assert_eq!(member_body[0]["unread_count"], 1);

    // Member marks the conversation as read — must ONLY mark the coach's
    // message (C1), never the member's own (M1/M2).
    let resp = app
        .patch(&format!("/api/v1/conversations/{conv_id}/read"))
        .authorization_bearer(&member.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["updated"], 1);

    let m1_read: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT read_at FROM messages WHERE id = $1")
            .bind(msg1)
            .fetch_one(&app.db)
            .await
            .unwrap();
    let m2_read: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT read_at FROM messages WHERE id = $1")
            .bind(msg2)
            .fetch_one(&app.db)
            .await
            .unwrap();
    let m3_read: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT read_at FROM messages WHERE id = $1")
            .bind(msg3)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(
        m1_read.is_none(),
        "member's own message M1 must not be marked read by the member's own mark-read call"
    );
    assert!(
        m2_read.is_none(),
        "member's own message M2 must not be marked read by the member's own mark-read call"
    );
    assert!(m3_read.is_some(), "coach's message C1 must be marked read");

    // Member's own unread_count is now zero...
    let member_view2 = app
        .get("/api/v1/conversations/me")
        .authorization_bearer(&member.access_token)
        .await;
    let member_body2: serde_json::Value = member_view2.json();
    assert_eq!(member_body2[0]["unread_count"], 0);

    // ...but the coach's unread_count is UNCHANGED (the member's mark-read
    // call must not affect the coach's own unread count for M1/M2).
    let coach_view2 = app
        .get("/api/v1/conversations/me")
        .authorization_bearer(&coach_token)
        .await;
    let coach_body2: serde_json::Value = coach_view2.json();
    assert_eq!(
        coach_body2[0]["unread_count"], 2,
        "coach's unread count must be untouched by the member's mark-read call"
    );

    // Coach now marks read — must mark M1/M2.
    let resp2 = app
        .patch(&format!("/api/v1/conversations/{conv_id}/read"))
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(resp2.status_code(), 200, "body={}", resp2.text());
    let body2: serde_json::Value = resp2.json();
    assert_eq!(body2["updated"], 2);

    let coach_view3 = app
        .get("/api/v1/conversations/me")
        .authorization_bearer(&coach_token)
        .await;
    let coach_body3: serde_json::Value = coach_view3.json();
    assert_eq!(coach_body3[0]["unread_count"], 0);
}
