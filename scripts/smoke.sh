#!/usr/bin/env bash
# End-to-end smoke test for the Dream Fly backend API.
#
# Prerequisites (see README.md "Seed 與 Smoke Test"):
#   1. Infra up:        docker-compose up -d
#   2. Server running:  cargo run          (in another terminal / background)
#   3. Seed data ready:  cargo run --bin seed
#
# Usage:
#   scripts/smoke.sh [BASE_URL]
#   BASE_URL defaults to http://localhost:3000/api/v1
#
# Exercises: health -> register -> login -> add a seeded course + the
# seeded 月票 (monthly-pass) product to cart -> validate a coupon ->
# checkout (with a payment_method) with Idempotency-Key -> verify
# enrolments/subscriptions/points reflect the order -> replay the same
# Idempotency-Key and confirm no duplicate order is created.
#
# Round 3/4 coverage (Task P5-B): also exercises the attendance/leave/
# messages/certificates/rewards/reports/schedule-me happy paths, plus this
# round's new endpoints — PATCH venues, POST+PATCH coaches, PATCH+DELETE
# coupons, GET enrolments/{id}/attendance, POST /contact (trial) + PATCH
# inquiries, GET/PUT settings, profile preferences+birth_date, GET
# /sessions/today, GET /reports/admin/activity, and the Round 4 Phase 4
# sections of GET /reports/admin.
#
# Every step prints an explicit PASS/FAIL line. Any failure exits 1
# immediately (set -e plus explicit `exit 1` in `fail`).

set -euo pipefail

BASE_URL="${1:-http://localhost:3000/api/v1}"

for bin in curl jq; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "FAIL: prerequisite '$bin' is not installed"
    exit 1
  fi
done

TMP_BODY="$(mktemp)"
trap 'rm -f "$TMP_BODY"' EXIT

STEP=0
pass() {
  STEP=$((STEP + 1))
  echo "PASS [$STEP] $1"
}
fail() {
  STEP=$((STEP + 1))
  echo "FAIL [$STEP] $1"
  echo "      body: $(cat "$TMP_BODY" 2>/dev/null || true)"
  exit 1
}

# Issue an HTTP request; writes the response body to $TMP_BODY and echoes the
# HTTP status code. Usage: req METHOD PATH [JSON_BODY] [BEARER_TOKEN] [EXTRA_HEADER]
req() {
  local method="$1" path="$2" data="${3:-}" token="${4:-}" extra_header="${5:-}"
  local args=(-s -o "$TMP_BODY" -w '%{http_code}' -X "$method" "$BASE_URL$path" -H "Content-Type: application/json")
  [[ -n "$token" ]] && args+=(-H "Authorization: Bearer $token")
  [[ -n "$extra_header" ]] && args+=(-H "$extra_header")
  [[ -n "$data" ]] && args+=(-d "$data")
  curl "${args[@]}"
}

expect_status() {
  local desc="$1" expected="$2" actual="$3"
  if [[ "$actual" == "$expected" ]]; then
    pass "$desc (HTTP $actual)"
  else
    fail "$desc (expected HTTP $expected, got $actual)"
  fi
}

expect_truthy() {
  local desc="$1" jq_filter="$2"
  local result
  result="$(jq -r "$jq_filter" "$TMP_BODY" 2>/dev/null || echo "null")"
  if [[ "$result" == "true" ]]; then
    pass "$desc"
  else
    fail "$desc (jq '$jq_filter' -> '$result')"
  fi
}

echo "== Dream Fly smoke test against $BASE_URL =="

# ---------------------------------------------------------------------------
# 1. Health
# ---------------------------------------------------------------------------
status="$(req GET /health)"
expect_status "GET /health reachable" "200" "$status"
expect_truthy "health status is healthy" '.status == "healthy"'

# ---------------------------------------------------------------------------
# 2. Register a random member
# ---------------------------------------------------------------------------
RAND_SUFFIX="$(date +%s)$RANDOM"
EMAIL="smoke-${RAND_SUFFIX}@example.com"
PASSWORD="Smoke#2026"
register_body="$(jq -n --arg email "$EMAIL" --arg name "Smoke Test" --arg password "$PASSWORD" \
  '{email: $email, name: $name, password: $password}')"

