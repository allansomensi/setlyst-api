//! Human-readable text of in-app notifications, for their e-mail copies.
//!
//! In the app, the web client renders notifications from `type` + `data`
//! with its own translations. E-mails can't do that, so the same messages
//! are written here in the three supported languages.

use super::templates::Locale;
use crate::models::notification::NotificationType;
use serde_json::Value;

/// Title, body lines and the app path the e-mail's button opens.
#[derive(Debug, Clone, PartialEq)]
pub struct NotificationText {
    pub title: String,
    pub lines: Vec<String>,
    pub cta_path: Option<String>,
}

fn s<'a>(data: &'a Value, key: &str) -> &'a str {
    data.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn band_role(l: Locale, role: &str) -> String {
    match role {
        "owner" => l.pick("Owner", "Dono", "Propietario"),
        "admin" => l.pick("Admin", "Administrador", "Administrador"),
        "moderator" => l.pick("Moderator", "Moderador", "Moderador"),
        "member" => l.pick("Member", "Integrante", "Integrante"),
        other => other,
    }
    .to_string()
}

fn platform_role(l: Locale, role: &str) -> String {
    match role {
        "admin" => l.pick("Administrator", "Administrador", "Administrador"),
        "moderator" => l.pick("Moderator", "Moderador", "Moderador"),
        "user" => l.pick("User", "Usuário", "Usuario"),
        other => other,
    }
    .to_string()
}

fn band_path(data: &Value) -> Option<String> {
    data.get("band_id")
        .and_then(Value::as_str)
        .map(|id| format!("/dashboard/bands/{id}"))
}

