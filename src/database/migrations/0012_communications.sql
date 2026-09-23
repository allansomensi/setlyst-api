-- Communications: per-user communication preferences, a transactional
-- e-mail outbox, platform announcements and editable release notes.

-- ---------------------------------------------------------------------
-- Communication preferences (which categories reach the user by e-mail
-- and in the app). Shape validated by the API; missing keys fall back to
-- the defaults defined in `models::communication`.
-- ---------------------------------------------------------------------

ALTER TABLE user_preferences
    ADD COLUMN communication JSONB NOT NULL DEFAULT '{}';

-- ---------------------------------------------------------------------
-- E-mail outbox. Every e-mail is written here first (in the same
-- transaction as the change that caused it, where possible) and delivered
-- by a background worker, so a slow or unavailable SMTP server never
-- blocks or fails a request.
-- ---------------------------------------------------------------------

CREATE TYPE email_status AS ENUM ('pending', 'sending', 'sent', 'failed', 'skipped');

CREATE TABLE email_outbox (
    id UUID PRIMARY KEY,
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    to_email VARCHAR(254) NOT NULL,
    -- Template identifier (see `email::templates`).
    template VARCHAR(64) NOT NULL,
    locale VARCHAR(10) NOT NULL DEFAULT 'en',
    -- Template variables. Cleared once the message is delivered (or given
    -- up on) so one-time codes don't linger in the database.
    payload JSONB NOT NULL DEFAULT '{}',
    status email_status NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error VARCHAR(1000),
    scheduled_at TIMESTAMP NOT NULL,
    locked_at TIMESTAMP,
    sent_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_email_outbox_pending ON email_outbox (scheduled_at) WHERE status = 'pending';
CREATE INDEX idx_email_outbox_user ON email_outbox (user_id, created_at DESC);
CREATE INDEX idx_email_outbox_created ON email_outbox (created_at);

-- ---------------------------------------------------------------------
-- Announcements published by staff.
-- ---------------------------------------------------------------------

CREATE TYPE announcement_level AS ENUM ('info', 'success', 'warning', 'critical');

CREATE TABLE announcements (
    id UUID PRIMARY KEY,
    title VARCHAR(120) NOT NULL,
    -- Plain text; line breaks are preserved, URLs are not auto-linked.
    body TEXT NOT NULL,
    level announcement_level NOT NULL DEFAULT 'info',
    -- Delivery channels.
    show_modal BOOLEAN NOT NULL DEFAULT FALSE,
    show_banner BOOLEAN NOT NULL DEFAULT FALSE,
    send_notification BOOLEAN NOT NULL DEFAULT TRUE,
    send_email BOOLEAN NOT NULL DEFAULT FALSE,
    -- Behaviour.
    dismissible BOOLEAN NOT NULL DEFAULT TRUE,
    requires_acknowledgement BOOLEAN NOT NULL DEFAULT FALSE,
    cta_label VARCHAR(40),
    cta_url VARCHAR(500),
    -- Audience. NULL means "no restriction" for each dimension.
    audience_roles TEXT[],
    audience_plans TEXT[],
    audience_locales TEXT[],
    -- Visibility window. NULL `starts_at` = as soon as published.
    starts_at TIMESTAMP,
    ends_at TIMESTAMP,
    published_at TIMESTAMP,
    archived_at TIMESTAMP,
    -- Set once notifications/e-mails were fanned out.
    delivered_at TIMESTAMP,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    CONSTRAINT announcements_window_check CHECK (ends_at IS NULL OR starts_at IS NULL OR ends_at > starts_at)
);

CREATE INDEX idx_announcements_published ON announcements (published_at DESC) WHERE published_at IS NOT NULL;

CREATE TABLE announcement_receipts (
    announcement_id UUID NOT NULL REFERENCES announcements(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    seen_at TIMESTAMP,
    dismissed_at TIMESTAMP,
    acknowledged_at TIMESTAMP,
    PRIMARY KEY (announcement_id, user_id)
);

CREATE INDEX idx_announcement_receipts_user ON announcement_receipts (user_id);

-- ---------------------------------------------------------------------
-- Release notes ("What's new"), editable from the staff console.
-- `title` and every item's `text` are objects keyed by locale:
-- {"en": "...", "pt-BR": "...", "es": "..."}.
-- ---------------------------------------------------------------------

CREATE TABLE release_notes (
    id UUID PRIMARY KEY,
    version VARCHAR(20) NOT NULL UNIQUE,
    title JSONB NOT NULL,
    -- [{"kind": "new" | "improved" | "fixed" | "security", "text": {...}}]
    items JSONB NOT NULL DEFAULT '[]',
    released_on DATE NOT NULL,
    -- NULL = draft (visible to staff only).
    published_at TIMESTAMP,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE INDEX idx_release_notes_released ON release_notes (released_on DESC);

INSERT INTO release_notes (id, version, title, items, released_on, published_at, created_at, updated_at)
VALUES
(
    gen_random_uuid(),
    '0.10.0',
    '{"en": "Notifications, analytics and favorites", "pt-BR": "Notificações, estatísticas e favoritos", "es": "Notificaciones, estadísticas y favoritos"}',
    '[
      {"kind": "new", "text": {"en": "In-app notifications when your role in a band changes or you are removed from one.", "pt-BR": "Notificações no app quando seu papel em uma banda muda ou quando você é removido de uma.", "es": "Notificaciones en la app cuando cambia tu rol en una banda o te eliminan de una."}},
      {"kind": "new", "text": {"en": "Analytics: your activity over time, top genres and artists.", "pt-BR": "Estatísticas: sua atividade ao longo do tempo, principais gêneros e artistas.", "es": "Estadísticas: tu actividad a lo largo del tiempo, géneros y artistas principales."}},
      {"kind": "new", "text": {"en": "Favorite setlists and bands to keep them at the top.", "pt-BR": "Marque setlists e bandas como favoritos para mantê-los no topo.", "es": "Marca setlists y bandas como favoritos para tenerlos arriba."}}
    ]',
    DATE '2026-08-20',
    TIMESTAMP '2026-08-20 12:00:00',
    TIMESTAMP '2026-08-20 12:00:00',
    TIMESTAMP '2026-08-20 12:00:00'
),
(
    gen_random_uuid(),
    '0.11.0',
    '{"en": "Tags, a new PDF engine and a safer platform", "pt-BR": "Tags, novo motor de PDF e uma plataforma mais segura", "es": "Etiquetas, un nuevo motor de PDF y una plataforma más segura"}',
    '[
      {"kind": "new", "text": {"en": "Tag your songs (\"ballad\", \"opener\", \"romantic\") and filter your library by tag. Click any tag to see every song that shares it. Rename or merge tags from the Songs page.", "pt-BR": "Adicione tags às suas músicas (\"balada\", \"abertura\", \"romântica\") e filtre o repertório por tag. Clique em uma tag para ver todas as músicas que a usam. Renomeie ou junte tags na página de Músicas.", "es": "Etiqueta tus canciones («balada», «apertura», «romántica») y filtra tu repertorio por etiqueta. Haz clic en una etiqueta para ver todas las canciones que la usan. Renombra o une etiquetas desde la página de Canciones."}},
      {"kind": "new", "text": {"en": "PDF export rebuilt: compact and two-column layouts, text size, paper size and orientation, margins, page numbers, an optional Setlyst watermark and a full songbook with chords aligned above the lyrics. Save your favourite setup as the default.", "pt-BR": "Exportação para PDF refeita: layout compacto e em duas colunas, tamanho do texto, papel e orientação, margens, numeração de páginas, marca d''água opcional e um songbook completo com os acordes alinhados sobre a letra. Salve sua configuração favorita como padrão.", "es": "Exportación a PDF renovada: diseño compacto y a dos columnas, tamaño de texto, papel y orientación, márgenes, números de página, marca de agua opcional y un cancionero completo con los acordes alineados sobre la letra. Guarda tu configuración favorita como predeterminada."}},
      {"kind": "new", "text": {"en": "Your preferences now follow you to every device: Live Mode defaults, PDF defaults and list size are in Settings.", "pt-BR": "Suas preferências agora acompanham você em todos os dispositivos: padrões do Modo Ao Vivo, padrões de PDF e tamanho das listas ficam em Configurações.", "es": "Tus preferencias ahora te siguen en todos los dispositivos: valores del Modo en vivo, del PDF y tamaño de listas están en Configuración."}},
      {"kind": "security", "text": {"en": "Stronger passwords: every account must meet the new password policy. Changing your password signs you out everywhere.", "pt-BR": "Senhas mais fortes: toda conta precisa atender à nova política de senhas. Alterar a senha encerra a sessão em todos os dispositivos.", "es": "Contraseñas más fuertes: toda cuenta debe cumplir la nueva política. Cambiar la contraseña cierra tu sesión en todos los dispositivos."}},
      {"kind": "improved", "text": {"en": "See who last changed a song, setlist or band, and when.", "pt-BR": "Veja quem alterou por último uma música, setlist ou banda, e quando.", "es": "Consulta quién modificó por última vez una canción, setlist o banda, y cuándo."}},
      {"kind": "improved", "text": {"en": "Settings has a new layout, with Usage and limits showing how much of your space you are using.", "pt-BR": "Configurações ganhou um novo layout, com a seção Uso e limites mostrando quanto do seu espaço está em uso.", "es": "Configuración tiene un nuevo diseño, con Uso y límites mostrando cuánto de tu espacio estás usando."}},
      {"kind": "fixed", "text": {"en": "Clearing a song''s BPM, key or duration, a gig''s notes or a band''s description now actually clears it.", "pt-BR": "Apagar o BPM, tom ou duração de uma música, as notas de um show ou a descrição de uma banda agora realmente apaga o valor.", "es": "Borrar el BPM, tono o duración de una canción, las notas de un concierto o la descripción de una banda ahora sí lo borra."}},
      {"kind": "fixed", "text": {"en": "Deleting an account no longer removes the band setlists and songs that person created.", "pt-BR": "Excluir uma conta não remove mais as setlists e músicas de banda criadas por essa pessoa.", "es": "Eliminar una cuenta ya no borra los setlists y canciones de banda que esa persona creó."}}
    ]',
    DATE '2026-09-22',
    TIMESTAMP '2026-09-22 12:00:00',
    TIMESTAMP '2026-09-22 12:00:00',
    TIMESTAMP '2026-09-22 12:00:00'
);

-- ---------------------------------------------------------------------
-- New notification kinds.
-- ---------------------------------------------------------------------

ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'announcement';
ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'release_published';
ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'band_suggestion_created';
ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'band_suggestion_resolved';
ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'moderation_action';
ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'subscription_changed';
ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'trial_ending';
ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'credits_granted';
ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'security_alert';
