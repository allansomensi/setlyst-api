//! Transactional e-mail templates, rendered in English, Brazilian
//! Portuguese and Spanish.
//!
//! Every template produces a subject, an HTML body and a plain-text body
//! from the same content blocks, so both parts always say the same thing.
//! The HTML is deliberately conservative (table layout, inline styles, no
//! remote images or fonts) because e-mail clients strip most CSS and block
//! external resources. Every variable is HTML-escaped; only the fixed copy
//! below is trusted.

use crate::{models::communication::Category, services::billing::BILLING_NOTICE_KINDS};
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Brand color (Setlyst purple).
const BRAND: &str = "#6d28d9";
const TEXT: &str = "#1f1b2e";
const MUTED: &str = "#5b5670";
const BACKGROUND: &str = "#f4f2fa";
const FONT: &str = "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";
/// Longest subject line sent.
pub const MAX_SUBJECT_CHARS: usize = 120;

/// The e-mail languages. Anything else falls back to English.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    PtBr,
    Es,
}

impl Locale {
    pub const ALL: [Locale; 3] = [Locale::En, Locale::PtBr, Locale::Es];

    pub fn parse(value: &str) -> Self {
        match value {
            "pt-BR" | "pt" | "pt_BR" => Locale::PtBr,
            "es" => Locale::Es,
            _ => Locale::En,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::PtBr => "pt-BR",
            Locale::Es => "es",
        }
    }

    /// Picks the string for this locale.
    pub fn pick<'a>(&self, en: &'a str, pt: &'a str, es: &'a str) -> &'a str {
        match self {
            Locale::En => en,
            Locale::PtBr => pt,
            Locale::Es => es,
        }
    }

    pub fn format_date(&self, date: NaiveDate) -> String {
        match self {
            Locale::En => date.format("%b %-d, %Y").to_string(),
            Locale::PtBr | Locale::Es => date.format("%d/%m/%Y").to_string(),
        }
    }

    pub fn format_datetime(&self, value: NaiveDateTime) -> String {
        self.format_date(value.date())
    }
}

/// Picks a localized value out of `{"en": ..., "pt-BR": ..., "es": ...}`,
/// falling back to English, then to any value.
pub fn localized(value: &Value, locale: Locale) -> String {
    value
        .get(locale.code())
        .and_then(Value::as_str)
        .or_else(|| value.get("en").and_then(Value::as_str))
        .or_else(|| {
            value
                .as_object()
                .and_then(|o| o.values().find_map(Value::as_str))
        })
        .or_else(|| value.as_str())
        .unwrap_or_default()
        .to_string()
}

/// A release note item as stored (`{kind, text: {locale: ...}}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseItem {
    pub kind: String,
    pub text: Value,
}

/// Every e-mail the platform sends, with its variables. Stored in the
/// outbox as `template` (the variant name) + `payload` (the fields).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "template", content = "data", rename_all = "snake_case")]
pub enum EmailTemplate {
    EmailVerificationCode {
        username: String,
        code: String,
        expires_minutes: i64,
    },
    PasswordResetCode {
        username: String,
        code: String,
        expires_minutes: i64,
    },
    EmailChangeCode {
        username: String,
        code: String,
        expires_minutes: i64,
    },
    PasswordChanged {
        username: String,
    },
    TwoFactorEnabled {
        username: String,
    },
    TwoFactorDisabled {
        username: String,
    },
    EmailChangedNotice {
        username: String,
        /// Partially masked new address (`a***@example.com`).
        new_email_masked: String,
    },
    AccountDeleted {
        username: String,
    },
    /// Copy of an in-app notification. Title and lines are already
    /// localized (see `email::notification_text`).
    Notification {
        title: String,
        lines: Vec<String>,
        cta_label: Option<String>,
        /// App path, e.g. `/dashboard/bands/<id>`.
        cta_path: Option<String>,
        category: Category,
    },
    Announcement {
        title: String,
        body: String,
        level: String,
        cta_label: Option<String>,
        /// Relative app path (`/dashboard/...`) or absolute `https` URL.
        cta_url: Option<String>,
    },
    ReleaseNotes {
        version: String,
        /// Localized title map.
        title: Value,
        items: Vec<ReleaseItem>,
        released_on: NaiveDate,
    },
    TrialEnding {
        username: String,
        /// Localized plan name map.
        plan_name: Value,
        ends_at: NaiveDateTime,
    },
    SubscriptionChanged {
        username: String,
        /// `plan_granted`, `trial_started`, `extended`, `expired`, `revoked`.
        kind: String,
        plan_name: Value,
        current_period_end: Option<NaiveDateTime>,
    },
    /// Confirmation of a paid subscription (the contract): plan, price,
    /// period, next charge, the terms accepted, how to cancel and the
    /// 7-day withdrawal right.
    SubscriptionConfirmed {
        username: String,
        plan_name: Value,
        /// Minor units per period.
        amount_cents: Option<i64>,
        /// ISO 4217.
        currency: Option<String>,
        /// `monthly` or `yearly`.
        interval: Option<String>,
        next_charge_at: Option<NaiveDateTime>,
        /// Set while a card-on-file trial runs (the first charge is then).
        trial_ends_at: Option<NaiveDateTime>,
        terms_version: Option<String>,
    },
    /// A card-on-file trial ends and the first charge comes (7 days
    /// ahead).
    PaidTrialEnding {
        username: String,
        plan_name: Value,
        amount_cents: Option<i64>,
        currency: Option<String>,
        interval: Option<String>,
        charge_at: NaiveDateTime,
    },
    /// A yearly subscription renews soon.
    RenewalReminder {
        username: String,
        plan_name: Value,
        amount_cents: Option<i64>,
        currency: Option<String>,
        renews_at: NaiveDateTime,
    },
    /// A withdrawal (or a staff refund) was carried out: subscription
    /// ended and money refunded.
    WithdrawalConfirmed {
        username: String,
        plan_name: Value,
        refunded_cents: i64,
        currency: String,
        requested_at: NaiveDateTime,
        /// Ended by staff rather than at the buyer's request.
        #[serde(default)]
        by_staff: bool,
    },
    /// A card dispute (chargeback) canceled the subscription.
    PaymentDisputed {
        username: String,
        plan_name: Value,
    },
    /// The subscription's price changes from `effective_at`.
    PriceChange {
        username: String,
        plan_name: Value,
        interval: Option<String>,
        old_amount_cents: Option<i64>,
        new_amount_cents: i64,
        currency: Option<String>,
        effective_at: NaiveDateTime,
    },
    Welcome {
        username: String,
    },
    /// Step-up re-authentication code for an account without a password.
    ReauthCode {
        username: String,
        code: String,
        expires_minutes: i64,
    },
    /// A security event the owner must hear about. `kind`:
    /// `email_change_started`, `email_in_use`, `google_linked`,
    /// `google_unlinked`, `password_reset_by_staff`,
    /// `email_changed_by_staff`, `login_locked`, `reauth_sessions_revoked`.
    SecurityNotice {
        username: String,
        kind: String,
        /// Masked address (or other short detail) the kind refers to.
        detail: Option<String>,
    },
}

/// Why the recipient gets a message (first line of the footer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FooterReason {
    /// Sent to an address typed into an account (not necessarily verified).
    Address,
    Account,
    Security,
    Deleted,
    Critical,
    /// About a paid subscription: always sent, no unsubscribe link.
    Billing,
    Preference(Category),
}

