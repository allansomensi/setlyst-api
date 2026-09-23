//! Transactional e-mail templates, rendered in English, Brazilian
//! Portuguese and Spanish.
//!
//! Every template produces a subject, an HTML body and a plain-text body
//! from the same content blocks, so both parts always say the same thing.
//! The HTML is deliberately conservative (table layout, inline styles, no
//! remote images or fonts) because e-mail clients strip most CSS and block
//! external resources. Every variable is HTML-escaped; only the fixed copy
//! below is trusted.

use crate::models::communication::Category;
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
    Welcome {
        username: String,
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
            EmailTemplate::Welcome { .. } => "welcome",
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
            EmailTemplate::TrialEnding { .. } | EmailTemplate::SubscriptionChanged { .. } => {
                FooterReason::Preference(Category::Account)
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
        }
    }
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
}