/// The e-mail text of a notification of `kind` with `data`.
pub fn describe(kind: NotificationType, data: &Value, l: Locale) -> NotificationText {
    let band = s(data, "band_name");
    match kind {
        NotificationType::BandRoleChanged => NotificationText {
            title: l
                .pick(
                    "Your role in {b} changed",
                    "Seu papel na banda {b} mudou",
                    "Tu rol en la banda {b} cambió",
                )
                .replace("{b}", band),
            lines: vec![l
                .pick(
                    "Your role in the band {b} changed from {o} to {n}.",
                    "Seu papel na banda {b} passou de {o} para {n}.",
                    "Tu rol en la banda {b} pasó de {o} a {n}.",
                )
                .replace("{b}", band)
                .replace("{o}", &band_role(l, s(data, "old_role")))
                .replace("{n}", &band_role(l, s(data, "new_role")))],
            cta_path: band_path(data),
        },
        NotificationType::BandMemberRemoved => NotificationText {
            title: l
                .pick(
                    "You were removed from the band {b}",
                    "Você foi removido da banda {b}",
                    "Te retiraron de la banda {b}",
                )
                .replace("{b}", band),
            lines: vec![l
                .pick(
                    "You are no longer a member of {b}. The personal songs and setlists of your account were not affected.",
                    "Você não faz mais parte da banda {b}. As músicas e setlists pessoais da sua conta não foram afetadas.",
                    "Ya no formas parte de la banda {b}. Las canciones y setlists personales de tu cuenta no se vieron afectadas.",
                )
                .replace("{b}", band)],
            cta_path: Some("/dashboard/bands".into()),
        },
        NotificationType::BandMemberAdded => NotificationText {
            title: l
                .pick(
                    "You were added to the band {b}",
                    "Você foi adicionado à banda {b}",
                    "Te agregaron a la banda {b}",
                )
                .replace("{b}", band),
            lines: vec![l
                .pick(
                    "The Setlyst team added you to the band {b} as {r}.",
                    "A equipe do Setlyst adicionou você à banda {b} como {r}.",
                    "El equipo de Setlyst te agregó a la banda {b} como {r}.",
                )
                .replace("{b}", band)
                .replace("{r}", &band_role(l, s(data, "role")))],
            cta_path: band_path(data),
        },
        NotificationType::PlatformRoleChanged => NotificationText {
            title: l
                .pick(
                    "Your role on Setlyst changed",
                    "Seu papel no Setlyst mudou",
                    "Tu rol en Setlyst cambió",
                )
                .into(),
            lines: vec![l
                .pick(
                    "Your role on Setlyst changed from {o} to {n}.",
                    "Seu papel no Setlyst passou de {o} para {n}.",
                    "Tu rol en Setlyst pasó de {o} a {n}.",
                )
                .replace("{o}", &platform_role(l, s(data, "old_role")))
                .replace("{n}", &platform_role(l, s(data, "new_role")))],
            cta_path: Some("/dashboard".into()),
        },
        NotificationType::ShareLinkRevoked => {
            let is_gig = s(data, "kind") == "gig";
            let mut lines = vec![if is_gig {
                l.pick(
                    "The public link of the gig \"{t}\" was disabled by the Setlyst team.",
                    "O link público do show \"{t}\" foi desativado pela equipe do Setlyst.",
                    "El equipo de Setlyst desactivó el enlace público del concierto \"{t}\".",
                )
            } else {
                l.pick(
                    "The public link of the setlist \"{t}\" was disabled by the Setlyst team.",
                    "O link público da setlist \"{t}\" foi desativado pela equipe do Setlyst.",
                    "El equipo de Setlyst desactivó el enlace público de la setlist \"{t}\".",
                )
            }
            .replace("{t}", s(data, "title"))];
            let reason = s(data, "reason");
            if !reason.is_empty() {
                lines.push(format!("{} {reason}", l.pick("Reason:", "Motivo:", "Motivo:")));
            }
            NotificationText {
                title: l
                    .pick(
                        "A public link was disabled",
                        "Um link público foi desativado",
                        "Se desactivó un enlace público",
                    )
                    .into(),
                lines,
                cta_path: Some("/dashboard".into()),
            }
        }
        NotificationType::Announcement => NotificationText {
            title: s(data, "title").to_string(),
            lines: vec![l
                .pick(
                    "There is a new announcement from the Setlyst team.",
                    "Há um novo aviso da equipe do Setlyst.",
                    "Hay un nuevo aviso del equipo de Setlyst.",
                )
                .into()],
            cta_path: Some("/dashboard/announcements".into()),
        },
        NotificationType::ReleasePublished => NotificationText {
            title: l
                .pick(
                    "What's new in Setlyst {v}",
                    "Novidades do Setlyst: versão {v}",
                    "Novedades de Setlyst: versión {v}",
                )
                .replace("{v}", s(data, "version")),
            lines: vec![l
                .pick(
                    "A new version of Setlyst is available. See what changed.",
                    "Uma nova versão do Setlyst está disponível. Veja o que mudou.",
                    "Hay una nueva versión de Setlyst disponible. Mira qué cambió.",
                )
                .into()],
            cta_path: Some("/dashboard/whats-new".into()),
        },
        NotificationType::BandSuggestionCreated => NotificationText {
            title: l
                .pick(
                    "New suggestion in {b}",
                    "Nova sugestão na banda {b}",
                    "Nueva sugerencia en {b}",
                )
                .replace("{b}", band),
            lines: vec![l
                .pick(
                    "{u} suggested the song \"{s}\" for the band {b}. Vote on it or review it on the band page.",
                    "{u} sugeriu a música \"{s}\" para a banda {b}. Vote ou avalie a sugestão na página da banda.",
                    "{u} sugirió la canción \"{s}\" para la banda {b}. Vota o revisa la sugerencia en la página de la banda.",
                )
                .replace("{u}", s(data, "suggested_by"))
                .replace("{s}", s(data, "song_title"))
                .replace("{b}", band)],
            cta_path: band_path(data),
        },
        NotificationType::BandSuggestionResolved => {
            let status = match s(data, "status") {
                "accepted" => l.pick("accepted", "aceita", "aceptada"),
                "rejected" => l.pick("declined", "recusada", "rechazada"),
                _ => l.pick("withdrawn", "retirada", "retirada"),
            };
            NotificationText {
                title: l
                    .pick(
                        "Your suggestion was {st}",
                        "Sua sugestão foi {st}",
                        "Tu sugerencia fue {st}",
                    )
                    .replace("{st}", status),
                lines: vec![l
                    .pick(
                        "The suggestion of the song \"{s}\" in the band {b} was {st}.",
                        "A sugestão da música \"{s}\" na banda {b} foi {st}.",
                        "La sugerencia de la canción \"{s}\" en la banda {b} fue {st}.",
                    )
                    .replace("{s}", s(data, "song_title"))
                    .replace("{b}", band)
                    .replace("{st}", status)],
                cta_path: band_path(data),
            }
        }
        NotificationType::ModerationAction => {
            let (title, line) = match s(data, "action") {
                "avatar_removed" => (
                    l.pick(
                        "Your profile picture was removed",
                        "Sua foto de perfil foi removida",
                        "Se eliminó tu foto de perfil",
                    ),
                    l.pick(
                        "The Setlyst team removed your profile picture because it did not follow the Community Guidelines.",
                        "A equipe do Setlyst removeu sua foto de perfil por não seguir as Diretrizes da Comunidade.",
                        "El equipo de Setlyst eliminó tu foto de perfil porque no seguía las Normas de la Comunidad.",
                    ),
                ),
                "username_reset" => (
                    l.pick(
                        "Your username was reset",
                        "Seu nome de usuário foi redefinido",
                        "Se restableció tu nombre de usuario",
                    ),
                    l.pick(
                        "The Setlyst team replaced your username because it did not follow the Community Guidelines. You can choose a new one in Settings.",
                        "A equipe do Setlyst substituiu seu nome de usuário por não seguir as Diretrizes da Comunidade. Você pode escolher um novo nome em Configurações.",
                        "El equipo de Setlyst reemplazó tu nombre de usuario porque no seguía las Normas de la Comunidad. Puedes elegir uno nuevo en Configuración.",
                    ),
                ),
                _ => (
                    l.pick(
                        "A band logo was removed",
                        "O logo de uma banda foi removido",
                        "Se eliminó el logo de una banda",
                    ),
                    l.pick(
                        "The Setlyst team removed the logo of the band {b} because it did not follow the Community Guidelines.",
                        "A equipe do Setlyst removeu o logo da banda {b} por não seguir as Diretrizes da Comunidade.",
                        "El equipo de Setlyst eliminó el logo de la banda {b} porque no seguía las Normas de la Comunidad.",
                    ),
                ),
            };
            let mut lines = vec![line.replace("{b}", band)];
            let note = s(data, "note");
            if !note.is_empty() {
                lines.push(format!(
                    "{} {note}",
                    l.pick("Note from the team:", "Observação da equipe:", "Nota del equipo:")
                ));
            }
            NotificationText {
                title: title.into(),
                lines,
                cta_path: Some("/dashboard/profile".into()),
            }
        }
        NotificationType::CreditsGranted => {
            let amount = data.get("amount").and_then(Value::as_i64).unwrap_or(0);
            let reason = match s(data, "reason") {
                "referral_referrer" => l.pick(
                    "Thank you for recommending Setlyst. Someone you referred confirmed their email.",
                    "Obrigado por indicar o Setlyst. Uma pessoa que você indicou confirmou o e-mail.",
                    "Gracias por recomendar Setlyst. Una persona que invitaste confirmó su correo.",
                ),
                "referral_referred" => l.pick(
                    "You received welcome credits for joining through a referral.",
                    "Você recebeu créditos de boas-vindas por entrar com uma indicação.",
                    "Recibiste créditos de bienvenida por unirte con una invitación.",
                ),
                "promo_code" => l.pick(
                    "Credits from a promo code were added to your balance.",
                    "Créditos de um código promocional foram adicionados ao seu saldo.",
                    "Se agregaron a tu saldo créditos de un código promocional.",
                ),
                _ => l.pick(
                    "The Setlyst team added credits to your balance.",
                    "A equipe do Setlyst adicionou créditos ao seu saldo.",
                    "El equipo de Setlyst agregó créditos a tu saldo.",
                ),
            };
            NotificationText {
                title: l
                    .pick(
                        "You received {n} credits",
                        "Você recebeu {n} créditos",
                        "Recibiste {n} créditos",
                    )
                    .replace("{n}", &amount.to_string()),
                lines: vec![
                    reason.into(),
                    l.pick(
                        "Use your credits under Settings, in the Subscription section.",
                        "Use os créditos em Configurações, na seção Assinatura.",
                        "Usa tus créditos en Configuración, en la sección Suscripción.",
                    )
                    .into(),
                ],
                cta_path: Some("/dashboard/settings".into()),
            }
        }
        NotificationType::SubscriptionChanged
        | NotificationType::TrialEnding
        | NotificationType::SecurityAlert => NotificationText {
            title: l
                .pick(
                    "Your Setlyst account was updated",
                    "Sua conta do Setlyst foi atualizada",
                    "Se actualizó tu cuenta de Setlyst",
                )
                .into(),
            lines: vec![l
                .pick(
                    "Open Setlyst to see the details.",
                    "Abra o Setlyst para ver os detalhes.",
                    "Abre Setlyst para ver los detalles.",
                )
                .into()],
            cta_path: Some("/dashboard/settings".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_kind_has_text_in_every_locale() {
        let data = json!({
            "band_id": "b1", "band_name": "Os Tais", "old_role": "member", "new_role": "admin",
            "role": "member", "kind": "setlist", "title": "Sexta", "reason": "spam",
            "song_title": "Garota", "suggested_by": "ana", "status": "accepted",
            "action": "avatar_removed", "note": "n", "amount": 50, "version": "0.12.0"
        });
        for kind in [
            NotificationType::BandRoleChanged,
            NotificationType::BandMemberRemoved,
            NotificationType::BandMemberAdded,
            NotificationType::PlatformRoleChanged,
            NotificationType::ShareLinkRevoked,
            NotificationType::Announcement,
            NotificationType::ReleasePublished,
            NotificationType::BandSuggestionCreated,
            NotificationType::BandSuggestionResolved,
            NotificationType::ModerationAction,
            NotificationType::SubscriptionChanged,
            NotificationType::TrialEnding,
            NotificationType::CreditsGranted,
            NotificationType::SecurityAlert,
        ] {
            for locale in Locale::ALL {
                let text = describe(kind, &data, locale);
                assert!(!text.title.is_empty(), "{kind:?}");
                assert!(!text.lines.is_empty(), "{kind:?}");
                assert!(!text.title.contains('{'), "{kind:?}: {}", text.title);
                for line in &text.lines {
                    assert!(!line.contains("{"), "{kind:?}: {line}");
                }
            }
        }
        let role = describe(NotificationType::BandRoleChanged, &data, Locale::PtBr);
        assert_eq!(
            role.lines[0],
            "Seu papel na banda Os Tais passou de Integrante para Administrador."
        );
        assert_eq!(role.cta_path.as_deref(), Some("/dashboard/bands/b1"));
    }
}
