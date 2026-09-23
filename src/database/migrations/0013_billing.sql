-- Subscriptions: plans, per-user subscriptions (trial, complimentary,
-- paid), promo codes, promotions, referrals and platform credits.
--
-- Payment processing is not part of this migration: `subscriptions.source
-- = 'payment'` and `external_ref` are reserved for the payment provider
-- integration. Whether plans are enforced at all is a platform setting
-- (`platform_settings.key = 'billing'`, `enforced`), off by default: until
-- it is switched on every account keeps full access.

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

INSERT INTO plans (code, name, description, price_monthly_cents, price_yearly_cents, currency, limits, features, highlighted, is_public, sort_order, updated_at)
VALUES
(
    'basic',
    '{"en": "Basic", "pt-BR": "Básico", "es": "Básico"}',
    '{"en": "For solo musicians who need an organized repertoire and reliable stage sheets.", "pt-BR": "Para músicos solo que precisam de um repertório organizado e setlists confiáveis no palco.", "es": "Para músicos solistas que necesitan un repertorio organizado y setlists confiables en el escenario."}',
    1490, 14900, 'BRL',
    '{"songs": 300, "artists": 150, "setlists": 40, "gigs": 100, "tags": 30, "bands_owned": 0, "band_memberships": 3, "band_members": 6, "band_setlists": 40, "band_gigs": 100, "band_songs": 300, "setlist_items": 60, "tours": 0, "band_tours": 0}',
    '{"create_bands": false, "tours": false, "analytics_export": false, "advanced_pdf": false, "chordpro_import": true, "song_suggestions": true, "public_sharing": true, "offline_mode": true, "priority_support": false}',
    FALSE, TRUE, 10, NOW() AT TIME ZONE 'utc'
),
(
    'intermediate',
    '{"en": "Intermediate", "pt-BR": "Intermediário", "es": "Intermedio"}',
    '{"en": "For working musicians and small bands that play regularly.", "pt-BR": "Para músicos na ativa e bandas pequenas que tocam com frequência.", "es": "Para músicos en activo y bandas pequeñas que tocan con frecuencia."}',
    2490, 24900, 'BRL',
    '{"songs": 1000, "artists": 500, "setlists": 150, "gigs": 400, "tags": 80, "bands_owned": 2, "band_memberships": 10, "band_members": 12, "band_setlists": 150, "band_gigs": 400, "band_songs": 1000, "setlist_items": 120, "tours": 10, "band_tours": 10}',
    '{"create_bands": true, "tours": true, "analytics_export": true, "advanced_pdf": true, "chordpro_import": true, "song_suggestions": true, "public_sharing": true, "offline_mode": true, "priority_support": false}',
    TRUE, TRUE, 20, NOW() AT TIME ZONE 'utc'
),
(
    'pro',
    '{"en": "Pro", "pt-BR": "Pro", "es": "Pro"}',
    '{"en": "For professionals, bands and teams with large repertoires and busy calendars.", "pt-BR": "Para profissionais, bandas e equipes com repertórios grandes e agenda cheia.", "es": "Para profesionales, bandas y equipos con repertorios grandes y agenda llena."}',
    3990, 39900, 'BRL',
    '{"songs": 5000, "artists": 2000, "setlists": 600, "gigs": 2000, "tags": 200, "bands_owned": 10, "band_memberships": 30, "band_members": 40, "band_setlists": 600, "band_gigs": 2000, "band_songs": 5000, "setlist_items": 200, "tours": 50, "band_tours": 50}',
    '{"create_bands": true, "tours": true, "analytics_export": true, "advanced_pdf": true, "chordpro_import": true, "song_suggestions": true, "public_sharing": true, "offline_mode": true, "priority_support": true}',
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
    -- Reserved for the payment provider (customer/subscription id).
    external_ref VARCHAR(255),
    note VARCHAR(500),
    -- Last "your trial ends soon" reminder, so it is sent once.
    trial_reminder_sent_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

CREATE INDEX idx_subscriptions_status ON subscriptions (status);
CREATE INDEX idx_subscriptions_period_end ON subscriptions (current_period_end) WHERE current_period_end IS NOT NULL;
CREATE INDEX idx_subscriptions_trial_end ON subscriptions (trial_ends_at) WHERE trial_ends_at IS NOT NULL;

-- Every change to a subscription, for support and auditing.
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
    -- A percentage off future payments (applied by the payment
    -- integration; stored on redemption).
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

CREATE TABLE promo_redemptions (
    id UUID PRIMARY KEY,
    promo_code_id UUID NOT NULL REFERENCES promo_codes(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    redeemed_at TIMESTAMP NOT NULL,
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

-- ---------------------------------------------------------------------
-- Referrals and platform credits
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
    rewarded_at TIMESTAMP
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
