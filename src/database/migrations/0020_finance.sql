-- Payments received, for the staff finance report.
--
-- Stripe stays the source of truth: every paid invoice is copied here by
-- the webhook (`invoice.paid`) or by the staff "sync with Stripe" action,
-- and refunds by `charge.refunded`. Rows are keyed by the invoice id, so
-- the same invoice arriving twice is a no-op.
--
-- `user_id` is cleared, not cascaded, when an account is deleted: payment
-- records must be kept for tax and accounting purposes (LGPD art. 16, I),
-- and they carry no personal data of their own.

CREATE TABLE payments (
    id UUID PRIMARY KEY,
    provider VARCHAR(20) NOT NULL DEFAULT 'stripe',
    invoice_id VARCHAR(255) NOT NULL,
    payment_intent_id VARCHAR(255),
    customer_id VARCHAR(255),
    subscription_id VARCHAR(255),
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    plan_code VARCHAR(32),
    billing_interval VARCHAR(10)
        CHECK (billing_interval IS NULL OR billing_interval IN ('monthly', 'yearly')),
    -- Minor units (centavos), as charged: after discounts, before fees.
    amount_cents BIGINT NOT NULL CHECK (amount_cents >= 0),
    refunded_cents BIGINT NOT NULL DEFAULT 0 CHECK (refunded_cents >= 0),
    currency VARCHAR(3) NOT NULL,
    paid_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    CONSTRAINT payments_refund_not_above_amount CHECK (refunded_cents <= amount_cents)
);

CREATE UNIQUE INDEX idx_payments_provider_invoice ON payments (provider, invoice_id);
CREATE INDEX idx_payments_paid_at ON payments (paid_at DESC);
CREATE INDEX idx_payments_user ON payments (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX idx_payments_payment_intent ON payments (payment_intent_id)
    WHERE payment_intent_id IS NOT NULL;

-- Finance-report queries over subscription changes.
CREATE INDEX idx_subscription_events_kind_created ON subscription_events (kind, created_at);