impl EmailTemplate {
    /// Outbox `template` identifier.
    pub fn name(&self) -> &'static str {
        match self {
            EmailTemplate::EmailVerificationCode { .. } => "email_verification_code",
            EmailTemplate::PasswordResetCode { .. } => "password_reset_code",
            EmailTemplate::EmailChangeCode { .. } => "email_change_code",
            EmailTemplate::PasswordChanged { .. } => "password_changed",
            EmailTemplate::TwoFactorEnabled { .. } => "two_factor_enabled",
            EmailTemplate::TwoFactorDisabled { .. } => "two_factor_disabled",
            EmailTemplate::EmailChangedNotice { .. } => "email_changed_notice",
            EmailTemplate::AccountDeleted { .. } => "account_deleted",
            EmailTemplate::Notification { .. } => "notification",
            EmailTemplate::Announcement { .. } => "announcement",
            EmailTemplate::ReleaseNotes { .. } => "release_notes",
            EmailTemplate::TrialEnding { .. } => "trial_ending",
            EmailTemplate::SubscriptionChanged { .. } => "subscription_changed",
            EmailTemplate::SubscriptionConfirmed { .. } => "subscription_confirmed",
            EmailTemplate::PaidTrialEnding { .. } => "paid_trial_ending",
            EmailTemplate::RenewalReminder { .. } => "renewal_reminder",
            EmailTemplate::WithdrawalConfirmed { .. } => "withdrawal_confirmed",
            EmailTemplate::PaymentDisputed { .. } => "payment_disputed",
            EmailTemplate::PriceChange { .. } => "price_change",
            EmailTemplate::Welcome { .. } => "welcome",
            EmailTemplate::ReauthCode { .. } => "reauth_code",
            EmailTemplate::SecurityNotice { .. } => "security_notice",
        }
    }

    /// Splits into the outbox columns (`template`, `payload`).
    pub fn to_parts(&self) -> (&'static str, Value) {
        let value = serde_json::to_value(self).unwrap_or_default();
        (
            self.name(),
            value
                .get("data")
                .cloned()
                .unwrap_or(Value::Object(Default::default())),
        )
    }

    /// Rebuilds a template from the outbox columns.
    pub fn from_parts(template: &str, payload: &Value) -> Option<Self> {
        serde_json::from_value(serde_json::json!({ "template": template, "data": payload })).ok()
    }

    /// Templates carrying one-time codes: their payload is wiped from the
    /// outbox once the message is sent (or given up on).
    pub fn is_sensitive(&self) -> bool {
        matches!(
            self,
            EmailTemplate::EmailVerificationCode { .. }
                | EmailTemplate::PasswordResetCode { .. }
                | EmailTemplate::EmailChangeCode { .. }
                | EmailTemplate::ReauthCode { .. }
        )
    }

    /// The preference category an unsubscribe link switches off, or `None`
    /// for messages that can't be unsubscribed from (security and
    /// transactional messages, critical service notices).
    pub fn unsubscribe_category(&self) -> Option<Category> {
        match self.footer_reason() {
            FooterReason::Preference(category) if !category.locked() => Some(category),
            _ => None,
        }
    }

    fn footer_reason(&self) -> FooterReason {
        match self {
            EmailTemplate::EmailVerificationCode { .. } | EmailTemplate::EmailChangeCode { .. } => {
                FooterReason::Address
            }
            EmailTemplate::PasswordResetCode { .. }
            | EmailTemplate::PasswordChanged { .. }
            | EmailTemplate::TwoFactorEnabled { .. }
            | EmailTemplate::TwoFactorDisabled { .. }
            | EmailTemplate::EmailChangedNotice { .. } => FooterReason::Security,
            EmailTemplate::AccountDeleted { .. } => FooterReason::Deleted,
            EmailTemplate::Welcome { .. } => FooterReason::Account,
            EmailTemplate::Notification { category, .. } => match category {
                Category::Security => FooterReason::Security,
                other => FooterReason::Preference(*other),
            },
            EmailTemplate::Announcement { level, .. } if level == "critical" => {
                FooterReason::Critical
            }
            EmailTemplate::Announcement { .. } => FooterReason::Preference(Category::Announcements),
            EmailTemplate::ReleaseNotes { .. } => {
                FooterReason::Preference(Category::ProductUpdates)
            }
            // Notices about a paid subscription are billing messages;
            // plan grants, in-app trials and expiries stay optional.
            EmailTemplate::SubscriptionChanged { kind, .. }
                if BILLING_NOTICE_KINDS.contains(&kind.as_str()) =>
            {
                FooterReason::Billing
            }
            EmailTemplate::SubscriptionConfirmed { .. }
            | EmailTemplate::PaidTrialEnding { .. }
            | EmailTemplate::RenewalReminder { .. }
            | EmailTemplate::WithdrawalConfirmed { .. }
            | EmailTemplate::PaymentDisputed { .. }
            | EmailTemplate::PriceChange { .. } => FooterReason::Billing,
            EmailTemplate::TrialEnding { .. } | EmailTemplate::SubscriptionChanged { .. } => {
                FooterReason::Preference(Category::Account)
            }
            // Sent to an address that may belong to someone else's
            // account (or to none): "typed into an account".
            EmailTemplate::SecurityNotice { kind, .. } if kind == "email_in_use" => {
                FooterReason::Address
            }
            EmailTemplate::ReauthCode { .. } | EmailTemplate::SecurityNotice { .. } => {
                FooterReason::Security
            }
        }
    }

    /// Renders the message.
    pub fn render(&self, ctx: &RenderContext) -> RenderedEmail {
        let content = self.content(ctx);
        let subject = clean_subject(&content.subject);
        let footer = footer(ctx, self.footer_reason());
        RenderedEmail {
            html: render_html(ctx, &subject, &content, &footer),
            text: render_text(&content, &footer),
            subject,
            unsubscribe_url: footer.unsubscribe.as_ref().map(|(_, _, url)| url.clone()),
        }
    }

    fn content(&self, ctx: &RenderContext) -> Content {
        let l = ctx.locale;
        let app = |path: &str| ctx.app_url(path);
        match self {
            EmailTemplate::EmailVerificationCode {
                username,
                code,
                expires_minutes,
            } => Content {
                subject: l
                    .pick(
                        "Confirm your email address on Setlyst",
                        "Confirme seu e-mail no Setlyst",
                        "Confirma tu correo en Setlyst",
                    )
                    .into(),
                heading: l
                    .pick(
                        "Confirm your email address",
                        "Confirme seu endereço de e-mail",
                        "Confirma tu dirección de correo",
                    )
                    .into(),
                paragraphs: vec![
                    greeting(l, username),
                    l.pick(
                        "Use the code below to confirm this email address for your Setlyst account. It is valid for {n} minutes.",
                        "Use o código abaixo para confirmar este endereço de e-mail na sua conta do Setlyst. Ele é válido por {n} minutos.",
                        "Usa el código a continuación para confirmar esta dirección de correo en tu cuenta de Setlyst. Es válido durante {n} minutos.",
                    )
                    .replace("{n}", &expires_minutes.to_string()),
                ],
                code: Some(code.clone()),
                note: Some(
                    l.pick(
                        "If you did not request this code, you can ignore this message. No changes will be made to your account.",
                        "Se você não solicitou este código, pode ignorar esta mensagem. Nenhuma alteração será feita na sua conta.",
                        "Si no solicitaste este código, puedes ignorar este mensaje. No se realizará ningún cambio en tu cuenta.",
                    )
                    .into(),
                ),
                ..Default::default()
            },
            EmailTemplate::PasswordResetCode {
                username,
                code,
                expires_minutes,
            } => Content {
                subject: l
                    .pick(
                        "Reset your Setlyst password",
                        "Redefinição de senha do Setlyst",
                        "Restablecimiento de contraseña de Setlyst",
                    )
                    .into(),
                heading: l
                    .pick("Reset your password", "Redefina sua senha", "Restablece tu contraseña")
                    .into(),
                paragraphs: vec![
                    greeting(l, username),
                    l.pick(
                        "We received a request to reset the password of your Setlyst account. Use the code below to choose a new password. It is valid for {n} minutes.",
                        "Recebemos um pedido para redefinir a senha da sua conta do Setlyst. Use o código abaixo para criar uma nova senha. Ele é válido por {n} minutos.",
                        "Recibimos una solicitud para restablecer la contraseña de tu cuenta de Setlyst. Usa el código a continuación para crear una nueva contraseña. Es válido durante {n} minutos.",
                    )
                    .replace("{n}", &expires_minutes.to_string()),
                ],
                code: Some(code.clone()),
                note: Some(
                    l.pick(
                        "If you did not make this request, ignore this message. Your current password stays in place, and nobody can access your account without this code.",
                        "Se você não fez este pedido, ignore esta mensagem. Sua senha atual continua valendo e ninguém terá acesso à sua conta sem este código.",
                        "Si no hiciste esta solicitud, ignora este mensaje. Tu contraseña actual sigue vigente y nadie podrá acceder a tu cuenta sin este código.",
                    )
                    .into(),
                ),
                ..Default::default()
            },
            EmailTemplate::EmailChangeCode {
                username,
                code,
                expires_minutes,
            } => Content {
                subject: l
                    .pick(
                        "Confirm your new email on Setlyst",
                        "Confirme seu novo e-mail no Setlyst",
                        "Confirma tu nuevo correo en Setlyst",
                    )
                    .into(),
                heading: l
                    .pick(
                        "Confirm your new email address",
                        "Confirme seu novo endereço de e-mail",
                        "Confirma tu nueva dirección de correo",
                    )
                    .into(),
                paragraphs: vec![
                    l.pick(
                        "A request was made to change the email address of the Setlyst account {u} to this address. Use the code below to confirm. It is valid for {n} minutes.",
                        "Foi solicitada a alteração do e-mail da conta {u} no Setlyst para este endereço. Use o código abaixo para confirmar. Ele é válido por {n} minutos.",
                        "Se solicitó cambiar el correo de la cuenta {u} de Setlyst a esta dirección. Usa el código a continuación para confirmar. Es válido durante {n} minutos.",
                    )
                    .replace("{u}", username)
                    .replace("{n}", &expires_minutes.to_string()),
                ],
                code: Some(code.clone()),
                note: Some(
                    l.pick(
                        "If you do not recognize this request, ignore this message. The account email will not change.",
                        "Se você não reconhece este pedido, ignore esta mensagem. O e-mail da conta não será alterado.",
                        "Si no reconoces esta solicitud, ignora este mensaje. El correo de la cuenta no cambiará.",
                    )
                    .into(),
                ),
                ..Default::default()
            },
            EmailTemplate::PasswordChanged { username } => Content {
                subject: l
                    .pick(
                        "Your Setlyst password was changed",
                        "Sua senha do Setlyst foi alterada",
                        "Se cambió tu contraseña de Setlyst",
                    )
                    .into(),
                heading: l
                    .pick("Password changed", "Senha alterada", "Contraseña cambiada")
                    .into(),
                paragraphs: vec![
                    greeting(l, username),
                    l.pick(
                        "The password of your Setlyst account was changed and all open sessions were signed out. To continue, sign in again with the new password.",
                        "A senha da sua conta do Setlyst foi alterada e todas as sessões abertas foram encerradas. Para continuar, entre novamente com a nova senha.",
                        "La contraseña de tu cuenta de Setlyst se cambió y se cerraron todas las sesiones abiertas. Para continuar, inicia sesión de nuevo con la nueva contraseña.",
                    )
                    .into(),
                ],
                cta: Some((
                    l.pick("Sign in", "Entrar", "Iniciar sesión").into(),
                    app("/login"),
                )),
                note: Some(
                    l.pick(
                        "If this was not you, reset your password right away from the sign-in page and contact support.",
                        "Se não foi você, redefina sua senha imediatamente pela página de acesso e entre em contato com o suporte.",
                        "Si no fuiste tú, restablece tu contraseña de inmediato desde la página de acceso y contacta con soporte.",
                    )
                    .into(),
                ),
                ..Default::default()
            },
            EmailTemplate::TwoFactorEnabled { username } => Content {
                subject: l
                    .pick(
                        "Two-step verification is now on",
                        "A verificação em duas etapas foi ativada",
                        "La verificación en dos pasos está activada",
                    )
                    .into(),
                heading: l
                    .pick(
                        "Two-step verification enabled",
                        "Verificação em duas etapas ativada",
                        "Verificación en dos pasos activada",
                    )
                    .into(),
                paragraphs: vec![
                    l.pick(
                        "From now on, in addition to your password, Setlyst will ask for a code from your authenticator app to sign in to the account {u}.",
                        "A partir de agora, além da senha, o Setlyst pedirá um código do seu aplicativo autenticador para entrar na conta {u}.",
                        "A partir de ahora, además de la contraseña, Setlyst pedirá un código de tu aplicación de autenticación para iniciar sesión en la cuenta {u}.",
                    )
                    .replace("{u}", username),
                    l.pick(
                        "Keep your recovery codes in a safe place. They let you sign in if you lose access to the app.",
                        "Guarde os códigos de recuperação em um local seguro. Eles permitem entrar caso você perca o acesso ao aplicativo.",
                        "Guarda los códigos de recuperación en un lugar seguro. Te permiten iniciar sesión si pierdes el acceso a la aplicación.",
                    )
                    .into(),
                ],
                note: Some(
                    l.pick(
                        "If this was not you, sign in, reset your password and review your security settings.",
                        "Se não foi você, entre na sua conta, redefina a senha e revise as configurações de segurança.",
                        "Si no fuiste tú, inicia sesión, restablece tu contraseña y revisa la configuración de seguridad.",
                    )
                    .into(),
                ),
                ..Default::default()
            },
            EmailTemplate::TwoFactorDisabled { username } => Content {
                subject: l
                    .pick(
                        "Two-step verification was turned off",
                        "A verificação em duas etapas foi desativada",
                        "Se desactivó la verificación en dos pasos",
                    )
                    .into(),
                heading: l
                    .pick(
                        "Two-step verification disabled",
                        "Verificação em duas etapas desativada",
                        "Verificación en dos pasos desactivada",
                    )
                    .into(),
                paragraphs: vec![
                    l.pick(
                        "Two-step verification was turned off for the account {u}. Only your password is now required to sign in.",
                        "A verificação em duas etapas foi desativada na conta {u}. Agora, apenas a senha é pedida para entrar.",
                        "Se desactivó la verificación en dos pasos en la cuenta {u}. Ahora solo se pide la contraseña para iniciar sesión.",
                    )
                    .replace("{u}", username),
                    l.pick(
                        "We recommend keeping this protection on. You can turn it on again under Settings, in the Security section.",
                        "Recomendamos manter essa proteção ativa. Você pode ativá-la novamente em Configurações, na seção Segurança.",
                        "Recomendamos mantener esta protección activa. Puedes activarla de nuevo en Configuración, en la sección Seguridad.",
                    )
                    .into(),
                ],
                note: Some(
                    l.pick(
                        "If this was not you, reset your password right away and contact support.",
                        "Se não foi você, redefina sua senha imediatamente e entre em contato com o suporte.",
                        "Si no fuiste tú, restablece tu contraseña de inmediato y contacta con soporte.",
                    )
                    .into(),
                ),
                ..Default::default()
            },
            EmailTemplate::EmailChangedNotice {
                username,
                new_email_masked,
            } => Content {
                subject: l
                    .pick(
                        "The email of your Setlyst account was changed",
                        "O e-mail da sua conta do Setlyst foi alterado",
                        "Se cambió el correo de tu cuenta de Setlyst",
                    )
                    .into(),
                heading: l
                    .pick("Email address changed", "E-mail alterado", "Correo cambiado")
                    .into(),
                paragraphs: vec![
                    l.pick(
                        "The email address of the Setlyst account {u} was changed to {e}. Account messages will be sent to the new address from now on.",
                        "O e-mail da conta {u} no Setlyst foi alterado para {e}. As mensagens da conta serão enviadas para o novo endereço a partir de agora.",
                        "El correo de la cuenta {u} de Setlyst se cambió a {e}. Los mensajes de la cuenta se enviarán a la nueva dirección a partir de ahora.",
                    )
                    .replace("{u}", username)
                    .replace("{e}", new_email_masked),
                ],
                note: Some(
                    l.pick(
                        "If this was not you, contact support as soon as possible by replying to this message.",
                        "Se não foi você, entre em contato com o suporte o quanto antes respondendo a esta mensagem.",
                        "Si no fuiste tú, contacta con soporte lo antes posible respondiendo a este mensaje.",
                    )
                    .into(),
                ),
                ..Default::default()
            },
            EmailTemplate::AccountDeleted { username } => Content {
                subject: l
                    .pick(
                        "Your Setlyst account was deleted",
                        "Sua conta do Setlyst foi excluída",
                        "Tu cuenta de Setlyst fue eliminada",
                    )
                    .into(),
                heading: l
                    .pick("Account deleted", "Conta excluída", "Cuenta eliminada")
                    .into(),
                paragraphs: vec![
                    greeting(l, username),
                    l.pick(
                        "As requested, your Setlyst account was deleted, along with your personal songs, setlists and gigs. Bands you were responsible for were handed over to another member.",
                        "Conforme solicitado, sua conta do Setlyst foi excluída, junto com suas músicas, setlists e shows pessoais. As bandas pelas quais você era responsável foram transferidas para outro integrante.",
                        "Como solicitaste, tu cuenta de Setlyst fue eliminada, junto con tus canciones, setlists y conciertos personales. Las bandas de las que eras responsable se transfirieron a otro integrante.",
                    )
                    .into(),
                    l.pick(
                        "Thank you for using Setlyst. If you decide to come back, you will be welcome.",
                        "Obrigado por ter usado o Setlyst. Se quiser voltar, será um prazer receber você novamente.",
                        "Gracias por usar Setlyst. Si decides volver, será un placer recibirte de nuevo.",
                    )
                    .into(),
                ],
                note: Some(
                    l.pick(
                        "If you did not request this deletion, reply to this message.",
                        "Se você não solicitou a exclusão, responda a esta mensagem.",
                        "Si no solicitaste esta eliminación, responde a este mensaje.",
                    )
                    .into(),
                ),
                ..Default::default()
            },
            EmailTemplate::Notification {
                title,
                lines,
                cta_label,
                cta_path,
                ..
            } => Content {
                subject: title.clone(),
                heading: title.clone(),
                paragraphs: lines.clone(),
                cta: cta_path.as_ref().map(|path| {
                    (
                        cta_label.clone().unwrap_or_else(|| {
                            l.pick("Open in Setlyst", "Abrir no Setlyst", "Abrir en Setlyst")
                                .into()
                        }),
                        app(path),
                    )
                }),
                ..Default::default()
            },
            EmailTemplate::Announcement {
                title,
                body,
                level,
                cta_label,
                cta_url,
            } => Content {
                subject: if level == "critical" {
                    format!(
                        "{} {title}",
                        l.pick("Important notice:", "Aviso importante:", "Aviso importante:")
                    )
                } else {
                    title.clone()
                },
                heading: title.clone(),
                paragraphs: body
                    .split("\n\n")
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect(),
                cta: match (cta_label, cta_url) {
                    (Some(label), Some(url)) if url.starts_with("https://") => {
                        Some((label.clone(), url.clone()))
                    }
                    (Some(label), Some(path)) if path.starts_with('/') => {
                        Some((label.clone(), app(path)))
                    }
                    _ => None,
                },
                ..Default::default()
            },
            EmailTemplate::ReleaseNotes {
                version,
                title,
                items,
                released_on,
            } => Content {
                subject: l
                    .pick(
                        "What's new in Setlyst {v}",
                        "Novidades do Setlyst: versão {v}",
                        "Novedades de Setlyst: versión {v}",
                    )
                    .replace("{v}", version),
                heading: localized(title, l),
                paragraphs: vec![
                    l.pick(
                        "Version {v}, released on {d}.",
                        "Versão {v}, lançada em {d}.",
                        "Versión {v}, publicada el {d}.",
                    )
                    .replace("{v}", version)
                    .replace("{d}", &l.format_date(*released_on)),
                ],
                items: items
                    .iter()
                    .map(|item| format!("{}: {}", release_kind_label(l, &item.kind), localized(&item.text, l)))
                    .collect(),
                cta: Some((
                    l.pick("See all updates", "Ver todas as novidades", "Ver todas las novedades")
                        .into(),
                    app("/changelog"),
                )),
                ..Default::default()
            },
            EmailTemplate::TrialEnding {
                username,
                plan_name,
                ends_at,
            } => {
                let date = l.format_datetime(*ends_at);
                let plan = localized(plan_name, l);
                Content {
                    subject: l
                        .pick(
                            "Your trial ends soon",
                            "Seu período de teste termina em breve",
                            "Tu periodo de prueba termina pronto",
                        )
                        .into(),
                    heading: l
                        .pick(
                            "Your trial ends on {d}",
                            "Seu período de teste termina em {d}",
                            "Tu periodo de prueba termina el {d}",
                        )
                        .replace("{d}", &date),
                    paragraphs: vec![
                        greeting(l, username),
                        l.pick(
                            "The trial of the {p} plan on your Setlyst account ends on {d}. After that, the features exclusive to the plan are no longer available, but your songs, setlists and gigs stay saved.",
                            "O período de teste do plano {p} na sua conta do Setlyst termina em {d}. Depois disso, os recursos exclusivos do plano deixam de estar disponíveis, mas suas músicas, setlists e shows continuam salvos.",
                            "El periodo de prueba del plan {p} en tu cuenta de Setlyst termina el {d}. Después, las funciones exclusivas del plan dejarán de estar disponibles, pero tus canciones, setlists y conciertos seguirán guardados.",
                        )
                        .replace("{p}", &plan)
                        .replace("{d}", &date),
                        l.pick(
                            "To keep every feature, choose a plan under Settings, in the Subscription section.",
                            "Para continuar com todos os recursos, escolha um plano em Configurações, na seção Assinatura.",
                            "Para mantener todas las funciones, elige un plan en Configuración, en la sección Suscripción.",
                        )
                        .into(),
                    ],
                    cta: Some((l.pick("See plans", "Ver planos", "Ver planes").into(), app("/pricing"))),
                    ..Default::default()
                }
            }
            EmailTemplate::SubscriptionChanged {
                username,
                kind,
                plan_name,
                current_period_end,
            } => {
                let plan = localized(plan_name, l);
                let date = current_period_end.map(|d| l.format_datetime(d));
                let line = match (kind.as_str(), &date) {
                    ("trial_started", Some(d)) => l
                        .pick(
                            "Your {p} plan trial has started and runs until {d}.",
                            "Seu período de teste do plano {p} começou e vai até {d}.",
                            "Comenzó tu periodo de prueba del plan {p} y dura hasta el {d}.",
                        )
                        .replace("{d}", d),
                    ("expired", _) => l
                        .pick(
                            "The {p} plan on your account has expired. Your songs, setlists and gigs stay saved.",
                            "O plano {p} da sua conta expirou. Suas músicas, setlists e shows continuam salvos.",
                            "El plan {p} de tu cuenta venció. Tus canciones, setlists y conciertos siguen guardados.",
                        )
                        .to_string(),
                    ("revoked", _) => l
                        .pick(
                            "The {p} plan was removed from your account. Your songs, setlists and gigs stay saved.",
                            "O plano {p} foi removido da sua conta. Suas músicas, setlists e shows continuam salvos.",
                            "Se retiró el plan {p} de tu cuenta. Tus canciones, setlists y conciertos siguen guardados.",
                        )
                        .to_string(),
                    // Paid subscriptions.
                    ("subscribed" | "resumed", Some(d)) => l
                        .pick(
                            "Your subscription to the {p} plan is active. The next charge is on {d}.",
                            "Sua assinatura do plano {p} está ativa. A próxima cobrança será em {d}.",
                            "Tu suscripción al plan {p} está activa. El próximo cobro será el {d}.",
                        )
                        .replace("{d}", d),
                    ("plan_changed", Some(d)) => l
                        .pick(
                            "Your subscription is now on the {p} plan. The next charge is on {d}.",
                            "Sua assinatura agora é do plano {p}. A próxima cobrança será em {d}.",
                            "Tu suscripción ahora es del plan {p}. El próximo cobro será el {d}.",
                        )
                        .replace("{d}", d),
                    ("payment_failed", _) => l
                        .pick(
                            "We couldn't charge your {p} plan subscription. Update your payment method under Settings, in the Subscription section, to keep your plan.",
                            "Não conseguimos cobrar a assinatura do plano {p}. Atualize a forma de pagamento em Configurações, na seção Assinatura, para manter o seu plano.",
                            "No pudimos cobrar la suscripción del plan {p}. Actualiza el método de pago en Configuración, en la sección Suscripción, para mantener tu plan.",
                        )
                        .to_string(),
                    ("cancel_scheduled", Some(d)) => l
                        .pick(
                            "Your subscription was canceled. The {p} plan stays active until {d} and you won't be charged again.",
                            "Sua assinatura foi cancelada. O plano {p} continua ativo até {d} e não haverá novas cobranças.",
                            "Se canceló tu suscripción. El plan {p} sigue activo hasta el {d} y no habrá más cobros.",
                        )
                        .replace("{d}", d),
                    ("canceled", _) => l
                        .pick(
                            "Your subscription to the {p} plan has ended. Your songs, setlists and gigs stay saved.",
                            "A assinatura do plano {p} foi encerrada. Suas músicas, setlists e shows continuam salvos.",
                            "Terminó tu suscripción al plan {p}. Tus canciones, setlists y conciertos siguen guardados.",
                        )
                        .to_string(),
                    (_, Some(d)) => l
                        .pick(
                            "Your account now has the {p} plan until {d}.",
                            "Sua conta agora tem o plano {p} até {d}.",
                            "Tu cuenta ahora tiene el plan {p} hasta el {d}.",
                        )
                        .replace("{d}", d),
                    (_, None) => l
                        .pick(
                            "Your account now has the {p} plan.",
                            "Sua conta agora tem o plano {p}.",
                            "Tu cuenta ahora tiene el plan {p}.",
                        )
                        .to_string(),
                };
                Content {
                    subject: l
                        .pick(
                            "Your Setlyst subscription was updated",
                            "Sua assinatura do Setlyst foi atualizada",
                            "Se actualizó tu suscripción de Setlyst",
                        )
                        .into(),
                    heading: l
                        .pick("Subscription updated", "Assinatura atualizada", "Suscripción actualizada")
                        .into(),
                    paragraphs: vec![greeting(l, username), line.replace("{p}", &plan)],
                    cta: Some((
                        l.pick("View my subscription", "Ver minha assinatura", "Ver mi suscripción")
                            .into(),
                        app("/dashboard/settings"),
                    )),
                    ..Default::default()
                }
            }
            EmailTemplate::SubscriptionConfirmed {
                username,
                plan_name,
                amount_cents,
                currency,
                interval,
                next_charge_at,
                trial_ends_at,
                terms_version,
            } => {
                let plan = localized(plan_name, l);
                let price = price_text(l, *amount_cents, currency.as_deref(), interval.as_deref());
                let mut paragraphs = vec![
                    greeting(l, username),
                    match &price {
                        Some(price) => l
                            .pick(
                                "Your subscription to the {p} plan is confirmed: {v}.",
                                "Sua assinatura do plano {p} está confirmada: {v}.",
                                "Tu suscripción al plan {p} está confirmada: {v}.",
                            )
                            .replace("{v}", price),
                        None => l
                            .pick(
                                "Your subscription to the {p} plan is confirmed.",
                                "Sua assinatura do plano {p} está confirmada.",
                                "Tu suscripción al plan {p} está confirmada.",
                            )
                            .to_string(),
                    }
                    .replace("{p}", &plan),
                ];
                match (trial_ends_at, next_charge_at) {
                    (Some(trial), _) => paragraphs.push(
                        l.pick(
                            "Your trial continues until {d}; the first charge is made automatically on that date. We will email you 7 days before it. If you cancel before then, nothing is charged.",
                            "O período de teste continua até {d}; a primeira cobrança é feita automaticamente nessa data. Avisaremos você por e-mail 7 dias antes. Se cancelar antes, nada será cobrado.",
                            "El periodo de prueba continúa hasta el {d}; el primer cobro se hace automáticamente en esa fecha. Te avisaremos por correo 7 días antes. Si cancelas antes, no se cobra nada.",
                        )
                        .replace("{d}", &l.format_datetime(*trial)),
                    ),
                    (None, Some(next)) => paragraphs.push(
                        l.pick(
                            "The next charge is on {d}.",
                            "A próxima cobrança será em {d}.",
                            "El próximo cobro será el {d}.",
                        )
                        .replace("{d}", &l.format_datetime(*next)),
                    ),
                    (None, None) => {}
                }
                paragraphs.push(
                    l.pick(
                        "The subscription renews automatically at the current price. You can cancel at any time under Settings > Subscription: the plan stays active until the end of the period already paid and nothing more is charged.",
                        "A assinatura renova automaticamente pelo preço vigente. Você pode cancelar quando quiser em Configurações › Assinatura: o plano continua ativo até o fim do período já pago e não há novas cobranças.",
                        "La suscripción se renueva automáticamente al precio vigente. Puedes cancelarla cuando quieras en Configuración › Suscripción: el plan sigue activo hasta el final del periodo ya pagado y no hay más cobros.",
                    )
                    .into(),
                );
                paragraphs.push(withdrawal_right(l));
                paragraphs.push(
                    l.pick(
                        "Subscription Terms (version {v}): {s}\nTerms of Use: {t}",
                        "Termos de Assinatura (versão {v}): {s}\nTermos de Uso: {t}",
                        "Términos de Suscripción (versión {v}): {s}\nTérminos de Uso: {t}",
                    )
                    .replace("{v}", terms_version.as_deref().unwrap_or("-"))
                    .replace("{s}", &app("/legal/subscription"))
                    .replace("{t}", &app("/legal/terms")),
                );
                Content {
                    subject: l
                        .pick(
                            "Your Setlyst subscription is confirmed",
                            "Sua assinatura do Setlyst está confirmada",
                            "Tu suscripción de Setlyst está confirmada",
                        )
                        .into(),
                    heading: l
                        .pick("Subscription confirmed", "Assinatura confirmada", "Suscripción confirmada")
                        .into(),
                    paragraphs,
                    cta: Some((manage_label(l), app("/dashboard/settings#subscription"))),
                    note: Some(
                        l.pick(
                            "Keep this email: it confirms your subscription. Payment receipts are sent by Stripe, our payment processor.",
                            "Guarde este e-mail: ele confirma a contratação. Os recibos de pagamento são enviados pelo Stripe, nosso processador de pagamentos.",
                            "Guarda este correo: confirma la contratación. Los recibos de pago los envía Stripe, nuestro procesador de pagos.",
                        )
                        .into(),
                    ),
                    ..Default::default()
                }
            }
            EmailTemplate::PaidTrialEnding {
                username,
                plan_name,
                amount_cents,
                currency,
                interval,
                charge_at,
            } => {
                let plan = localized(plan_name, l);
                let date = l.format_datetime(*charge_at);
                let charge = match price_text(l, *amount_cents, currency.as_deref(), interval.as_deref()) {
                    Some(price) => l
                        .pick(
                            "On {d} we will charge {v} for your {p} plan subscription to the card on file.",
                            "Em {d} cobraremos {v} pela assinatura do plano {p} no cartão cadastrado.",
                            "El {d} cobraremos {v} por la suscripción al plan {p} en la tarjeta registrada.",
                        )
                        .replace("{v}", &price),
                    None => l
                        .pick(
                            "On {d} we will make the first charge for your {p} plan subscription to the card on file.",
                            "Em {d} faremos a primeira cobrança da assinatura do plano {p} no cartão cadastrado.",
                            "El {d} haremos el primer cobro de la suscripción al plan {p} en la tarjeta registrada.",
                        )
                        .to_string(),
                };
                Content {
                    subject: l
                        .pick(
                            "Your first Setlyst charge is on {d}",
                            "Sua primeira cobrança do Setlyst será em {d}",
                            "Tu primer cobro de Setlyst será el {d}",
                        )
                        .replace("{d}", &date),
                    heading: l
                        .pick(
                            "Your trial ends on {d}",
                            "Seu período de teste termina em {d}",
                            "Tu periodo de prueba termina el {d}",
                        )
                        .replace("{d}", &date),
                    paragraphs: vec![
                        greeting(l, username),
                        charge.replace("{d}", &date).replace("{p}", &plan),
                        l.pick(
                            "To avoid being charged, cancel before {d} under Settings > Subscription. If you cancel before that date, nothing is charged.",
                            "Para não ser cobrado, cancele até {d} em Configurações › Assinatura. Se cancelar antes dessa data, nada será cobrado.",
                            "Para no recibir el cobro, cancela antes del {d} en Configuración › Suscripción. Si cancelas antes de esa fecha, no se cobra nada.",
                        )
                        .replace("{d}", &date),
                        withdrawal_right(l),
                    ],
                    cta: Some((manage_label(l), app("/dashboard/settings#subscription"))),
                    ..Default::default()
                }
            }
            EmailTemplate::RenewalReminder {
                username,
                plan_name,
                amount_cents,
                currency,
                renews_at,
            } => {
                let plan = localized(plan_name, l);
                let date = l.format_datetime(*renews_at);
                let renewal = match price_text(l, *amount_cents, currency.as_deref(), None) {
                    Some(price) => l
                        .pick(
                            "On {d} your yearly {p} plan subscription renews automatically and we will charge {v} to the card on file.",
                            "Em {d} sua assinatura anual do plano {p} será renovada automaticamente e cobraremos {v} no cartão cadastrado.",
                            "El {d} tu suscripción anual al plan {p} se renovará automáticamente y cobraremos {v} en la tarjeta registrada.",
                        )
                        .replace("{v}", &price),
                    None => l
                        .pick(
                            "On {d} your yearly {p} plan subscription renews automatically and is charged to the card on file.",
                            "Em {d} sua assinatura anual do plano {p} será renovada automaticamente e cobrada no cartão cadastrado.",
                            "El {d} tu suscripción anual al plan {p} se renovará automáticamente y se cobrará en la tarjeta registrada.",
                        )
                        .to_string(),
                };
                Content {
                    subject: l
                        .pick(
                            "Your yearly Setlyst subscription renews on {d}",
                            "Sua assinatura anual do Setlyst renova em {d}",
                            "Tu suscripción anual de Setlyst se renueva el {d}",
                        )
                        .replace("{d}", &date),
                    heading: l
                        .pick("Upcoming renewal", "Renovação próxima", "Próxima renovación")
                        .into(),
                    paragraphs: vec![
                        greeting(l, username),
                        renewal.replace("{d}", &date).replace("{p}", &plan),
                        l.pick(
                            "If you don't want to renew, cancel before that date under Settings > Subscription; the plan stays active until the end of the period already paid.",
                            "Se não quiser renovar, cancele antes dessa data em Configurações › Assinatura; o plano continua ativo até o fim do período já pago.",
                            "Si no quieres renovar, cancela antes de esa fecha en Configuración › Suscripción; el plan sigue activo hasta el final del periodo ya pagado.",
                        )
                        .into(),
                        withdrawal_right(l),
                    ],
                    cta: Some((manage_label(l), app("/dashboard/settings#subscription"))),
                    ..Default::default()
                }
            }
            EmailTemplate::WithdrawalConfirmed {
                username,
                plan_name,
                refunded_cents,
                currency,
                requested_at,
                by_staff,
            } => {
                let plan = localized(plan_name, l);
                let ended = if *by_staff {
                    l.pick(
                        "Your {p} plan subscription was ended by the Setlyst team and will not be charged again.",
                        "A assinatura do plano {p} foi encerrada pela equipe do Setlyst e não haverá novas cobranças.",
                        "El equipo de Setlyst terminó tu suscripción al plan {p} y no habrá más cobros.",
                    )
                    .replace("{p}", &plan)
                } else {
                    l.pick(
                        "We received your request to withdraw on {d}. Your {p} plan subscription has ended and will not be charged again.",
                        "Recebemos seu pedido de desistência em {d}. A assinatura do plano {p} foi encerrada e não haverá novas cobranças.",
                        "Recibimos tu solicitud de desistimiento el {d}. Tu suscripción al plan {p} terminó y no habrá más cobros.",
                    )
                    .replace("{d}", &l.format_datetime(*requested_at))
                    .replace("{p}", &plan)
                };
                let refund = if *refunded_cents > 0 {
                    l.pick(
                        "We refunded {v} to the card used for the purchase. How long it takes to show on your statement depends on the card issuer.",
                        "Estornamos {v} no cartão usado na compra. O prazo para o estorno aparecer na fatura depende do emissor do cartão.",
                        "Reembolsamos {v} en la tarjeta usada en la compra. El plazo para que aparezca en el extracto depende del emisor de la tarjeta.",
                    )
                    .replace("{v}", &money(l, *refunded_cents, currency))
                } else {
                    l.pick(
                        "There was nothing to refund.",
                        "Não havia valores a estornar.",
                        "No había importes que reembolsar.",
                    )
                    .into()
                };
                Content {
                    subject: if *by_staff {
                        l.pick(
                            "Your Setlyst subscription was canceled and refunded",
                            "Sua assinatura do Setlyst foi cancelada e estornada",
                            "Tu suscripción de Setlyst fue cancelada y reembolsada",
                        )
                    } else {
                        l.pick(
                            "Your Setlyst withdrawal is confirmed",
                            "Confirmação da desistência da sua assinatura do Setlyst",
                            "Confirmación del desistimiento de tu suscripción de Setlyst",
                        )
                    }
                    .into(),
                    heading: l
                        .pick("Subscription ended", "Assinatura encerrada", "Suscripción terminada")
                        .into(),
                    paragraphs: vec![
                        greeting(l, username),
                        ended,
                        refund,
                        l.pick(
                            "Your account no longer has a plan; your songs, setlists and gigs stay saved.",
                            "Sua conta passa a não ter plano; suas músicas, setlists e shows continuam salvos.",
                            "Tu cuenta ya no tiene plan; tus canciones, setlists y conciertos siguen guardados.",
                        )
                        .into(),
                    ],
                    cta: Some((manage_label(l), app("/dashboard/settings#subscription"))),
                    note: Some(
                        l.pick(
                            "If you have any questions, reply to this email.",
                            "Se tiver dúvidas, responda a este e-mail.",
                            "Si tienes dudas, responde a este correo.",
                        )
                        .into(),
                    ),
                    ..Default::default()
                }
            }
            EmailTemplate::PaymentDisputed {
                username,
                plan_name,
            } => Content {
                subject: l
                    .pick(
                        "Your Setlyst subscription was canceled after a payment dispute",
                        "Sua assinatura do Setlyst foi cancelada após uma contestação",
                        "Tu suscripción de Setlyst fue cancelada tras una disputa de pago",
                    )
                    .into(),
                heading: l
                    .pick("Payment disputed", "Pagamento contestado", "Pago disputado")
                    .into(),
                paragraphs: vec![
                    greeting(l, username),
                    l.pick(
                        "Your card issuer told us that a payment for your {p} plan subscription was disputed. The subscription was canceled and will not be charged again.",
                        "A administradora do seu cartão nos informou uma contestação de um pagamento da assinatura do plano {p}. A assinatura foi cancelada e não haverá novas cobranças.",
                        "El emisor de tu tarjeta nos informó una disputa de un pago de la suscripción al plan {p}. La suscripción fue cancelada y no habrá más cobros.",
                    )
                    .replace("{p}", &localized(plan_name, l)),
                    l.pick(
                        "Your songs, setlists and gigs stay saved. If you don't recognize this dispute, reply to this email.",
                        "Suas músicas, setlists e shows continuam salvos. Se você não reconhece essa contestação, responda a este e-mail.",
                        "Tus canciones, setlists y conciertos siguen guardados. Si no reconoces esta disputa, responde a este correo.",
                    )
                    .into(),
                ],
                ..Default::default()
            },
            EmailTemplate::PriceChange {
                username,
                plan_name,
                interval,
                old_amount_cents,
                new_amount_cents,
                currency,
                effective_at,
            } => {
                let plan = localized(plan_name, l);
                let date = l.format_datetime(*effective_at);
                let currency = currency.as_deref().unwrap_or("BRL");
                let new_price = money(l, *new_amount_cents, currency);
                let change = match old_amount_cents {
                    Some(old) => l
                        .pick(
                            "From {d}, the {i} price of your {p} plan subscription changes from {o} to {n}.",
                            "A partir de {d}, o preço {i} da sua assinatura do plano {p} passa de {o} para {n}.",
                            "A partir del {d}, el precio {i} de tu suscripción al plan {p} pasa de {o} a {n}.",
                        )
                        .replace("{o}", &money(l, *old, currency)),
                    None => l
                        .pick(
                            "From {d}, the {i} price of your {p} plan subscription will be {n}.",
                            "A partir de {d}, o preço {i} da sua assinatura do plano {p} passa a ser {n}.",
                            "A partir del {d}, el precio {i} de tu suscripción al plan {p} será {n}.",
                        )
                        .to_string(),
                };
                Content {
                    subject: l
                        .pick(
                            "The price of your Setlyst subscription is changing",
                            "O preço da sua assinatura do Setlyst vai mudar",
                            "El precio de tu suscripción de Setlyst va a cambiar",
                        )
                        .into(),
                    heading: l
                        .pick("Price change", "Mudança de preço", "Cambio de precio")
                        .into(),
                    paragraphs: vec![
                        greeting(l, username),
                        change
                            .replace("{d}", &date)
                            .replace("{i}", interval_label(l, interval.as_deref()))
                            .replace("{p}", &plan)
                            .replace("{n}", &new_price),
                        l.pick(
                            "The new price applies from your first renewal after that date. If you don't agree, you can cancel at any time under Settings > Subscription, with no fee; the plan stays active until the end of the period already paid.",
                            "O novo preço vale a partir da primeira renovação após essa data. Se não concordar, você pode cancelar a qualquer momento em Configurações › Assinatura, sem multa; o plano continua ativo até o fim do período já pago.",
                            "El nuevo precio se aplica desde tu primera renovación después de esa fecha. Si no estás de acuerdo, puedes cancelar en cualquier momento en Configuración › Suscripción, sin penalización; el plan sigue activo hasta el final del periodo ya pagado.",
                        )
                        .into(),
                    ],
                    cta: Some((manage_label(l), app("/dashboard/settings#subscription"))),
                    ..Default::default()
                }
            }
            EmailTemplate::Welcome { username } => Content {
                subject: l
                    .pick(
                        "Welcome to Setlyst",
                        "Boas-vindas ao Setlyst",
                        "Te damos la bienvenida a Setlyst",
                    )
                    .into(),
                heading: l
                    .pick(
                        "Welcome to Setlyst, {u}",
                        "Boas-vindas ao Setlyst, {u}",
                        "Te damos la bienvenida a Setlyst, {u}",
                    )
                    .replace("{u}", username),
                paragraphs: vec![
                    l.pick(
                        "Your account is ready. With Setlyst you organize your repertoire, build setlists with key, tempo and song order, and use Live Mode on stage.",
                        "Sua conta está pronta. Com o Setlyst você organiza o repertório, monta setlists com tom, andamento e ordem das músicas e usa o Modo Ao Vivo no palco.",
                        "Tu cuenta está lista. Con Setlyst organizas tu repertorio, armas setlists con tono, tempo y orden de las canciones, y usas el Modo en vivo en el escenario.",
                    )
                    .into(),
                    l.pick(
                        "To get started, add a few songs and build your first setlist. If you play in a band, invite the other members to share the repertoire.",
                        "Para começar, cadastre algumas músicas e monte sua primeira setlist. Se você toca em banda, convide os outros integrantes para compartilhar o repertório.",
                        "Para empezar, agrega algunas canciones y arma tu primera setlist. Si tocas en una banda, invita a los demás integrantes a compartir el repertorio.",
                    )
                    .into(),
                ],
                cta: Some((
                    l.pick("Open the dashboard", "Abrir o painel", "Abrir el panel").into(),
                    app("/dashboard"),
                )),
                ..Default::default()
            },
            EmailTemplate::ReauthCode {
                username,
                code,
                expires_minutes,
            } => Content {
                subject: l
                    .pick(
                        "Your Setlyst confirmation code",
                        "Seu código de confirmação do Setlyst",
                        "Tu código de confirmación de Setlyst",
                    )
                    .into(),
                heading: l
                    .pick(
                        "Confirm it's you",
                        "Confirme que é você",
                        "Confirma que eres tú",
                    )
                    .into(),
                paragraphs: vec![
                    greeting(l, username),
                    l.pick(
                        "Use the code below to confirm a sensitive change in your Setlyst account. It is valid for {n} minutes.",
                        "Use o código abaixo para confirmar uma alteração sensível na sua conta do Setlyst. Ele é válido por {n} minutos.",
                        "Usa el código a continuación para confirmar un cambio sensible en tu cuenta de Setlyst. Es válido durante {n} minutos.",
                    )
                    .replace("{n}", &expires_minutes.to_string()),
                ],
                code: Some(code.clone()),
                note: Some(security_hint(l)),
                ..Default::default()
            },
            EmailTemplate::SecurityNotice {
                username,
                kind,
                detail,
            } => security_notice_content(l, username, kind, detail.as_deref(), &app),
        }
    }
}

