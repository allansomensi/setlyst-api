-- Billing hardening before the paid launch (Brazil, BRL, CDC).
--
-- - Subscriptions keep what Stripe reports about the price actually paid
--   (finance report), when a failed renewal started (`past_due` grace),
--   the Subscription Terms accepted at checkout, the card-on-file trial
--   end and which billing reminders were already sent.
-- - Checkout Sessions are recorded so a new checkout expires the ones
--   still open (no second subscription from an old tab).
-- - Refunds get their own table (bucketed by their own date, failed ones
--   stop counting) and payments carry disputes and Stripe fees.
-- - Customers deleted at the provider can be recreated (idempotency key
--   generation), and a failed customer deletion is retried by a job.

-- ---------------------------------------------------------------------
-- Subscriptions
-- ---------------------------------------------------------------------

ALTER TABLE subscriptions
    -- When the paid subscription last moved to `past_due`: it keeps the
    -- plan for `PAYMENT_GRACE_DAYS` from here, then loses it.
    ADD COLUMN past_due_since TIMESTAMP,
    -- The provider's own status (`trialing`, `active`, `past_due`...).
    ADD COLUMN provider_status VARCHAR(30),
    -- Price of the subscription item as Stripe charges it (minor units),
    -- for recurring revenue at the price each subscriber really pays.
    ADD COLUMN unit_amount_cents BIGINT CHECK (unit_amount_cents IS NULL OR unit_amount_cents >= 0),
    ADD COLUMN currency VARCHAR(3),
    -- "Your first charge is in 7 days" (card-on-file trial), sent once.
    ADD COLUMN paid_trial_reminder_sent_at TIMESTAMP,
    -- The period whose yearly renewal reminder was sent (once per period).
    ADD COLUMN renewal_reminder_period_end TIMESTAMP,
    -- Subscription Terms accepted on the provider's checkout page.
    ADD COLUMN terms_version VARCHAR(40),
    ADD COLUMN terms_accepted_at TIMESTAMP;

CREATE INDEX idx_subscriptions_payment_live ON subscriptions (source, status)
    WHERE source = 'payment';

-- ---------------------------------------------------------------------
-- Checkout Sessions
-- ---------------------------------------------------------------------

CREATE TABLE checkout_sessions (
    session_id VARCHAR(255) PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL,
    -- Expired by the API (a newer checkout was started) or by Stripe.
    expired_at TIMESTAMP,
    completed_at TIMESTAMP
);

CREATE INDEX idx_checkout_sessions_open ON checkout_sessions (user_id, created_at)
    WHERE expired_at IS NULL AND completed_at IS NULL;

-- ---------------------------------------------------------------------
-- Refunds, disputes and fees
-- ---------------------------------------------------------------------

-- Every refund Stripe reports (webhook `refund.*`, `charge.refund.updated`,
-- the refunds of a `charge.refunded` charge, and the finance sync).
-- `payments.refunded_cents` is derived from the rows that still count
-- (`succeeded` or `pending`).
CREATE TABLE refunds (
    id VARCHAR(255) PRIMARY KEY,
    payment_intent_id VARCHAR(255),
    charge_id VARCHAR(255),
    amount_cents BIGINT NOT NULL CHECK (amount_cents >= 0),
    currency VARCHAR(3),
    status VARCHAR(30) NOT NULL,
    -- When Stripe created the refund: the report counts it in that month.
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_refunds_payment_intent ON refunds (payment_intent_id)
    WHERE payment_intent_id IS NOT NULL;
CREATE INDEX idx_refunds_created ON refunds (created_at);

ALTER TABLE payments
    ADD COLUMN charge_id VARCHAR(255),
    -- Amount taken back by a card dispute (chargeback). Kept when the
    -- dispute is lost, cleared when it is won.
    ADD COLUMN disputed_cents BIGINT NOT NULL DEFAULT 0 CHECK (disputed_cents >= 0),
    ADD COLUMN dispute_status VARCHAR(40),
    -- Stripe's fee and the net amount of the charge (balance transaction),
    -- when known.
    ADD COLUMN fee_cents BIGINT,
    ADD COLUMN net_cents BIGINT;

CREATE INDEX idx_payments_subscription ON payments (subscription_id)
    WHERE subscription_id IS NOT NULL;

-- ---------------------------------------------------------------------
-- Customers
-- ---------------------------------------------------------------------

-- Part of the customer-creation idempotency key: bumped when the stored
-- customer turns out to be deleted at Stripe, so a new one is created
-- instead of replaying the dead one for 24 hours.
ALTER TABLE users ADD COLUMN stripe_customer_generation INTEGER NOT NULL DEFAULT 0;

-- Provider customers whose deletion failed (account deletion, LGPD):
-- retried hourly until Stripe confirms.
CREATE TABLE stripe_cleanup_queue (
    customer_id VARCHAR(255) PRIMARY KEY,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error VARCHAR(500),
    next_attempt_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL
);

-- ---------------------------------------------------------------------
-- Referrals: the referred account's own bonus is paid on e-mail
-- verification; the referrer's on that account's first paid invoice.
-- ---------------------------------------------------------------------

ALTER TABLE referrals ADD COLUMN referred_rewarded_at TIMESTAMP;

-- ---------------------------------------------------------------------
-- Periodic billing jobs that must run at most once per interval across
-- replicas (the daily Stripe reconciliation).
-- ---------------------------------------------------------------------

CREATE TABLE billing_job_runs (
    name VARCHAR(60) PRIMARY KEY,
    last_run_at TIMESTAMP NOT NULL
);
