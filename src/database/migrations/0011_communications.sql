-- Communications: a transactional e-mail outbox, platform announcements
-- and editable release notes. Per-user communication preferences live in
-- `user_preferences.communication`.

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
    -- The mailbox the address really delivers to (lower case, no `+tag`,
    -- Gmail dots folded), so the per-recipient caps can't be multiplied
    -- with variants of one address.
    to_canonical VARCHAR(254) NOT NULL,
    -- Template identifier (see `email::templates`).
    template VARCHAR(64) NOT NULL,
    locale VARCHAR(10) NOT NULL DEFAULT 'en',
    -- Template variables. Cleared once the message is delivered (or given
    -- up on) so one-time codes don't linger in the database.
    payload JSONB NOT NULL DEFAULT '{}',
    status email_status NOT NULL DEFAULT 'pending',
    -- Lower is sent first: 0 = one-time codes and security notices, 3 =
    -- account and billing messages, 5 = everything else (in-app
    -- notification copies), 9 = bulk (announcements, release notes). A
    -- bulk send of thousands of messages must never delay a password-reset
    -- code.
    priority SMALLINT NOT NULL DEFAULT 5,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error VARCHAR(1000),
    scheduled_at TIMESTAMP NOT NULL,
    locked_at TIMESTAMP,
    sent_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL
);

-- The worker claims due messages by priority, then age.
CREATE INDEX idx_email_outbox_pending_priority
    ON email_outbox (priority, scheduled_at) WHERE status = 'pending';
-- Stale `sending` locks are recovered on every worker run.
CREATE INDEX idx_email_outbox_sending
    ON email_outbox (locked_at) WHERE status = 'sending';
-- Global hourly cap on non-security mail (the worker counts what it sent
-- in the last hour).
CREATE INDEX idx_email_outbox_sent_bulk
    ON email_outbox (sent_at) WHERE status = 'sent' AND priority > 0;
-- Per-recipient caps (`outbox::enqueue`).
CREATE INDEX idx_email_outbox_recipient
    ON email_outbox (LOWER(to_email), template, created_at);