/// An amount in minor units, as written in `l` (`R$ 39,90`).
fn money(l: Locale, cents: i64, currency: &str) -> String {
    let negative = cents < 0;
    let cents = cents.unsigned_abs();
    let (units, fraction) = (cents / 100, cents % 100);
    let (thousands, decimal) = match l {
        Locale::En => (',', '.'),
        Locale::PtBr | Locale::Es => ('.', ','),
    };
    let digits = units.to_string();
    let mut grouped = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(thousands);
        }
        grouped.push(digit);
    }
    let number = format!(
        "{}{grouped}{decimal}{fraction:02}",
        if negative { "-" } else { "" }
    );
    match (currency.to_ascii_uppercase().as_str(), l) {
        ("BRL", Locale::En) => format!("R${number}"),
        ("BRL", _) => format!("R$ {number}"),
        (other, _) => format!("{other} {number}"),
    }
}

/// `monthly` / `yearly` in words.
fn interval_label(l: Locale, interval: Option<&str>) -> &'static str {
    match interval {
        Some("yearly") => l.pick("yearly", "anual", "anual"),
        _ => l.pick("monthly", "mensal", "mensual"),
    }
}

/// "R$ 39,90 per month", when the amount is known.
fn price_text(
    l: Locale,
    cents: Option<i64>,
    currency: Option<&str>,
    interval: Option<&str>,
) -> Option<String> {
    let amount = money(l, cents?, currency.unwrap_or("BRL"));
    Some(match interval {
        Some("yearly") => l
            .pick("{v} per year", "{v} por ano", "{v} al año")
            .replace("{v}", &amount),
        Some(_) => l
            .pick("{v} per month", "{v} por mês", "{v} al mes")
            .replace("{v}", &amount),
        None => amount,
    })
}

