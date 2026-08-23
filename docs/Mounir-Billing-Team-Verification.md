# Mounir billing + team requirements — verification checklist

Use this before replying that the work is complete. All items should be ✅.

## 1. No hard-wired plan / customer cannot set plan via API

| Check | How to verify |
|-------|----------------|
| No `PROKURO_DEFAULT_PLAN` in code or CDK | `rg PROKURO_DEFAULT_PLAN` → docs only (README shows admin curl, not env var) |
| Plan from Dynamo: Stripe **or** admin override | `GET /v1/billing/status` → `plan_source`: `stripe` \| `admin` \| `free` |
| Pilot onboarding without customer API | `POST /v1/billing/admin/plan` + `X-Prokuro-Admin-Secret` |
| Stripe webhook does **not** default unknown price to Growth | Unit test `active_stripe_subscription_without_price_does_not_default_to_growth` |

```bash
curl -sS -X POST "$GATEWAY/v1/billing/admin/plan" \
  -H "X-Prokuro-Admin-Secret: $PROKURO_ADMIN_SECRET" \
  -H 'Content-Type: application/json' \
  -d '{"account_id":"<cognito-sub>","plan":"growth","note":"pilot"}'
```

## 2. Stats / caps accurate

| Check | How to verify |
|-------|----------------|
| `active_boms_count` from live BOM list | `/v1/billing/status` → `usage.active_boms_count` matches BOM count |
| BOM create enforces caps + increments usage | `ensure_bom_create` on `POST /v1/boms` |
| BOM update enforces `max_lines_per_bom` | `ensure_bom_update` on `PUT /v1/boms/{id}` |
| Caps enforced when `BILLING_TABLE` set (prod) | `caps_enforced()` → Dynamo store |
| `current_period_end` readable in UI | Unix epoch normalized to ISO in API |

```bash
cargo test -p prokuro-gateway --lib
# 45+ tests including billing + team
```

## 3. Team invite — SNS email (same pattern as BOM alarms)

| Check | How to verify |
|-------|----------------|
| CDK: SNS topic → Lambda → SES | `lib/constructs/team-invite-email.ts` |
| Gateway publishes to `TEAM_INVITE_SNS_TOPIC_ARN` | ECS env on gateway task |
| Response includes `email_delivery` | `queued` \| `sent` \| `failed` \| `not_configured` |
| Copy-link always returned | `accept_url` in create invite response |

```bash
aws sns list-subscriptions-by-topic --topic-arn arn:aws:sns:us-west-2:713463138528:prokuro-team-invite-email
aws lambda get-function-configuration --function-name prokuro-team-invite-email
```

## 4. Team UI readjustment (Account page)

| Check | How to verify |
|-------|----------------|
| Members listed first, invite form last | `/account` Team section |
| Invite hidden on Free / full seats | `canInvite` from `use-team.ts` |
| 402 shows human message | `PlanCapError` in `lib/api.ts` |
| Admin plan badge | "Admin-assigned plan" when `plan_source === 'admin'` |
| Email delivery messaging | queued / failed / copy-link fallback |

```bash
cd prokuroWeb && npm run build
```

## 5. Live smoke (production)

1. Account shows Growth + admin-assigned after admin curl
2. Invite creates pending row + `accept_url`
3. Email delivers when SES identities verified (or copy-link works)
4. Accept requires signed-in email matching invite

---

**Reply template for Mounir:**

> Billing is wired to Dynamo (Stripe or admin override) — no hard-coded Growth. Stats use live BOM counts + enforced caps on create/update. Pilot onboarding is admin API only. Team invites use the SNS→Lambda→SES path (same pattern as BOM alarms). Account team UI reworked: members first, invite gated by plan/seats, honest email delivery status. Tests: `cargo test -p prokuro-gateway --lib` (45 pass). Remaining ops: SES prod quota + DKIM for noreply@.