status="$(req POST /auth/register "$register_body")"
expect_status "POST /auth/register ($EMAIL)" "200" "$status"
expect_truthy "register response has access_token" '(.access_token // "") != ""'

# ---------------------------------------------------------------------------
# 3. Login with the same credentials
# ---------------------------------------------------------------------------
login_body="$(jq -n --arg email "$EMAIL" --arg password "$PASSWORD" '{email: $email, password: $password}')"
status="$(req POST /auth/login "$login_body")"
expect_status "POST /auth/login" "200" "$status"
ACCESS_TOKEN="$(jq -r '.access_token' "$TMP_BODY")"
if [[ -z "$ACCESS_TOKEN" || "$ACCESS_TOKEN" == "null" ]]; then
  fail "login did not return access_token"
else
  pass "login returned access_token"
fi

# ---------------------------------------------------------------------------
# 4. Pick a seeded course
# ---------------------------------------------------------------------------
status="$(req GET "/courses?page=1&per_page=20" "" "$ACCESS_TOKEN")"
expect_status "GET /courses" "200" "$status"
COURSE_COUNT="$(jq '.courses | length' "$TMP_BODY")"
if [[ "$COURSE_COUNT" -lt 1 ]]; then
  fail "no courses found — did you run 'cargo run --bin seed'?"
else
  pass "GET /courses returned $COURSE_COUNT course(s)"
fi
COURSE_ID="$(jq -r '.courses[0].id' "$TMP_BODY")"
COURSE_NAME="$(jq -r '.courses[0].name' "$TMP_BODY")"
echo "      using course: $COURSE_NAME ($COURSE_ID)"

# ---------------------------------------------------------------------------
# 5. Add the course to cart
# ---------------------------------------------------------------------------
add_course_body="$(jq -n --arg id "$COURSE_ID" '{item_type: "course", item_id: $id}')"
status="$(req POST /cart/items "$add_course_body" "$ACCESS_TOKEN")"
expect_status "POST /cart/items (course)" "200" "$status"
result="$(jq -r --arg cid "$COURSE_ID" '[.items[] | select(.item_type=="course" and .item_id==$cid)] | length > 0' "$TMP_BODY")"
if [[ "$result" == "true" ]]; then
  pass "cart contains the added course"
else
  fail "cart does not contain the added course"
fi

# ---------------------------------------------------------------------------
# 6. Pick the seeded 月票 (monthly-pass) product
# ---------------------------------------------------------------------------
status="$(req GET "/products?product_type=membership&per_page=50" "" "$ACCESS_TOKEN")"
expect_status "GET /products?product_type=membership" "200" "$status"
PRODUCT_ID="$(jq -r '[.products[] | select(.name == "月票")][0].id // empty' "$TMP_BODY")"
if [[ -z "$PRODUCT_ID" ]]; then
  fail "could not find seeded product '月票' (monthly-pass) — did you run 'cargo run --bin seed'?"
else
  pass "found 月票 product ($PRODUCT_ID)"
fi

# ---------------------------------------------------------------------------
# 7. Add the plan to cart
# ---------------------------------------------------------------------------
add_product_body="$(jq -n --arg id "$PRODUCT_ID" '{item_type: "product", item_id: $id}')"
status="$(req POST /cart/items "$add_product_body" "$ACCESS_TOKEN")"
expect_status "POST /cart/items (月票)" "200" "$status"
result="$(jq -r --arg pid "$PRODUCT_ID" '[.items[] | select(.item_type=="product" and .item_id==$pid)] | length > 0' "$TMP_BODY")"
if [[ "$result" == "true" ]]; then
  pass "cart contains the added plan"
else
  fail "cart does not contain the added plan"
fi