/// The 7-day withdrawal right (CDC art. 49), as every billing e-mail
/// states it.
fn withdrawal_right(l: Locale) -> String {
    l.pick(
        "Right of withdrawal: you may withdraw within 7 days of a charge for a full refund, with the \"Withdraw from subscription\" button under Settings > Subscription or by replying to this email.",
        "Direito de arrependimento: você pode desistir em até 7 dias da cobrança, com reembolso integral, pelo botão \"Desistir da assinatura\" em Configurações › Assinatura ou respondendo a este e-mail.",
        "Derecho de desistimiento: puedes desistir dentro de los 7 días posteriores a un cobro con reembolso total, con el botón \"Desistir de la suscripción\" en Configuración › Suscripción o respondiendo a este correo.",
    )
    .into()
}

fn manage_label(l: Locale) -> String {
    l.pick(
        "Manage my subscription",
        "Gerenciar minha assinatura",
        "Gestionar mi suscripción",
    )
    .into()
}

fn greeting(l: Locale, username: &str) -> String {
    l.pick("Hello, {u}.", "Olá, {u}.", "Hola, {u}.")
        .replace("{u}", username)
}

fn release_kind_label(l: Locale, kind: &str) -> &'static str {
    match kind {
        "new" => l.pick("New", "Novo", "Nuevo"),
        "improved" => l.pick("Improved", "Melhoria", "Mejora"),
        "fixed" => l.pick("Fixed", "Correção", "Corrección"),
        "security" => l.pick("Security", "Segurança", "Seguridad"),
        _ => l.pick("Update", "Atualização", "Actualización"),
    }
}