CREATE INDEX idx_email_outbox_canonical
    ON email_outbox (to_canonical, template, created_at);
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
CREATE INDEX idx_announcements_created_by ON announcements (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX idx_announcements_updated_by ON announcements (updated_by) WHERE updated_by IS NOT NULL;

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
CREATE INDEX idx_release_notes_created_by ON release_notes (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX idx_release_notes_updated_by ON release_notes (updated_by) WHERE updated_by IS NOT NULL;

-- Releases so far. v0.12 is published on the date the migration runs (the
-- deploy date); staff can adjust the text and the date in the console.
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

INSERT INTO release_notes (id, version, title, items, released_on, published_at, created_at, updated_at)
VALUES (
    gen_random_uuid(),
    '0.12.0',
    '{"en": "Plans, tours, band repertoire and a more secure account", "pt-BR": "Planos, turnês, repertório da banda e uma conta mais segura", "es": "Planes, giras, repertorio de la banda y una cuenta más segura"}',
    '[
      {"kind": "new", "text": {
        "pt-BR": "Planos Básico, Intermediário e Pro, com 30 dias de teste, códigos promocionais e um programa de indicação que gera créditos. Durante o pré-lançamento, todos os recursos continuam liberados sem custo.",
        "en": "Basic, Intermediate and Pro plans, with a 30-day trial, promo codes and a referral programme that earns credits. During the pre-release period every feature remains free.",
        "es": "Planes Básico, Intermedio y Pro, con 30 días de prueba, códigos promocionales y un programa de referidos que genera créditos. Durante el prelanzamiento, todas las funciones siguen siendo gratuitas."}},
      {"kind": "new", "text": {
        "pt-BR": "Turnês: agrupe shows com datas de início e fim e acompanhe cada show com a sua setlist.",
        "en": "Tours: group gigs under a start and end date and follow each gig with its setlist.",
        "es": "Giras: agrupa conciertos con fecha de inicio y fin y sigue cada concierto con su setlist."}},
      {"kind": "new", "text": {
        "pt-BR": "Cada banda agora tem um Repertório, preenchido automaticamente com as músicas das suas setlists. Ao montar uma setlist da banda, escolha direto do repertório.",
        "en": "Every band now has a Repertoire, filled automatically with the songs of its setlists. When building a band setlist, pick songs straight from it.",
        "es": "Cada banda tiene ahora un Repertorio, que se completa automáticamente con las canciones de sus setlists. Al armar una setlist de la banda, elige directamente del repertorio."}},
      {"kind": "new", "text": {
        "pt-BR": "Sugestões com votação: os integrantes sugerem músicas para as setlists da banda e votam antes de a música entrar.",
        "en": "Suggestions with voting: members suggest songs for the band''s setlists and vote before a song is added.",
        "es": "Sugerencias con votación: los integrantes sugieren canciones para las setlists de la banda y votan antes de que se agreguen."}},
      {"kind": "new", "text": {
        "pt-BR": "Lembretes na página da banda, com cores, data e destaque no topo.",
        "en": "Reminders on the band page, with colours, a date and pinning.",
        "es": "Recordatorios en la página de la banda, con colores, fecha y opción de fijar."}},
      {"kind": "new", "text": {
        "pt-BR": "Lixeira: músicas, artistas, setlists, shows e turnês excluídos podem ser restaurados por 30 dias.",
        "en": "Trash: deleted songs, artists, setlists, gigs and tours can be restored for 30 days.",
        "es": "Papelera: las canciones, artistas, setlists, conciertos y giras eliminados se pueden restaurar durante 30 días."}},
      {"kind": "new", "text": {
        "pt-BR": "Novos campos nas músicas: energia, compasso, capotraste, afinação e observações de execução. Músicas e setlists aceitam links do YouTube, Spotify, Google Drive e outros serviços.",
        "en": "New song fields: energy, time signature, capo, tuning and performance notes. Songs and setlists accept links to YouTube, Spotify, Google Drive and other services.",
        "es": "Nuevos campos en las canciones: energía, compás, cejilla, afinación y notas de interpretación. Canciones y setlists aceptan enlaces de YouTube, Spotify, Google Drive y otros servicios."}},
      {"kind": "new", "text": {
        "pt-BR": "A análise da setlist mostra a curva de energia ao lado do BPM e aponta pontos fortes e pontos a melhorar na sequência do show.",
        "en": "Setlist analysis shows the energy curve next to BPM and points out strengths and what to improve in the running order.",
        "es": "El análisis de la setlist muestra la curva de energía junto al BPM y señala fortalezas y puntos a mejorar en el orden del show."}},
      {"kind": "new", "text": {
        "pt-BR": "Exporte cada música em PDF ou ChordPro e importe músicas em ChordPro com pré-visualização.",
        "en": "Export any song to PDF or ChordPro, and import ChordPro songs with a preview.",
        "es": "Exporta cualquier canción en PDF o ChordPro e importa canciones ChordPro con vista previa."}},
      {"kind": "new", "text": {
        "pt-BR": "Fixe setlists, bandas, músicas, shows e turnês na tela inicial para acessá-los mais rápido.",
        "en": "Pin setlists, bands, songs, gigs and tours to the home screen for quicker access.",
        "es": "Fija setlists, bandas, canciones, conciertos y giras en la pantalla de inicio para acceder más rápido."}},
      {"kind": "new", "text": {
        "pt-BR": "Perfil com foto, apresentação, cidade e instrumentos, além de avisos da plataforma e preferências de comunicação por e-mail.",
        "en": "Profiles with a photo, bio, city and instruments, plus platform announcements and e-mail communication preferences.",
        "es": "Perfil con foto, presentación, ciudad e instrumentos, además de avisos de la plataforma y preferencias de comunicación por correo."}},
      {"kind": "security", "text": {
        "pt-BR": "Autenticação em dois fatores opcional, recuperação de senha por código enviado ao e-mail, entrada com Google e proteção adicional contra tentativas de acesso indevido.",
        "en": "Optional two-factor authentication, password recovery with a code sent by e-mail, Google sign-in and extra protection against unauthorized sign-in attempts.",
        "es": "Autenticación en dos pasos opcional, recuperación de contraseña con un código enviado por correo, acceso con Google y protección adicional contra intentos de acceso indebido."}},
      {"kind": "improved", "text": {
        "pt-BR": "Estatísticas reorganizadas, com exportação em CSV, PDF e imagem.",
        "en": "Statistics reorganized, with CSV, PDF and image export.",
        "es": "Estadísticas reorganizadas, con exportación en CSV, PDF e imagen."}},
      {"kind": "improved", "text": {
        "pt-BR": "Tema claro redesenhado, com mais contraste entre fundo, cartões e bordas.",
        "en": "Redesigned light theme, with clearer contrast between background, cards and borders.",
        "es": "Tema claro rediseñado, con más contraste entre fondo, tarjetas y bordes."}},
      {"kind": "fixed", "text": {
        "pt-BR": "Correções na importação de backup, na edição de shows, na cópia de links e na navegação do editor de letras.",
        "en": "Fixes to backup import, gig editing, link copying and navigation in the lyrics editor.",
        "es": "Correcciones en la importación de copias de seguridad, la edición de conciertos, la copia de enlaces y la navegación del editor de letras."}}
    ]',
    (NOW() AT TIME ZONE 'utc')::date,
    NOW() AT TIME ZONE 'utc',
    NOW() AT TIME ZONE 'utc',
    NOW() AT TIME ZONE 'utc'
);