# ---------------------------------------------------------------------------
# 8. Validate the WELCOME50 coupon
# ---------------------------------------------------------------------------
status="$(req GET /coupons/WELCOME50/validate "" "$ACCESS_TOKEN")"
expect_status "GET /coupons/WELCOME50/validate" "200" "$status"
expect_truthy "coupon discount_cents == 5000" '.discount_cents == 5000'

# ---------------------------------------------------------------------------
# 9. Checkout with the coupon + Idempotency-Key
# ---------------------------------------------------------------------------
IDEMPOTENCY_KEY="smoke-${RAND_SUFFIX}"
checkout_body='{"coupon_code": "WELCOME50", "use_points": false, "payment_method": "line_pay"}'
status="$(req POST /orders "$checkout_body" "$ACCESS_TOKEN" "Idempotency-Key: $IDEMPOTENCY_KEY")"
expect_status "POST /orders (checkout)" "200" "$status"

ORDER_ID="$(jq -r '.id' "$TMP_BODY")"
ORDER_NUMBER="$(jq -r '.order_number' "$TMP_BODY")"
expect_truthy "order status is paid" '.status == "paid"'
expect_truthy "response has enrolments[0]" '(.enrolments // []) | length > 0'
expect_truthy "response has subscriptions[0]" '(.subscriptions // []) | length > 0'
expect_truthy "order payment_method reflects the request (line_pay)" '.payment_method == "line_pay"'
echo "      order: $ORDER_NUMBER ($ORDER_ID)"

# ---------------------------------------------------------------------------
# 10. Cross-check /enrolments/me, /subscriptions/me, /points/me
# ---------------------------------------------------------------------------
status="$(req GET /enrolments/me "" "$ACCESS_TOKEN")"
expect_status "GET /enrolments/me" "200" "$status"
result="$(jq -r --arg cid "$COURSE_ID" '[.[] | select(.course_id == $cid and .status == "active")] | length > 0' "$TMP_BODY")"
if [[ "$result" == "true" ]]; then
  pass "/enrolments/me includes the purchased course"
else
  fail "/enrolments/me does not include the purchased course"
fi
ENROLMENT_ID="$(jq -r --arg cid "$COURSE_ID" '[.[] | select(.course_id == $cid and .status == "active")][0].id' "$TMP_BODY")"

status="$(req GET /subscriptions/me "" "$ACCESS_TOKEN")"
expect_status "GET /subscriptions/me" "200" "$status"
result="$(jq -r --arg pid "$PRODUCT_ID" '[.[] | select(.product_id == $pid and .status == "active")] | length > 0' "$TMP_BODY")"
if [[ "$result" == "true" ]]; then
  pass "/subscriptions/me includes the purchased plan"
else
  fail "/subscriptions/me does not include the purchased plan"
fi

status="$(req GET /points/me "" "$ACCESS_TOKEN")"
expect_status "GET /points/me" "200" "$status"
result="$(jq -r --arg oid "$ORDER_ID" '[.ledger[] | select(.order_id == $oid and .reason == "checkout_earn")] | length > 0' "$TMP_BODY")"
if [[ "$result" == "true" ]]; then
  pass "/points/me ledger reflects the order's earned points"
else
  fail "/points/me ledger does not reflect the order's earned points"
fi

# ---------------------------------------------------------------------------
# 11. Replay the same Idempotency-Key — must return the same order
# ---------------------------------------------------------------------------
status="$(req POST /orders '{}' "$ACCESS_TOKEN" "Idempotency-Key: $IDEMPOTENCY_KEY")"
expect_status "POST /orders (idempotent replay)" "200" "$status"
REPLAY_ORDER_NUMBER="$(jq -r '.order_number' "$TMP_BODY")"
if [[ "$REPLAY_ORDER_NUMBER" == "$ORDER_NUMBER" ]]; then
  pass "replay returned the same order_number ($REPLAY_ORDER_NUMBER)"
else
  fail "replay returned a different order_number (expected $ORDER_NUMBER, got $REPLAY_ORDER_NUMBER)"
fi