/// Inputs shared by every rendering.
#[derive(Debug, Clone)]
pub struct RenderContext {
    /// Public web origin without a trailing slash.
    pub app_base_url: String,
    pub locale: Locale,
    /// Present on messages the recipient can unsubscribe from.
    pub unsubscribe_url: Option<String>,
}

impl RenderContext {
    /// `{base}/{locale}{path}`.
    pub fn app_url(&self, path: &str) -> String {
        format!(
            "{}/{}{}",
            self.app_base_url.trim_end_matches('/'),
            self.locale.code(),
            path
        )
    }
}

/// A rendered message.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedEmail {
    pub subject: String,
    pub html: String,
    pub text: String,
    /// The unsubscribe link shown in the footer, when the message has one
    /// (the worker turns it into `List-Unsubscribe` headers).
    pub unsubscribe_url: Option<String>,
}

/// Content blocks shared by the HTML and text renderings. Plain text:
/// escaped when rendered as HTML.
#[derive(Debug, Clone, Default)]
struct Content {
    subject: String,
    heading: String,
    paragraphs: Vec<String>,
    /// A one-time code, shown large.
    code: Option<String>,
    items: Vec<String>,
    /// `(label, absolute URL)`.
    cta: Option<(String, String)>,
    note: Option<String>,
}

