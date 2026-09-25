-- Subscriptions: plans, per-user subscriptions (trial, complimentary,
-- paid through Stripe), promo codes, promotions, referrals and platform
-- credits.
--
-- Whether plans are enforced at all is a platform setting
-- (`platform_settings.key = 'billing'`, `enforced`), off by default. While
-- it is off (the beta), every verified account has every feature within
-- the platform defaults. Once it's on, accounts without a plan fall to the
-- free tier (one band, no tours, no PDF or report exports) and accounts
-- that haven't verified their e-mail get very small limits either way
-- (both tiers are built in: `QuotaLimits::FREE`, `QuotaLimits::UNVERIFIED`).
--
-- Stripe is the source of truth for paid subscriptions: the webhook
-- (`POST /webhooks/stripe`) mirrors each Stripe subscription into
-- `subscriptions` with `source = 'payment'` and the Stripe subscription id
-- in `external_ref`.

-- ---------------------------------------------------------------------
-- Plans
-- ---------------------------------------------------------------------

CREATE TABLE plans (
    code VARCHAR(32) PRIMARY KEY,
    -- Localized strings: {"en": "...", "pt-BR": "...", "es": "..."}.
    name JSONB NOT NULL,
    description JSONB NOT NULL DEFAULT '{}',
    price_monthly_cents INTEGER NOT NULL DEFAULT 0 CHECK (price_monthly_cents >= 0),
    price_yearly_cents INTEGER NOT NULL DEFAULT 0 CHECK (price_yearly_cents >= 0),
    currency VARCHAR(3) NOT NULL DEFAULT 'BRL',
    -- A complete `QuotaLimits` object.
    limits JSONB NOT NULL DEFAULT '{}',
    -- Feature flags: {"create_bands": true, "tours": false, ...}.
    features JSONB NOT NULL DEFAULT '{}',
    highlighted BOOLEAN NOT NULL DEFAULT FALSE,
    is_public BOOLEAN NOT NULL DEFAULT TRUE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

CREATE INDEX idx_plans_updated_by ON plans (updated_by) WHERE updated_by IS NOT NULL;

-- Limits are sized for real use on modest infrastructure: a solo working
-- musician rarely keeps more than a few hundred songs, a busy one plays
-- 100-150 gigs a year, and a long night is ~60 songs. `pdf_export` is
-- included in every paid plan (advanced options still need
-- `advanced_pdf`).
INSERT INTO plans (code, name, description, price_monthly_cents, price_yearly_cents, currency, limits, features, highlighted, is_public, sort_order, updated_at)
VALUES
(
    'basic',
    '{"en": "Basic", "pt-BR": "Básico", "es": "Básico"}',
    '{"en": "For solo musicians who need an organized repertoire and reliable stage sheets.", "pt-BR": "Para músicos solo que precisam de um repertório organizado e setlists confiáveis no palco.", "es": "Para músicos solistas que necesitan un repertorio organizado y setlists confiables en el escenario."}',
    1490, 14900, 'BRL',
    '{"songs": 250, "artists": 150, "setlists": 30, "gigs": 100, "tags": 30, "bands_owned": 1, "band_memberships": 3, "band_members": 6, "band_setlists": 30, "band_gigs": 100, "band_songs": 300, "setlist_items": 60, "tours": 0, "band_tours": 0}',
    '{"create_bands": true, "tours": false, "analytics_export": false, "pdf_export": true, "advanced_pdf": false, "chordpro_import": true, "song_suggestions": true, "public_sharing": true, "offline_mode": true, "priority_support": false}',
    FALSE, TRUE, 10, NOW() AT TIME ZONE 'utc'
),
(
    'intermediate',
    '{"en": "Intermediate", "pt-BR": "Intermediário", "es": "Intermedio"}',
    '{"en": "For working musicians and small bands that play regularly.", "pt-BR": "Para músicos na ativa e bandas pequenas que tocam com frequência.", "es": "Para músicos en activo y bandas pequeñas que tocan con frecuencia."}',
    2490, 24900, 'BRL',
    '{"songs": 800, "artists": 400, "setlists": 100, "gigs": 300, "tags": 60, "bands_owned": 2, "band_memberships": 8, "band_members": 10, "band_setlists": 100, "band_gigs": 300, "band_songs": 800, "setlist_items": 100, "tours": 10, "band_tours": 10}',
    '{"create_bands": true, "tours": true, "analytics_export": true, "pdf_export": true, "advanced_pdf": true, "chordpro_import": true, "song_suggestions": true, "public_sharing": true, "offline_mode": true, "priority_support": false}',
    TRUE, TRUE, 20, NOW() AT TIME ZONE 'utc'
),
(
    'pro',
    '{"en": "Pro", "pt-BR": "Pro", "es": "Pro"}',
    '{"en": "For professionals, bands and teams with large repertoires and busy calendars.", "pt-BR": "Para profissionais, bandas e equipes com repertórios grandes e agenda cheia.", "es": "Para profesionales, bandas y equipos con repertorios grandes y agenda llena."}',
    3990, 39900, 'BRL',
    '{"songs": 2000, "artists": 1000, "setlists": 300, "gigs": 1000, "tags": 150, "bands_owned": 5, "band_memberships": 20, "band_members": 25, "band_setlists": 300, "band_gigs": 1000, "band_songs": 2500, "setlist_items": 150, "tours": 30, "band_tours": 30}',
    '{"create_bands": true, "tours": true, "analytics_export": true, "pdf_export": true, "advanced_pdf": true, "chordpro_import": true, "song_suggestions": true, "public_sharing": true, "offline_mode": true, "priority_support": true}',
    FALSE, TRUE, 30, NOW() AT TIME ZONE 'utc'
);

-- ---------------------------------------------------------------------
-- Subscriptions (at most one per user)
-- ---------------------------------------------------------------------

CREATE TYPE subscription_status AS ENUM ('trialing', 'active', 'past_due', 'canceled', 'expired');
CREATE TYPE subscription_source AS ENUM ('trial', 'admin', 'promo_code', 'credits', 'referral', 'payment');

CREATE TABLE subscriptions (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    plan_code VARCHAR(32) NOT NULL REFERENCES plans(code) ON UPDATE CASCADE,
    status subscription_status NOT NULL,
    source subscription_source NOT NULL,
    started_at TIMESTAMP NOT NULL,
    -- End of the current period. NULL = open-ended (e.g. a complimentary
    -- plan granted by staff without an end date).
    current_period_end TIMESTAMP,
    trial_ends_at TIMESTAMP,
    cancel_at_period_end BOOLEAN NOT NULL DEFAULT FALSE,
    canceled_at TIMESTAMP,
    -- The payment provider's subscription id.
    external_ref VARCHAR(255),
    -- The provider's own status (`trialing`, `active`, `past_due`...).
    provider_status VARCHAR(30),
    -- How often a paid subscription is charged.
    billing_interval VARCHAR(10)
        CHECK (billing_interval IS NULL OR billing_interval IN ('monthly', 'yearly')),
    -- Price of the subscription item as Stripe charges it (minor units),
    -- for recurring revenue at the price each subscriber really pays.
    unit_amount_cents BIGINT CHECK (unit_amount_cents IS NULL OR unit_amount_cents >= 0),
    currency VARCHAR(3),
    -- When the paid subscription last moved to `past_due`: it keeps the
    -- plan for `PAYMENT_GRACE_DAYS` from here, then loses it.
    past_due_since TIMESTAMP,
    -- Subscription Terms accepted on the provider's checkout page.
    terms_version VARCHAR(40),
    terms_accepted_at TIMESTAMP,
    note VARCHAR(500),
    -- Reminders already sent, so each is sent once: "your trial ends
    -- soon", "your first charge is in 7 days" (card-on-file trial) and the
    -- yearly renewal reminder (once per period, keyed by its end).
    trial_reminder_sent_at TIMESTAMP,
    paid_trial_reminder_sent_at TIMESTAMP,
    renewal_reminder_period_end TIMESTAMP,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

CREATE INDEX idx_subscriptions_status ON subscriptions (status);
CREATE INDEX idx_subscriptions_period_end ON subscriptions (current_period_end) WHERE current_period_end IS NOT NULL;
CREATE INDEX idx_subscriptions_trial_end ON subscriptions (trial_ends_at) WHERE trial_ends_at IS NOT NULL;
CREATE INDEX idx_subscriptions_external_ref ON subscriptions (external_ref)
    WHERE external_ref IS NOT NULL;
CREATE INDEX idx_subscriptions_payment_live ON subscriptions (source, status)
    WHERE source = 'payment';
CREATE INDEX idx_subscriptions_updated_by ON subscriptions (updated_by) WHERE updated_by IS NOT NULL;

-- Every change to a subscription, for support, auditing and the finance
-- report.
CREATE TABLE subscription_events (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind VARCHAR(40) NOT NULL,
    from_plan VARCHAR(32),
    to_plan VARCHAR(32),
    from_status subscription_status,
    to_status subscription_status,
    data JSONB NOT NULL DEFAULT '{}',
    actor_id UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_subscription_events_user ON subscription_events (user_id, created_at DESC);
CREATE INDEX idx_subscription_events_kind_created ON subscription_events (kind, created_at);
CREATE INDEX idx_subscription_events_actor ON subscription_events (actor_id) WHERE actor_id IS NOT NULL;

-- Trials: one per e-mail address, for good. Keyed HMAC of the canonical
-- address (lower case, `+tag` removed, dots removed for Gmail).
-- Deliberately NOT tied to `users`: deleting the account and signing up
-- again with the same address must not restart the trial. Only a keyed
-- hash is kept, never the address.
CREATE TABLE trial_claims (
    email_hash BYTEA PRIMARY KEY,
    claimed_at TIMESTAMP NOT NULL
);

-- ---------------------------------------------------------------------
-- Stripe
-- ---------------------------------------------------------------------

-- Checkout Sessions, so a new checkout expires the ones still open (no
-- second subscription from an old tab).
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

-- Webhook events already applied, so a redelivered event is a no-op.
-- Pruned after 30 days by the maintenance job (Stripe stops retrying an
-- event after three days).
CREATE TABLE stripe_events (
    id VARCHAR(255) PRIMARY KEY,
    type VARCHAR(100) NOT NULL,
    received_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_stripe_events_received ON stripe_events (received_at);

-- Provider customers whose deletion failed (account deletion, LGPD):
-- retried hourly until Stripe confirms.
CREATE TABLE stripe_cleanup_queue (
    customer_id VARCHAR(255) PRIMARY KEY,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error VARCHAR(500),
    next_attempt_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL
);

-- Periodic billing jobs that must run at most once per interval across
-- replicas (the daily Stripe reconciliation).
CREATE TABLE billing_job_runs (
    name VARCHAR(60) PRIMARY KEY,
    last_run_at TIMESTAMP NOT NULL
);

-- ---------------------------------------------------------------------
-- Promo codes
-- ---------------------------------------------------------------------

CREATE TYPE promo_kind AS ENUM (
    -- Grants `plan_code` for `duration_days`.
    'plan_grant',
    -- Adds `duration_days` to a running trial (or starts one).
    'trial_extension',
    -- Adds `credits` to the account.
    'credits',
    -- A percentage off the first payment (stored on redemption, spent by
    -- the payment integration).
    'discount'
);

CREATE TABLE promo_codes (
    id UUID PRIMARY KEY,
    -- Stored upper-case; matched case-insensitively.
    code VARCHAR(32) NOT NULL UNIQUE,
    description VARCHAR(255),
    kind promo_kind NOT NULL,
    plan_code VARCHAR(32) REFERENCES plans(code) ON UPDATE CASCADE,
    duration_days INTEGER CHECK (duration_days IS NULL OR duration_days BETWEEN 1 AND 3650),
    credits INTEGER CHECK (credits IS NULL OR credits BETWEEN 1 AND 100000),
    discount_percent INTEGER CHECK (discount_percent IS NULL OR discount_percent BETWEEN 1 AND 100),
    max_redemptions INTEGER CHECK (max_redemptions IS NULL OR max_redemptions >= 1),
    redemptions_count INTEGER NOT NULL DEFAULT 0,
    -- Only accounts created after `starts_at` (or the code's creation)
    -- may redeem it.
    new_users_only BOOLEAN NOT NULL DEFAULT FALSE,
    starts_at TIMESTAMP,
    expires_at TIMESTAMP,
    disabled_at TIMESTAMP,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_promo_codes_created_by ON promo_codes (created_by) WHERE created_by IS NOT NULL;

CREATE TABLE promo_redemptions (
    id UUID PRIMARY KEY,
    promo_code_id UUID NOT NULL REFERENCES promo_codes(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    redeemed_at TIMESTAMP NOT NULL,
    -- A redeemed `discount` code is spent on the first payment that uses
    -- it.
    applied_at TIMESTAMP,
    UNIQUE (promo_code_id, user_id)
);

CREATE INDEX idx_promo_redemptions_user ON promo_redemptions (user_id);

-- ---------------------------------------------------------------------
-- Promotions: time-boxed discounts advertised on the pricing page.
-- ---------------------------------------------------------------------

CREATE TABLE promotions (
    id UUID PRIMARY KEY,
    name VARCHAR(80) NOT NULL,
    -- Localized headline shown on the pricing page.
    headline JSONB NOT NULL DEFAULT '{}',
    -- NULL = applies to every paid plan.
    plan_code VARCHAR(32) REFERENCES plans(code) ON UPDATE CASCADE,
    discount_percent INTEGER NOT NULL CHECK (discount_percent BETWEEN 1 AND 100),
    starts_at TIMESTAMP NOT NULL,
    ends_at TIMESTAMP NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    CONSTRAINT promotions_window_check CHECK (ends_at > starts_at)
);

CREATE INDEX idx_promotions_created_by ON promotions (created_by) WHERE created_by IS NOT NULL;

-- ---------------------------------------------------------------------
-- Referrals and platform credits. The referred account's own bonus is
-- paid on e-mail verification; the referrer's on that account's first
-- paid invoice.
-- ---------------------------------------------------------------------

CREATE TYPE referral_status AS ENUM ('pending', 'rewarded', 'rejected');

CREATE TABLE referrals (
    id UUID PRIMARY KEY,
    referrer_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    referred_id UUID NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    status referral_status NOT NULL DEFAULT 'pending',
    -- Why a referral was rejected (same person, limit reached...).
    note VARCHAR(255),
    created_at TIMESTAMP NOT NULL,
    -- The referrer's reward.
    rewarded_at TIMESTAMP,
    -- The referred account's own bonus.
    referred_rewarded_at TIMESTAMP
);

CREATE INDEX idx_referrals_referrer ON referrals (referrer_id, created_at DESC);

-- Append-only ledger. The balance is the sum of `amount`.
CREATE TABLE credit_ledger (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    amount INTEGER NOT NULL CHECK (amount <> 0),
    -- 'referral_referrer', 'referral_referred', 'promo_code',
    -- 'reward_redemption', 'admin_adjustment'.
    reason VARCHAR(40) NOT NULL,
    reference_id UUID,
    note VARCHAR(255),
    actor_id UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_credit_ledger_user ON credit_ledger (user_id, created_at DESC);
CREATE INDEX idx_credit_ledger_actor ON credit_ledger (actor_id) WHERE actor_id IS NOT NULL;
