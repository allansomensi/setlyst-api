-- Card payments through Stripe.
--
-- Stripe is the source of truth for paid subscriptions: the webhook
-- (`POST /webhooks/stripe`) mirrors each Stripe subscription into
-- `subscriptions` with `source = 'payment'` and the Stripe subscription id
-- in `external_ref`.

-- The Stripe customer of an account, created on its first checkout.
ALTER TABLE users ADD COLUMN stripe_customer_id VARCHAR(255);
CREATE UNIQUE INDEX idx_users_stripe_customer ON users (stripe_customer_id)
    WHERE stripe_customer_id IS NOT NULL;

CREATE INDEX idx_subscriptions_external_ref ON subscriptions (external_ref)
    WHERE external_ref IS NOT NULL;

-- How often a paid subscription is charged ('monthly' or 'yearly').
ALTER TABLE subscriptions ADD COLUMN billing_interval VARCHAR(10)
    CHECK (billing_interval IS NULL OR billing_interval IN ('monthly', 'yearly'));

-- Webhook events already applied, so a redelivered event is a no-op.
-- Pruned after 30 days by the maintenance job (Stripe stops retrying an
-- event after three days).
CREATE TABLE stripe_events (
    id VARCHAR(255) PRIMARY KEY,
    type VARCHAR(100) NOT NULL,
    received_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_stripe_events_received ON stripe_events (received_at);

-- A redeemed `discount` promo code is spent on the first payment that
-- uses it.
ALTER TABLE promo_redemptions ADD COLUMN applied_at TIMESTAMP;