struct Footer {
    reason: String,
    unsubscribe: Option<(String, String, String)>,
    terms: (String, String),
    privacy: (String, String),
}

fn footer(ctx: &RenderContext, reason: FooterReason) -> Footer {
    let l = ctx.locale;
    let reason_text = match reason {
        FooterReason::Address => l
            .pick(
                "You are receiving this email because this address was entered on a Setlyst account.",
                "Você recebe este e-mail porque este endereço foi informado em uma conta do Setlyst.",
                "Recibes este correo porque esta dirección se indicó en una cuenta de Setlyst.",
            )
            .to_string(),
        FooterReason::Account => l
            .pick(
                "You are receiving this email because you have a Setlyst account.",
                "Você recebe este e-mail porque tem uma conta no Setlyst.",
                "Recibes este correo porque tienes una cuenta en Setlyst.",
            )
            .to_string(),
        FooterReason::Security => l
            .pick(
                "You are receiving this email because it concerns the security of your Setlyst account. Security notices are always sent.",
                "Você recebe este e-mail porque ele trata da segurança da sua conta no Setlyst. Avisos de segurança são sempre enviados.",
                "Recibes este correo porque trata sobre la seguridad de tu cuenta en Setlyst. Los avisos de seguridad siempre se envían.",
            )
            .to_string(),
        FooterReason::Deleted => l
            .pick(
                "You are receiving this email because your Setlyst account was deleted.",
                "Você recebe este e-mail porque a sua conta no Setlyst foi excluída.",
                "Recibes este correo porque tu cuenta en Setlyst fue eliminada.",
            )
            .to_string(),
        FooterReason::Critical => l
            .pick(
                "You are receiving this email because it is an important notice about the Setlyst service.",
                "Você recebe este e-mail porque ele é um aviso importante sobre o serviço Setlyst.",
                "Recibes este correo porque es un aviso importante sobre el servicio Setlyst.",
            )
            .to_string(),
        FooterReason::Billing => l
            .pick(
                "You are receiving this email because it concerns your Setlyst subscription. Billing notices are always sent.",
                "Você recebe este e-mail porque ele trata da sua assinatura no Setlyst. Avisos de cobrança são sempre enviados.",
                "Recibes este correo porque trata sobre tu suscripción en Setlyst. Los avisos de cobro siempre se envían.",
            )
            .to_string(),
        FooterReason::Preference(category) => {
            let name = match category {
                Category::Account => l.pick("account", "conta", "cuenta"),
                Category::Bands => l.pick("band", "bandas", "bandas"),
                Category::Announcements => l.pick("announcement", "avisos", "avisos"),
                Category::ProductUpdates => l.pick("product update", "novidades", "novedades"),
                Category::Marketing => l.pick("offer", "ofertas", "ofertas"),
                Category::Security => l.pick("security", "segurança", "seguridad"),
            };
            l.pick(
                "You are receiving this email because you turned on {c} emails in your communication preferences.",
                "Você recebe este e-mail porque ativou os e-mails de {c} nas preferências de comunicação.",
                "Recibes este correo porque activaste los correos de {c} en tus preferencias de comunicación.",
            )
            .replace("{c}", name)
        }
    };
    Footer {
        reason: reason_text,
        unsubscribe: ctx.unsubscribe_url.as_ref().map(|url| {
            (
                l.pick(
                    "Don't want these emails?",
                    "Não quer mais receber estes e-mails?",
                    "¿No quieres recibir estos correos?",
                )
                .to_string(),
                l.pick("Unsubscribe", "Cancelar inscrição", "Cancelar suscripción")
                    .to_string(),
                url.clone(),
            )
        }),
        terms: (
            l.pick("Terms of Use", "Termos de Uso", "Términos de Uso")
                .into(),
            ctx.app_url("/legal/terms"),
        ),
        privacy: (
            l.pick(
                "Privacy Policy",
                "Política de Privacidade",
                "Política de Privacidad",
            )
            .into(),
            ctx.app_url("/legal/privacy"),
        ),
    }
}

/// Escapes text for HTML content and attribute values.
pub fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// A subject line: one line, bounded.
fn clean_subject(subject: &str) -> String {
    let single_line: String = subject
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = single_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() <= MAX_SUBJECT_CHARS {
        trimmed
    } else {
        let cut: String = trimmed.chars().take(MAX_SUBJECT_CHARS - 3).collect();
        format!("{}...", cut.trim_end())
    }
}

/// Escaped text with line breaks preserved.
fn escape_multiline(value: &str) -> String {
    escape_html(value).replace('\n', "<br>")
}

fn render_html(ctx: &RenderContext, subject: &str, content: &Content, footer: &Footer) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        r#"<h1 style="margin:0 0 16px;font-size:21px;line-height:1.3;color:{TEXT};">{}</h1>"#,
        escape_html(&content.heading)
    ));
    for paragraph in &content.paragraphs {
        body.push_str(&format!(
            r#"<p style="margin:0 0 14px;">{}</p>"#,
            escape_multiline(paragraph)
        ));
    }
    if let Some(code) = &content.code {
        body.push_str(&format!(
            r#"<p style="margin:22px 0;text-align:center;"><span style="display:inline-block;padding:12px 22px;font-family:Menlo,Consolas,'Courier New',monospace;font-size:28px;font-weight:700;letter-spacing:6px;color:{TEXT};background:{BACKGROUND};border-radius:8px;">{}</span></p>"#,
            escape_html(code)
        ));
    }
    if !content.items.is_empty() {
        body.push_str(r#"<ul style="margin:0 0 14px;padding-left:20px;">"#);
        for item in &content.items {
            body.push_str(&format!(
                r#"<li style="margin:0 0 8px;">{}</li>"#,
                escape_html(item)
            ));
        }
        body.push_str("</ul>");
    }
    if let Some((label, url)) = &content.cta {
        body.push_str(&format!(
            r#"<table role="presentation" cellpadding="0" cellspacing="0" border="0" style="margin:22px 0;"><tr><td style="border-radius:8px;background:{BRAND};"><a href="{}" style="display:inline-block;padding:12px 22px;font-family:{FONT};font-size:15px;font-weight:600;color:#ffffff;text-decoration:none;border-radius:8px;">{}</a></td></tr></table>"#,
            escape_html(url),
            escape_html(label)
        ));
    }
    if let Some(note) = &content.note {
        body.push_str(&format!(
            r#"<p style="margin:18px 0 0;font-size:13px;color:{MUTED};">{}</p>"#,
            escape_multiline(note)
        ));
    }

    let mut foot = format!(
        r#"<p style="margin:0 0 8px;">{}</p>"#,
        escape_html(&footer.reason)
    );
    if let Some((text, label, url)) = &footer.unsubscribe {
        foot.push_str(&format!(
            r#"<p style="margin:0 0 8px;">{} <a href="{}" style="color:{BRAND};">{}</a></p>"#,
            escape_html(text),
            escape_html(url),
            escape_html(label)
        ));
    }
    foot.push_str(&format!(
        r#"<p style="margin:0;"><a href="{}" style="color:{BRAND};">{}</a> &middot; <a href="{}" style="color:{BRAND};">{}</a></p>"#,
        escape_html(&footer.terms.1),
        escape_html(&footer.terms.0),
        escape_html(&footer.privacy.1),
        escape_html(&footer.privacy.0)
    ));

    let preheader = content
        .paragraphs
        .iter()
        .find(|p| !p.is_empty())
        .cloned()
        .unwrap_or_default();

    format!(
        r#"<!DOCTYPE html>
<html lang="{lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light">
<title>{title}</title>
</head>
<body style="margin:0;padding:0;background:{BACKGROUND};">
<div style="display:none;max-height:0;overflow:hidden;opacity:0;">{preheader}</div>
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="background:{BACKGROUND};">
<tr><td align="center" style="padding:24px 12px;">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="max-width:560px;background:#ffffff;border:1px solid #e6e1f5;border-radius:12px;">
<tr><td style="padding:20px 28px;border-bottom:3px solid {BRAND};font-family:{FONT};font-size:20px;font-weight:700;color:{BRAND};">Setlyst</td></tr>
<tr><td style="padding:28px;font-family:{FONT};font-size:15px;line-height:1.6;color:{TEXT};">{body}</td></tr>
<tr><td style="padding:18px 28px;border-top:1px solid #eeeaf7;font-family:{FONT};font-size:12px;line-height:1.5;color:#6b6680;">{foot}</td></tr>
</table>
</td></tr>
</table>
</body>
</html>"#,
        lang = ctx.locale.code(),
        title = escape_html(subject),
        preheader = escape_html(&preheader),
    )
}

fn render_text(content: &Content, footer: &Footer) -> String {
    let mut out = String::new();
    out.push_str(&content.heading);
    out.push_str("\n\n");
    for paragraph in &content.paragraphs {
        out.push_str(paragraph);
        out.push_str("\n\n");
    }
    if let Some(code) = &content.code {
        out.push_str(&format!("    {code}\n\n"));
    }
    for item in &content.items {
        out.push_str(&format!("- {item}\n"));
    }
    if !content.items.is_empty() {
        out.push('\n');
    }
    if let Some((label, url)) = &content.cta {
        out.push_str(&format!("{label}: {url}\n\n"));
    }
    if let Some(note) = &content.note {
        out.push_str(note);
        out.push_str("\n\n");
    }
    out.push_str("--\n");
    out.push_str(&footer.reason);
    out.push('\n');
    if let Some((text, label, url)) = &footer.unsubscribe {
        out.push_str(&format!("{text} {label}: {url}\n"));
    }
    out.push_str(&format!("{}: {}\n", footer.terms.0, footer.terms.1));
    out.push_str(&format!("{}: {}\n", footer.privacy.0, footer.privacy.1));
    out
}