# ---------------------------------------------------------------------------
# 12. Round 3/4 coverage — log in as the seeded admin and a seeded coach
# ---------------------------------------------------------------------------
admin_login_body='{"email": "admin@dreamfly.tw", "password": "Admin#2026"}'
status="$(req POST /auth/login "$admin_login_body")"
expect_status "POST /auth/login (seeded admin)" "200" "$status"
ADMIN_TOKEN="$(jq -r '.access_token' "$TMP_BODY")"
if [[ -z "$ADMIN_TOKEN" || "$ADMIN_TOKEN" == "null" ]]; then
  fail "admin login did not return access_token"
else
  pass "admin login returned access_token"
fi

coach_login_body='{"email": "coach1@dreamfly.tw", "password": "Coach#2026"}'
status="$(req POST /auth/login "$coach_login_body")"
expect_status "POST /auth/login (seeded coach)" "200" "$status"
COACH_TOKEN="$(jq -r '.access_token' "$TMP_BODY")"
if [[ -z "$COACH_TOKEN" || "$COACH_TOKEN" == "null" ]]; then
  fail "coach login did not return access_token"
else
  pass "coach login returned access_token"
fi

# ---------------------------------------------------------------------------
# 13. Round 3 module happy paths — attendance/leave/messages/certificates/
#     rewards/reports/schedule-me. One or two representative calls each;
#     the full role-permission matrix is already covered by `cargo test`, so
#     this just proves each module's wiring against a real running server
#     talking to a real database.
# ---------------------------------------------------------------------------
status="$(req GET /coaches/me/students "" "$COACH_TOKEN")"
expect_status "GET /coaches/me/students (attendance)" "200" "$status"

status="$(req GET /leave-requests/me "" "$ACCESS_TOKEN")"
expect_status "GET /leave-requests/me (leave)" "200" "$status"
status="$(req GET /leave-requests "" "$ADMIN_TOKEN")"
expect_status "GET /leave-requests (leave, admin)" "200" "$status"

status="$(req GET /conversations/me "" "$ACCESS_TOKEN")"
expect_status "GET /conversations/me (messages)" "200" "$status"

status="$(req GET /certificates/me "" "$ACCESS_TOKEN")"
expect_status "GET /certificates/me (certificates)" "200" "$status"
status="$(req GET /report-cards/me "" "$ACCESS_TOKEN")"
expect_status "GET /report-cards/me (certificates)" "200" "$status"

status="$(req GET /rewards "" "$ACCESS_TOKEN")"
expect_status "GET /rewards (rewards)" "200" "$status"
status="$(req GET /rewards/redemptions/me "" "$ACCESS_TOKEN")"
expect_status "GET /rewards/redemptions/me (rewards)" "200" "$status"

status="$(req GET /reports/me "" "$ACCESS_TOKEN")"
expect_status "GET /reports/me (reports, member)" "200" "$status"

status="$(req GET /schedule/me "" "$ACCESS_TOKEN")"
expect_status "GET /schedule/me (schedule-me)" "200" "$status"

# ---------------------------------------------------------------------------
# 14. PATCH /venues/{id} — admin
# ---------------------------------------------------------------------------
status="$(req GET /venues)"
expect_status "GET /venues" "200" "$status"
VENUE_ID="$(jq -r '.[0].id // empty' "$TMP_BODY")"
ORIGINAL_VENUE_DESCRIPTION="$(jq -r '.[0].description' "$TMP_BODY")"
if [[ -z "$VENUE_ID" ]]; then
  fail "no venues found — did you run 'cargo run --bin seed'?"
else
  pass "found a seeded venue ($VENUE_ID)"
fi

patch_venue_body='{"description": "smoke test 更新的場館描述"}'
status="$(req PATCH "/venues/$VENUE_ID" "$patch_venue_body" "$ADMIN_TOKEN")"
expect_status "PATCH /venues/{id}" "200" "$status"
expect_truthy "venue description reflects the PATCH" '.description == "smoke test 更新的場館描述"'

# Restore the seed venue's original description — it's shown on the public
# site, so this PATCH must not leave it permanently overwritten.
restore_venue_body="$(jq -n --arg d "$ORIGINAL_VENUE_DESCRIPTION" '{description: $d}')"
status="$(req PATCH "/venues/$VENUE_ID" "$restore_venue_body" "$ADMIN_TOKEN")"
expect_status "PATCH /venues/{id} (restore original description)" "200" "$status"

