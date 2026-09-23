-- Release notes for v0.12. Published on the date the migration runs (the
-- deploy date); staff can adjust the text and the date in the console.

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
)
ON CONFLICT (version) DO NOTHING;