/// "Wasn't you?" advice closing every security message.
fn security_hint(l: Locale) -> String {
    l.pick(
        "If this was not you, secure your account now: change your password (or reset it from the sign-in page), sign out everywhere and turn on two-step verification in the security settings.",
        "Se não foi você, proteja sua conta agora: altere sua senha (ou redefina pela página de acesso), encerre todas as sessões e ative a verificação em duas etapas nas configurações de segurança.",
        "Si no fuiste tú, protege tu cuenta ahora: cambia tu contraseña (o restablécela desde la página de acceso), cierra todas las sesiones y activa la verificación en dos pasos en la configuración de seguridad.",
    )
    .into()
}

/// Content of [`EmailTemplate::SecurityNotice`].
fn security_notice_content(
    l: Locale,
    username: &str,
    kind: &str,
    detail: Option<&str>,
    app: &dyn Fn(&str) -> String,
) -> Content {
    let detail = detail.unwrap_or("");
    let fill = |text: &str| text.replace("{u}", username).replace("{d}", detail);
    let (subject, heading, body) = match kind {
        "email_change_started" => (
            l.pick(
                "A change of your Setlyst email was requested",
                "Foi solicitada a troca do seu e-mail no Setlyst",
                "Se solicitó cambiar tu correo en Setlyst",
            ),
            l.pick("Email change requested", "Troca de e-mail solicitada", "Cambio de correo solicitado"),
            l.pick(
                "Someone signed in to the account {u} asked to change its email address to {d}. The change only happens once the code sent to the new address is confirmed.",
                "Alguém conectado à conta {u} pediu para trocar o endereço de e-mail para {d}. A troca só acontece depois que o código enviado ao novo endereço for confirmado.",
                "Alguien con sesión en la cuenta {u} pidió cambiar su dirección de correo a {d}. El cambio solo ocurre cuando se confirme el código enviado a la nueva dirección.",
            ),
        ),
        "email_in_use" => (
            l.pick(
                "Someone tried to use your email on Setlyst",
                "Alguém tentou usar seu e-mail no Setlyst",
                "Alguien intentó usar tu correo en Setlyst",
            ),
            l.pick("Your email is already in use", "Seu e-mail já está em uso", "Tu correo ya está en uso"),
            l.pick(
                "Someone asked to use this email address for another Setlyst account ({u}). It already belongs to an account, so nothing was changed.",
                "Alguém pediu para usar este endereço de e-mail em outra conta do Setlyst ({u}). Ele já pertence a uma conta, então nada foi alterado.",
                "Alguien pidió usar esta dirección de correo en otra cuenta de Setlyst ({u}). Ya pertenece a una cuenta, así que no se cambió nada.",
            ),
        ),
        "google_linked" => (
            l.pick(
                "Google sign-in was linked to your Setlyst account",
                "O acesso com Google foi vinculado à sua conta do Setlyst",
                "Se vinculó el acceso con Google a tu cuenta de Setlyst",
            ),
            l.pick("Google account linked", "Conta Google vinculada", "Cuenta de Google vinculada"),
            l.pick(
                "The Google account {d} can now be used to sign in to the Setlyst account {u}.",
                "A conta Google {d} agora pode ser usada para entrar na conta {u} do Setlyst.",
                "La cuenta de Google {d} ahora puede usarse para iniciar sesión en la cuenta {u} de Setlyst.",
            ),
        ),
        "google_unlinked" => (
            l.pick(
                "Google sign-in was removed from your Setlyst account",
                "O acesso com Google foi removido da sua conta do Setlyst",
                "Se quitó el acceso con Google de tu cuenta de Setlyst",
            ),
            l.pick("Google account unlinked", "Conta Google desvinculada", "Cuenta de Google desvinculada"),
            l.pick(
                "Google can no longer be used to sign in to the Setlyst account {u}.",
                "O Google não pode mais ser usado para entrar na conta {u} do Setlyst.",
                "Google ya no puede usarse para iniciar sesión en la cuenta {u} de Setlyst.",
            ),
        ),
        "password_reset_by_staff" => (
            l.pick(
                "Setlyst support set a new password for your account",
                "O suporte do Setlyst definiu uma nova senha para sua conta",
                "El soporte de Setlyst definió una nueva contraseña para tu cuenta",
            ),
            l.pick("Password reset by support", "Senha redefinida pelo suporte", "Contraseña restablecida por soporte"),
            l.pick(
                "A member of the Setlyst team set a temporary password for the account {u} and signed out every session. Sign in with the password you received from support and choose a new one.",
                "Um membro da equipe do Setlyst definiu uma senha temporária para a conta {u} e encerrou todas as sessões. Entre com a senha que você recebeu do suporte e escolha uma nova.",
                "Un miembro del equipo de Setlyst definió una contraseña temporal para la cuenta {u} y cerró todas las sesiones. Inicia sesión con la contraseña que recibiste del soporte y elige una nueva.",
            ),
        ),
        "email_changed_by_staff" => (
            l.pick(
                "Setlyst support changed your account email",
                "O suporte do Setlyst alterou o e-mail da sua conta",
                "El soporte de Setlyst cambió el correo de tu cuenta",
            ),
            l.pick("Email changed by support", "E-mail alterado pelo suporte", "Correo cambiado por soporte"),
            l.pick(
                "A member of the Setlyst team changed the email address of the account {u} to {d}. Messages about the account now go to the new address.",
                "Um membro da equipe do Setlyst alterou o endereço de e-mail da conta {u} para {d}. As mensagens sobre a conta agora vão para o novo endereço.",
                "Un miembro del equipo de Setlyst cambió la dirección de correo de la cuenta {u} a {d}. Los mensajes sobre la cuenta ahora van a la nueva dirección.",
            ),
        ),
        "login_locked" => (
            l.pick(
                "Sign-in attempts to your Setlyst account were blocked",
                "Tentativas de acesso à sua conta do Setlyst foram bloqueadas",
                "Se bloquearon intentos de acceso a tu cuenta de Setlyst",
            ),
            l.pick("Sign-in attempts blocked", "Tentativas de acesso bloqueadas", "Intentos de acceso bloqueados"),
            l.pick(
                "There were several failed attempts to sign in to the account {u}, so further attempts were blocked for a while. Your password was not changed. If you are locked out, reset your password from the sign-in page: that lifts the block.",
                "Houve várias tentativas de acesso sem sucesso à conta {u}, então novas tentativas foram bloqueadas por um tempo. Sua senha não foi alterada. Se você não conseguir entrar, redefina sua senha pela página de acesso: isso remove o bloqueio.",
                "Hubo varios intentos fallidos de iniciar sesión en la cuenta {u}, así que se bloquearon nuevos intentos por un tiempo. Tu contraseña no se cambió. Si no puedes entrar, restablece tu contraseña desde la página de acceso: eso quita el bloqueo.",
            ),
        ),
        _ => (
            l.pick(
                "Your Setlyst account was signed out everywhere",
                "Sua conta do Setlyst foi desconectada de todos os dispositivos",
                "Se cerró la sesión de tu cuenta de Setlyst en todas partes",
            ),
            l.pick("Signed out everywhere", "Sessões encerradas", "Sesiones cerradas"),
            l.pick(
                "Someone signed in to the account {u} typed the wrong password or confirmation code too many times, so every session was signed out as a precaution.",
                "Alguém conectado à conta {u} digitou a senha ou o código de confirmação errado muitas vezes, então todas as sessões foram encerradas por precaução.",
                "Alguien con sesión en la cuenta {u} escribió la contraseña o el código de confirmación incorrecto demasiadas veces, así que se cerraron todas las sesiones por precaución.",
            ),
        ),
    };
    let cta = (kind != "email_in_use").then(|| {
        (
            l.pick(
                "Review your security settings",
                "Revisar as configurações de segurança",
                "Revisar la configuración de seguridad",
            )
            .to_string(),
            app("/dashboard/settings?section=security"),
        )
    });
    Content {
        subject: subject.into(),
        heading: heading.into(),
        paragraphs: vec![fill(body)],
        cta,
        note: Some(if kind == "email_in_use" {
            l.pick(
                "If this was you, sign in to the account that already uses this address instead. If not, you can ignore this message.",
                "Se foi você, entre na conta que já usa este endereço. Se não foi, pode ignorar esta mensagem.",
                "Si fuiste tú, inicia sesión en la cuenta que ya usa esta dirección. Si no, puedes ignorar este mensaje.",
            )
            .into()
        } else {
            security_hint(l)
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use serde_json::json;

    fn ctx(locale: Locale, unsubscribe: bool) -> RenderContext {
        RenderContext {
            app_base_url: "https://setlyst.app".into(),
            locale,
            unsubscribe_url: unsubscribe
                .then(|| "https://setlyst.app/en/unsubscribe?token=abc".into()),
        }
    }

    fn samples() -> Vec<EmailTemplate> {
        let when = NaiveDate::from_ymd_opt(2026, 10, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        vec![
            EmailTemplate::EmailVerificationCode {
                username: "ana".into(),
                code: "123456".into(),
                expires_minutes: 15,
            },
            EmailTemplate::PasswordResetCode {
                username: "ana".into(),
                code: "654321".into(),
                expires_minutes: 15,
            },
            EmailTemplate::EmailChangeCode {
                username: "ana".into(),
                code: "111222".into(),
                expires_minutes: 15,
            },
            EmailTemplate::PasswordChanged {
                username: "ana".into(),
            },
            EmailTemplate::TwoFactorEnabled {
                username: "ana".into(),
            },
            EmailTemplate::TwoFactorDisabled {
                username: "ana".into(),
            },
            EmailTemplate::EmailChangedNotice {
                username: "ana".into(),
                new_email_masked: "a***@example.com".into(),
            },
            EmailTemplate::AccountDeleted {
                username: "ana".into(),
            },
            EmailTemplate::Notification {
                title: "Title".into(),
                lines: vec!["Line".into()],
                cta_label: None,
                cta_path: Some("/dashboard".into()),
                category: Category::Bands,
            },
            EmailTemplate::Announcement {
                title: "Maintenance".into(),
                body: "First.\n\nSecond.".into(),
                level: "info".into(),
                cta_label: Some("Read".into()),
                cta_url: Some("/dashboard/announcements".into()),
            },
            EmailTemplate::ReleaseNotes {
                version: "0.12.0".into(),
                title: json!({ "en": "Big release", "pt-BR": "Grande versão" }),
                items: vec![ReleaseItem {
                    kind: "new".into(),
                    text: json!({ "en": "Tours", "pt-BR": "Turnês", "es": "Giras" }),
                }],
                released_on: NaiveDate::from_ymd_opt(2026, 9, 23).unwrap(),
            },
            EmailTemplate::TrialEnding {
                username: "ana".into(),
                plan_name: json!({ "en": "Pro", "pt-BR": "Pro", "es": "Pro" }),
                ends_at: when,
            },
            EmailTemplate::SubscriptionChanged {
                username: "ana".into(),
                kind: "plan_granted".into(),
                plan_name: json!({ "en": "Pro" }),
                current_period_end: Some(when),
            },
            EmailTemplate::Welcome {
                username: "ana".into(),
            },
        ]
    }

    #[test]
    fn every_template_renders_in_every_locale() {
        for template in samples() {
            for locale in Locale::ALL {
                let rendered =
                    template.render(&ctx(locale, template.unsubscribe_category().is_some()));
                assert!(!rendered.subject.is_empty(), "{}", template.name());
                assert!(rendered.subject.chars().count() <= MAX_SUBJECT_CHARS);
                assert!(rendered.html.contains("Setlyst"));
                assert!(
                    rendered
                        .html
                        .contains(&format!("lang=\"{}\"", locale.code()))
                );
                assert!(rendered.html.contains("/legal/terms"));
                assert!(rendered.text.contains("/legal/privacy"));
                assert!(!rendered.html.contains('—') && !rendered.text.contains('—'));
                assert!(!rendered.html.contains("<img"), "no remote images");
            }
        }
    }

    #[test]
    fn payment_events_have_their_own_wording() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 23)
            .unwrap()
            .and_hms_opt(12, 0, 0);
        let generic = EmailTemplate::SubscriptionChanged {
            username: "ana".into(),
            kind: "plan_granted".into(),
            plan_name: json!({"en": "Pro", "pt-BR": "Pro", "es": "Pro"}),
            current_period_end: date,
        }
        .render(&ctx(Locale::PtBr, false))
        .text;
        let mut seen = Vec::new();
        for kind in [
            "subscribed",
            "resumed",
            "plan_changed",
            "payment_failed",
            "cancel_scheduled",
            "canceled",
        ] {
            let text = EmailTemplate::SubscriptionChanged {
                username: "ana".into(),
                kind: kind.into(),
                plan_name: json!({"en": "Pro", "pt-BR": "Pro", "es": "Pro"}),
                current_period_end: date,
            }
            .render(&ctx(Locale::PtBr, false))
            .text;
            assert!(!text.contains("{p}") && !text.contains("{d}"), "{kind}");
            assert!(text.contains("Pro"), "{kind}");
            assert_ne!(text, generic, "{kind} falls back to the generic line");
            seen.push(text);
        }
        assert!(seen[3].contains("forma de pagamento"));
        assert!(seen[4].contains("não haverá novas cobranças"));
    }

    fn billing_samples() -> Vec<EmailTemplate> {
        let when = NaiveDate::from_ymd_opt(2026, 10, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let pro = json!({ "en": "Pro", "pt-BR": "Pro", "es": "Pro" });
        vec![
            EmailTemplate::SubscriptionConfirmed {
                username: "ana".into(),
                plan_name: pro.clone(),
                amount_cents: Some(3990),
                currency: Some("BRL".into()),
                interval: Some("monthly".into()),
                next_charge_at: Some(when),
                trial_ends_at: Some(when),
                terms_version: Some("2026-09-24".into()),
            },
            EmailTemplate::PaidTrialEnding {
                username: "ana".into(),
                plan_name: pro.clone(),
                amount_cents: Some(39900),
                currency: Some("BRL".into()),
                interval: Some("yearly".into()),
                charge_at: when,
            },
            EmailTemplate::RenewalReminder {
                username: "ana".into(),
                plan_name: pro.clone(),
                amount_cents: Some(39900),
                currency: Some("BRL".into()),
                renews_at: when,
            },
            EmailTemplate::WithdrawalConfirmed {
                username: "ana".into(),
                plan_name: pro.clone(),
                refunded_cents: 3990,
                currency: "brl".into(),
                requested_at: when,
                by_staff: false,
            },
            EmailTemplate::PaymentDisputed {
                username: "ana".into(),
                plan_name: pro.clone(),
            },
            EmailTemplate::PriceChange {
                username: "ana".into(),
                plan_name: pro,
                interval: Some("monthly".into()),
                old_amount_cents: Some(3990),
                new_amount_cents: 4490,
                currency: Some("BRL".into()),
                effective_at: when,
            },
        ]
    }

    #[test]
    fn billing_templates_render_and_are_always_sent() {
        for template in billing_samples() {
            assert_eq!(template.unsubscribe_category(), None, "{}", template.name());
            let (name, payload) = template.to_parts();
            assert_eq!(
                EmailTemplate::from_parts(name, &payload),
                Some(template.clone())
            );
            for locale in Locale::ALL {
                let rendered = template.render(&ctx(locale, false));
                assert!(rendered.subject.chars().count() <= MAX_SUBJECT_CHARS);
                assert!(!rendered.html.contains('—') && !rendered.text.contains('—'));
                assert!(
                    !rendered.text.contains('{'),
                    "{}: {}",
                    template.name(),
                    rendered.text
                );
                assert!(rendered.text.contains("/legal/terms"));
            }
            let pt = template.render(&ctx(Locale::PtBr, false)).text;
            assert!(
                pt.contains("Avisos de cobrança são sempre enviados"),
                "{pt}"
            );
        }
        // The contract confirmation states price, terms, cancellation and
        // the withdrawal right.
        let confirmed = billing_samples()[0].render(&ctx(Locale::PtBr, false)).text;
        assert!(confirmed.contains("R$ 39,90 por mês"), "{confirmed}");
        assert!(confirmed.contains("/pt-BR/legal/subscription"));
        assert!(confirmed.contains("versão 2026-09-24"));
        assert!(confirmed.contains("7 dias"));
        assert!(confirmed.contains("cancelar quando quiser"));
        let reminder = billing_samples()[1].render(&ctx(Locale::PtBr, false)).text;
        assert!(reminder.contains("R$ 399,00 por ano"), "{reminder}");
        assert!(reminder.contains("01/10/2026"));
        // Paid-subscription notices can't be unsubscribed from; plan
        // grants still can.
        let failed = EmailTemplate::SubscriptionChanged {
            username: "ana".into(),
            kind: "payment_failed".into(),
            plan_name: json!({"en": "Pro"}),
            current_period_end: None,
        };
        assert_eq!(failed.unsubscribe_category(), None);
        let granted = EmailTemplate::SubscriptionChanged {
            username: "ana".into(),
            kind: "plan_granted".into(),
            plan_name: json!({"en": "Pro"}),
            current_period_end: None,
        };
        assert_eq!(granted.unsubscribe_category(), Some(Category::Account));
    }

    #[test]
    fn money_is_written_per_locale() {
        assert_eq!(money(Locale::PtBr, 3990, "BRL"), "R$ 39,90");
        assert_eq!(money(Locale::En, 123_456, "brl"), "R$1,234.56");
        assert_eq!(money(Locale::Es, 5, "USD"), "USD 0,05");
    }

    #[test]
    fn templates_round_trip_through_the_outbox_columns() {
        for template in samples() {
            let (name, payload) = template.to_parts();
            assert_eq!(EmailTemplate::from_parts(name, &payload), Some(template));
        }
        assert!(EmailTemplate::from_parts("nope", &json!({})).is_none());
    }

    #[test]
    fn variables_are_html_escaped() {
        let template = EmailTemplate::Notification {
            title: "<script>alert(1)</script>".into(),
            lines: vec!["Tom & \"Jerry\" <b>".into()],
            cta_label: Some("Go\"><x>".into()),
            cta_path: Some("/x\"onmouseover=\"y".into()),
            category: Category::Bands,
        };
        let rendered = template.render(&ctx(Locale::En, true));
        assert!(!rendered.html.contains("<script>"));
        assert!(
            rendered
                .html
                .contains("&lt;script&gt;alert(1)&lt;/script&gt;")
        );
        assert!(
            rendered
                .html
                .contains("Tom &amp; &quot;Jerry&quot; &lt;b&gt;")
        );
        assert!(!rendered.html.contains("\"onmouseover=\""));
        assert!(!rendered.html.contains("<x>"));
        // The plain-text part is not escaped.
        assert!(rendered.text.contains("Tom & \"Jerry\" <b>"));
    }

    #[test]
    fn unsubscribe_links_only_where_expected() {
        let expected_unsubscribable = [
            "notification",
            "announcement",
            "release_notes",
            "trial_ending",
            "subscription_changed",
        ];
        for template in samples() {
            let unsubscribable = template.unsubscribe_category().is_some();
            assert_eq!(
                unsubscribable,
                expected_unsubscribable.contains(&template.name()),
                "{}",
                template.name()
            );
            let rendered = template.render(&ctx(Locale::PtBr, unsubscribable));
            assert_eq!(rendered.html.contains("Cancelar inscrição"), unsubscribable);
        }
        let critical = EmailTemplate::Announcement {
            title: "Outage".into(),
            body: "Body".into(),
            level: "critical".into(),
            cta_label: None,
            cta_url: None,
        };
        assert!(critical.unsubscribe_category().is_none());
        let security_notification = EmailTemplate::Notification {
            title: "t".into(),
            lines: vec![],
            cta_label: None,
            cta_path: None,
            category: Category::Security,
        };
        assert!(security_notification.unsubscribe_category().is_none());
    }

    #[test]
    fn subjects_are_single_line_and_bounded() {
        assert_eq!(clean_subject("a\r\nBcc: x"), "a Bcc: x");
        let long = clean_subject(&"word ".repeat(60));
        assert!(long.chars().count() <= MAX_SUBJECT_CHARS);
        assert!(long.ends_with("..."));
    }

    #[test]
    fn codes_and_links_are_present() {
        let rendered = samples()[0].render(&ctx(Locale::PtBr, false));
        assert!(rendered.html.contains("123456"));
        assert!(rendered.text.contains("123456"));
        assert!(rendered.subject.contains("Confirme"));
        let welcome = samples()[13].render(&ctx(Locale::Es, false));
        assert!(welcome.html.contains("https://setlyst.app/es/dashboard"));
        let announcement = samples()[9].render(&ctx(Locale::En, true));
        assert!(
            announcement
                .html
                .contains("https://setlyst.app/en/dashboard/announcements")
        );
        assert_eq!(localized(&json!({ "en": "A" }), Locale::Es), "A");
    }

    #[test]
    fn account_security_templates_render_in_every_locale() {
        let mut templates = vec![EmailTemplate::ReauthCode {
            username: "ana".into(),
            code: "246810".into(),
            expires_minutes: 10,
        }];
        for kind in [
            "email_change_started",
            "email_in_use",
            "google_linked",
            "google_unlinked",
            "password_reset_by_staff",
            "email_changed_by_staff",
            "login_locked",
            "reauth_sessions_revoked",
        ] {
            templates.push(EmailTemplate::SecurityNotice {
                username: "ana".into(),
                kind: kind.into(),
                detail: Some("n***@example.com".into()),
            });
        }
        for template in &templates {
            assert!(
                template.unsubscribe_category().is_none(),
                "{}",
                template.name()
            );
            let parts = template.to_parts();
            assert_eq!(
                EmailTemplate::from_parts(parts.0, &parts.1).as_ref(),
                Some(template)
            );
            for locale in Locale::ALL {
                let rendered = template.render(&ctx(locale, false));
                assert!(!rendered.subject.is_empty());
                assert!(rendered.subject.chars().count() <= MAX_SUBJECT_CHARS);
                assert!(!rendered.text.contains("{u}") && !rendered.text.contains("{d}"));
                assert!(!rendered.html.contains('—') && !rendered.text.contains('—'));
            }
        }
        assert!(templates[0].is_sensitive());
        let started = templates[1].render(&ctx(Locale::PtBr, false));
        assert!(started.text.contains("n***@example.com"));
        assert!(
            started
                .text
                .contains("/pt-BR/dashboard/settings?section=security")
        );
    }
}