# ---------------------------------------------------------------------------
# 15. POST /coaches + PATCH /coaches/{id} — admin (fresh throwaway user, so
#     this never touches the seeded coach1..coach4 accounts)
# ---------------------------------------------------------------------------
new_coach_email="smoke-coach-${RAND_SUFFIX}@example.com"
create_user_body="$(jq -n --arg email "$new_coach_email" --arg password "$PASSWORD" \
  '{email: $email, name: "Smoke Coach", password: $password}')"
status="$(req POST /users "$create_user_body" "$ADMIN_TOKEN")"
expect_status "POST /users (new coach's user account)" "200" "$status"
NEW_COACH_USER_ID="$(jq -r '.id' "$TMP_BODY")"

create_coach_body="$(jq -n --arg uid "$NEW_COACH_USER_ID" '{user_id: $uid, title: "Smoke Test 教練"}')"
status="$(req POST /coaches "$create_coach_body" "$ADMIN_TOKEN")"
expect_status "POST /coaches" "200" "$status"
NEW_COACH_ID="$(jq -r '.id' "$TMP_BODY")"

patch_coach_body='{"title": "Smoke Test 資深教練"}'
status="$(req PATCH "/coaches/$NEW_COACH_ID" "$patch_coach_body" "$ADMIN_TOKEN")"
expect_status "PATCH /coaches/{id}" "200" "$status"
expect_truthy "coach title reflects the PATCH" '.title == "Smoke Test 資深教練"'

# ---------------------------------------------------------------------------
# 16. PATCH + DELETE /coupons/{id} — admin, on a throwaway coupon created
#     just for this check (never WELCOME50, used earlier for checkout)
# ---------------------------------------------------------------------------
smoke_coupon_code="SMOKE${RAND_SUFFIX}"
create_coupon_body="$(jq -n --arg code "$smoke_coupon_code" '{code: $code, discount_cents: 100}')"
status="$(req POST /coupons "$create_coupon_body" "$ADMIN_TOKEN")"
expect_status "POST /coupons (throwaway)" "200" "$status"
SMOKE_COUPON_ID="$(jq -r '.id' "$TMP_BODY")"

patch_coupon_body='{"discount_cents": 200}'
status="$(req PATCH "/coupons/$SMOKE_COUPON_ID" "$patch_coupon_body" "$ADMIN_TOKEN")"
expect_status "PATCH /coupons/{id}" "200" "$status"
expect_truthy "coupon discount_cents reflects the PATCH" '.discount_cents == 200'

status="$(req DELETE "/coupons/$SMOKE_COUPON_ID" "" "$ADMIN_TOKEN")"
expect_status "DELETE /coupons/{id}" "204" "$status"

# ---------------------------------------------------------------------------
# 17. GET /enrolments/{id}/attendance — owner (this script's own member,
#     using the enrolment id resolved back in step 10)
# ---------------------------------------------------------------------------
if [[ -z "$ENROLMENT_ID" || "$ENROLMENT_ID" == "null" ]]; then
  fail "could not resolve this script's own enrolment id (step 10)"
else
  status="$(req GET "/enrolments/$ENROLMENT_ID/attendance" "" "$ACCESS_TOKEN")"
  expect_status "GET /enrolments/{id}/attendance" "200" "$status"
fi

# ---------------------------------------------------------------------------
# 18. POST /contact (trial inquiry, public) + PATCH /contact/inquiries/{id}
#     (admin follow-up)
# ---------------------------------------------------------------------------
trial_email="smoke-trial-${RAND_SUFFIX}@example.com"
create_inquiry_body="$(jq -n --arg email "$trial_email" \
  '{name: "Smoke 家長", email: $email, phone: "0912345678", subject: "試上預約",
    message: "想幫小孩預約一堂體驗課", inquiry_type: "trial",
    metadata: {student_age: 8, preferred_day: "六"}}')"
