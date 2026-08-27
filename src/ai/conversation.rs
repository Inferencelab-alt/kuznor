use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::Message;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateProfile {
    StandardAlternating,
    SystemSupported,
    NoSystem,
    TemplateDriven,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedConversation {
    pub messages: Vec<ApiMessage>,
    pub fallback_without_system: Vec<ApiMessage>,
    pub profile: TemplateProfile,
}

pub fn trim_history(
    system: String,
    history: &[Message],
    question: &str,
    context_size: usize,
    max_tokens: usize,
) -> Vec<ApiMessage> {
    let budget = context_size.saturating_sub(max_tokens).max(256) * 4;
    let question = question.trim();
    let mut selected = Vec::new();
    let mut used = system.len() + question.len();
    for message in history.iter().rev() {
        if !matches!(message.role.as_str(), "user" | "assistant")
            || message.content.trim().is_empty()
        {
            continue;
        }
        if used + message.content.len() > budget {
            break;
        }
        used += message.content.len();
        selected.push(ApiMessage {
            role: message.role.clone(),
            content: message.content.clone(),
        });
    }
    selected.reverse();
    let mut input = Vec::with_capacity(selected.len() + 2);
    if !system.trim().is_empty() {
        input.push(ApiMessage {
            role: "system".into(),
            content: system,
        });
    }
    input.extend(selected);
    if !question.is_empty() {
        input.push(ApiMessage {
            role: "user".into(),
            content: question.into(),
        });
    }
    normalize_chat_messages(&input, TemplateProfile::SystemSupported).unwrap_or_default()
}

pub fn prepare(messages: &[ApiMessage], profile: TemplateProfile) -> Result<PreparedConversation> {
    let normalized = normalize_chat_messages(messages, profile)?;
    let fallback_without_system = normalize_chat_messages(messages, TemplateProfile::NoSystem)?;
    Ok(PreparedConversation {
        messages: normalized,
        fallback_without_system,
        profile,
    })
}

pub fn normalize_chat_messages(
    messages: &[ApiMessage],
    profile: TemplateProfile,
) -> Result<Vec<ApiMessage>> {
    let mut system = Vec::new();
    let mut turns = Vec::new();
    for message in messages {
        let content = message.content.trim();
        if content.is_empty() {
            continue;
        }
        match message.role.as_str() {
            "system" => system.push(content.to_owned()),
            "user" | "assistant" => turns.push(ApiMessage {
                role: message.role.clone(),
                content: content.to_owned(),
            }),
            _ => {}
        }
    }

    let mut merged_turns: Vec<ApiMessage> = Vec::with_capacity(turns.len());
    for message in turns {
        if let Some(previous) = merged_turns.last_mut() {
            if previous.role == message.role {
                previous.content.push_str("\n\n");
                previous.content.push_str(&message.content);
                continue;
            }
        }
        merged_turns.push(message);
    }
    while merged_turns
        .first()
        .is_some_and(|message| message.role == "assistant")
    {
        merged_turns.remove(0);
    }
    if merged_turns.is_empty() && system.is_empty() {
        bail!("La conversacion no contiene mensajes utilizables");
    }

    let combined_system = system.join("\n\n");
    let use_system = !combined_system.is_empty() && profile != TemplateProfile::NoSystem;
    if profile == TemplateProfile::NoSystem && !combined_system.is_empty() {
        if let Some(first_user) = merged_turns
            .iter_mut()
            .find(|message| message.role == "user")
        {
            first_user.content = format!("{combined_system}\n\n{}", first_user.content);
        } else {
            merged_turns.insert(
                0,
                ApiMessage {
                    role: "user".into(),
                    content: combined_system.clone(),
                },
            );
        }
    }

    let mut output = Vec::with_capacity(merged_turns.len() + usize::from(use_system));
    if use_system {
        output.push(ApiMessage {
            role: "system".into(),
            content: combined_system,
        });
    }
    output.extend(merged_turns);
    validate_sequence(&output, profile)?;
    Ok(output)
}

pub fn validate_sequence(messages: &[ApiMessage], profile: TemplateProfile) -> Result<()> {
    let mut index = 0;
    if messages
        .first()
        .is_some_and(|message| message.role == "system")
    {
        if profile == TemplateProfile::NoSystem {
            bail!("El perfil de template no admite system");
        }
        index = 1;
    }
    let mut expected = "user";
    for message in &messages[index..] {
        if message.role != expected {
            bail!("Secuencia de roles invalida: se esperaba {expected}");
        }
        expected = if expected == "user" {
            "assistant"
        } else {
            "user"
        };
    }
    Ok(())
}

pub fn profile_from_props(props: &Value) -> TemplateProfile {
    if props
        .get("chat_template_caps")
        .and_then(|caps| caps.get("supports_system_role"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        return TemplateProfile::NoSystem;
    }
    if props
        .get("chat_template_caps")
        .and_then(|caps| caps.get("supports_system_role"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return TemplateProfile::TemplateDriven;
    }
    let template = props
        .get("chat_template")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if template.is_empty() {
        TemplateProfile::StandardAlternating
    } else if template.contains("system")
        || template.contains("<<sys>>")
        || template.contains("system_message")
    {
        TemplateProfile::SystemSupported
    } else {
        TemplateProfile::StandardAlternating
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str) -> ApiMessage {
        ApiMessage {
            role: role.into(),
            content: content.into(),
        }
    }

    #[test]
    fn supports_system_user_and_alternating_history() {
        let messages = normalize_chat_messages(
            &[message("system", "reglas"), message("user", "hola")],
            TemplateProfile::SystemSupported,
        )
        .unwrap();
        assert_eq!(
            messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            ["system", "user"]
        );

        let messages = normalize_chat_messages(
            &[
                message("system", "reglas"),
                message("user", "hola"),
                message("assistant", "respuesta"),
                message("user", "otra"),
            ],
            TemplateProfile::SystemSupported,
        )
        .unwrap();
        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn merges_consecutive_roles_and_removes_empty_messages() {
        let messages = normalize_chat_messages(
            &[
                message("system", ""),
                message("user", "uno"),
                message("user", "dos"),
                message("assistant", ""),
                message("assistant", "tres"),
            ],
            TemplateProfile::SystemSupported,
        )
        .unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages[0].content.contains("uno\n\ndos"));
        assert_eq!(messages[1].role, "assistant");
    }

    #[test]
    fn drops_leading_assistant_from_trimmed_failed_history() {
        let history = vec![Message {
            id: 1,
            chat_id: 1,
            role: "assistant".into(),
            content: "respuesta vieja".into(),
            sources: vec![],
            created_at: String::new(),
        }];
        let result = trim_history("reglas".into(), &history, "pregunta", 4096, 512);
        assert_eq!(
            result.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            ["system", "user"]
        );
    }

    #[test]
    fn fuses_system_for_templates_without_system_role() {
        let prepared = prepare(
            &[message("system", "reglas"), message("user", "pregunta")],
            TemplateProfile::NoSystem,
        )
        .unwrap();
        assert_eq!(prepared.messages.len(), 1);
        assert!(prepared.messages[0].content.contains("reglas"));
        assert_eq!(prepared.messages[0].role, "user");
    }

    #[test]
    fn recognizes_props_capabilities_without_model_name() {
        let props = serde_json::json!({
            "chat_template": "custom",
            "chat_template_caps": {"supports_system_role": false}
        });
        assert_eq!(profile_from_props(&props), TemplateProfile::NoSystem);
    }

    #[test]
    fn representative_family_templates_keep_valid_roles() {
        for (family, template) in [
            ("qwen", "<|im_start|>{{ message['role'] }}"),
            ("gemma", "{% if message['role'] == 'system' %}"),
            ("llama", "<<SYS>> {{ content }}"),
            ("mistral", "[INST] {{ content }} [/INST]"),
            ("phi", "<|user|>{{ content }}<|end|>"),
        ] {
            let props = serde_json::json!({"chat_template": template});
            let profile = profile_from_props(&props);
            let prepared = prepare(
                &[message("system", family), message("user", "pregunta")],
                profile,
            )
            .unwrap();
            assert!(validate_sequence(&prepared.messages, profile).is_ok());
        }
    }

    #[test]
    fn context_is_one_user_message_not_a_second_user_turn() {
        let prepared = prepare(
            &[
                message("system", "documentos"),
                message("user", "Pregunta: de que trata?\n\nContexto: evidencia"),
            ],
            TemplateProfile::SystemSupported,
        )
        .unwrap();
        assert_eq!(
            prepared
                .messages
                .iter()
                .filter(|m| m.role == "user")
                .count(),
            1
        );
    }
}
