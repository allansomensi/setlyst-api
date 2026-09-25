-- Payments received and their refunds, for the staff finance report.
--
-- Stripe stays the source of truth: every paid invoice is copied here by
-- the webhook (`invoice.paid`) or by the staff "sync with Stripe" action,
-- and refunds, disputes and fees as Stripe reports them. Rows are keyed by
-- the provider's ids, so the same event arriving twice is a no-op.

-- `user_id` is cleared, not cascaded, when an account is deleted: payment
-- records must be kept for tax and accounting purposes (LGPD art. 16, I),
-- and they carry no personal data of their own.
CREATE TABLE payments (
    id UUID PRIMARY KEY,
    provider VARCHAR(20) NOT NULL DEFAULT 'stripe',
    invoice_id VARCHAR(255) NOT NULL,
    payment_intent_id VARCHAR(255),
    charge_id VARCHAR(255),
    customer_id VARCHAR(255),
    subscription_id VARCHAR(255),
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    plan_code VARCHAR(32),
    billing_interval VARCHAR(10)
        CHECK (billing_interval IS NULL OR billing_interval IN ('monthly', 'yearly')),
    -- Minor units (centavos), as charged: after discounts, before fees.
    amount_cents BIGINT NOT NULL CHECK (amount_cents >= 0),
    -- Derived from the `refunds` rows that still count (`succeeded` or
    -- `pending`).
    refunded_cents BIGINT NOT NULL DEFAULT 0 CHECK (refunded_cents >= 0),
    -- Amount taken back by a card dispute (chargeback). Kept when the
    -- dispute is lost, cleared when it is won.
    disputed_cents BIGINT NOT NULL DEFAULT 0 CHECK (disputed_cents >= 0),
    dispute_status VARCHAR(40),
    -- Stripe's fee and the net amount of the charge (balance transaction),
    -- when known.
    fee_cents BIGINT,
    net_cents BIGINT,
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
CREATE INDEX idx_payments_subscription ON payments (subscription_id)
    WHERE subscription_id IS NOT NULL;

-- Every refund Stripe reports (webhook `refund.*`, `charge.refund.updated`,
-- the refunds of a `charge.refunded` charge, and the finance sync), one row
-- each, so it is counted in the month it happened and a refund that later
-- fails stops counting.
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