status="$(req POST /contact "$create_inquiry_body")"
expect_status "POST /contact (trial inquiry)" "200" "$status"
expect_truthy "inquiry inquiry_type is trial" '.inquiry_type == "trial"'
INQUIRY_ID="$(jq -r '.id' "$TMP_BODY")"

patch_inquiry_body='{"status": "in_progress"}'
status="$(req PATCH "/contact/inquiries/$INQUIRY_ID" "$patch_inquiry_body" "$ADMIN_TOKEN")"
expect_status "PATCH /contact/inquiries/{id}" "200" "$status"
expect_truthy "inquiry status reflects the PATCH" '.status == "in_progress"'

# ---------------------------------------------------------------------------
# 19. GET + PUT /settings — admin
# ---------------------------------------------------------------------------
status="$(req GET /settings "" "$ADMIN_TOKEN")"
expect_status "GET /settings" "200" "$status"

put_settings_body='{"settings": {"smoke_test_key": "smoke-value"}}'
status="$(req PUT /settings "$put_settings_body" "$ADMIN_TOKEN")"
expect_status "PUT /settings" "200" "$status"
expect_truthy "settings roundtrip the upserted key" '.settings.smoke_test_key == "smoke-value"'

# ---------------------------------------------------------------------------
# 20. PATCH /users/me — profile preferences + birth_date
# ---------------------------------------------------------------------------
patch_profile_body='{"preferences": {"dark": true, "class_reminder": true}, "birth_date": "1995-06-15"}'
status="$(req PATCH /users/me "$patch_profile_body" "$ACCESS_TOKEN")"
expect_status "PATCH /users/me (preferences + birth_date)" "200" "$status"
expect_truthy "profile preferences.dark reflects the PATCH" '.preferences.dark == true'
expect_truthy "profile birth_date reflects the PATCH" '.birth_date == "1995-06-15"'

# ---------------------------------------------------------------------------
# 21. GET /sessions/today — admin
# ---------------------------------------------------------------------------
status="$(req GET /sessions/today "" "$ADMIN_TOKEN")"
expect_status "GET /sessions/today (admin)" "200" "$status"

# ---------------------------------------------------------------------------
# 22. GET /reports/admin/activity — admin
# ---------------------------------------------------------------------------
status="$(req GET /reports/admin/activity "" "$ADMIN_TOKEN")"
expect_status "GET /reports/admin/activity" "200" "$status"
expect_truthy "activity response has items[]" '(.items // null) != null'

# ---------------------------------------------------------------------------
# 23. GET /reports/admin — assert the Round 4 Phase 4 sections are present,
#     non-null, and (for the fixed-bucket sections) the documented length
#     regardless of how much data is behind them
# ---------------------------------------------------------------------------
status="$(req GET /reports/admin "" "$ADMIN_TOKEN")"
expect_status "GET /reports/admin" "200" "$status"
expect_truthy "admin report has all Round 4 sections, none null" '[.kpis, .revenue_breakdown, .income_sources_12m, .category_split, .payment_split, .attendance_distribution, .age_distribution, .tier_distribution, .retention, .funnel, .weekday_load, .venue_usage] | all(. != null)'
expect_truthy "revenue_breakdown has the fixed 6 sources" '(.revenue_breakdown | length) == 6'
expect_truthy "income_sources_12m has 12 months x 6 sources = 72 rows" '(.income_sources_12m | length) == 72'
expect_truthy "category_split has the fixed 5 sources" '(.category_split | length) == 5'
expect_truthy "attendance_distribution has the fixed 4 buckets" '(.attendance_distribution | length) == 4'
expect_truthy "age_distribution has the fixed 6 buckets" '(.age_distribution | length) == 6'
expect_truthy "tier_distribution has the fixed 4 buckets" '(.tier_distribution | length) == 4'
expect_truthy "retention has the fixed 6 months" '(.retention | length) == 6'
expect_truthy "weekday_load has the fixed 7 buckets" '(.weekday_load | length) == 7'

echo ""
echo "All $STEP checks passed."
exit 0
