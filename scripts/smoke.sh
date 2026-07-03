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
# checkout with Idempotency-Key -> verify enrolments/subscriptions/points
# reflect the order -> replay the same Idempotency-Key and confirm no
# duplicate order is created.
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
checkout_body='{"coupon_code": "WELCOME50", "use_points": false}'
status="$(req POST /orders "$checkout_body" "$ACCESS_TOKEN" "Idempotency-Key: $IDEMPOTENCY_KEY")"
expect_status "POST /orders (checkout)" "200" "$status"

ORDER_ID="$(jq -r '.id' "$TMP_BODY")"
ORDER_NUMBER="$(jq -r '.order_number' "$TMP_BODY")"
expect_truthy "order status is paid" '.status == "paid"'
expect_truthy "response has enrolments[0]" '(.enrolments // []) | length > 0'
expect_truthy "response has subscriptions[0]" '(.subscriptions // []) | length > 0'
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

echo ""
echo "All $STEP checks passed."
exit 0
