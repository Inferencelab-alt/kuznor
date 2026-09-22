use std::{
    io::{BufRead, BufReader},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ai::conversation::{self, PreparedConversation, TemplateProfile},
    config::{Settings, endpoint_url},
    models::Message,
};

pub use crate::ai::conversation::ApiMessage;

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ApiMessage],
    temperature: f32,
    max_tokens: usize,
    stream: bool,
    chat_template_kwargs: ChatTemplateKwargs,
}

#[derive(Debug, Serialize)]
struct ChatTemplateKwargs {
    enable_thinking: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}
#[derive(Debug, Deserialize)]
struct Choice {
    message: ApiMessage,
}

#[derive(Debug, Deserialize)]
struct StreamResponse {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    content: Option<String>,
}

#[derive(Debug, Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
}
#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}
#[derive(Debug, Deserialize)]
struct EmbeddingData {
    index: usize,
    embedding: Vec<f32>,
}

fn client(timeout: Duration) -> Result<Client> {
    Ok(Client::builder().timeout(timeout).no_proxy().build()?)
}

pub fn health(host: &str, port: u16) -> bool {
    client(Duration::from_secs(2))
        .and_then(|c| {
            c.get(endpoint_url(host, port, "/health")?)
                .send()
                .map_err(Into::into)
        })
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

pub fn detect_template_profile(settings: &Settings) -> Result<TemplateProfile> {
    let response = client(Duration::from_secs(2))?
        .get(settings.chat_props_url()?)
        .send()
        .context("No se pudo consultar /props del servidor de chat")?;
    if !response.status().is_success() {
        anyhow::bail!("El endpoint /props respondio {}", response.status());
    }
    let props: Value = response.json().context("Respuesta /props invalida")?;
    Ok(conversation::profile_from_props(&props))
}

pub fn prepare_messages(
    settings: &Settings,
    messages: &[ApiMessage],
) -> Result<PreparedConversation> {
    let profile = detect_template_profile(settings).unwrap_or(TemplateProfile::StandardAlternating);
    conversation::prepare(messages, profile)
}

#[allow(dead_code)]
pub fn chat(settings: &Settings, messages: &[ApiMessage]) -> Result<String> {
    let prepared = prepare_messages(settings, messages)?;
    chat_prepared(settings, &prepared)
}

pub fn chat_prepared(settings: &Settings, prepared: &PreparedConversation) -> Result<String> {
    match request_chat(settings, &prepared.messages) {
        Ok(answer) => Ok(answer),
        Err(error) if is_template_error(&error) => {
            request_chat(settings, &prepared.fallback_without_system).map_err(|fallback| {
                error.context(format!("Reintento compatible fallido: {fallback}"))
            })
        }
        Err(error) => Err(error),
    }
}

fn request_chat(settings: &Settings, messages: &[ApiMessage]) -> Result<String> {
    let response = client(Duration::from_secs(300))?
        .post(settings.chat_url()?)
        .json(&ChatRequest {
            model: "local",
            messages,
            temperature: settings.temperature,
            max_tokens: settings.max_tokens,
            stream: false,
            chat_template_kwargs: ChatTemplateKwargs {
                enable_thinking: false,
            },
        })
        .send()
        .context("No se pudo conectar con el servidor de chat")?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        anyhow::bail!("El servidor de chat respondio {status}: {body}");
    }
    parse_chat_response(&body)
}

#[allow(dead_code)]
pub fn chat_stream(
    settings: &Settings,
    messages: &[ApiMessage],
    cancelled: &AtomicBool,
    on_delta: impl FnMut(&str),
) -> Result<String> {
    let prepared = prepare_messages(settings, messages)?;
    chat_stream_prepared(settings, &prepared, cancelled, on_delta)
}

pub fn chat_stream_prepared(
    settings: &Settings,
    prepared: &PreparedConversation,
    cancelled: &AtomicBool,
    mut on_delta: impl FnMut(&str),
) -> Result<String> {
    match request_chat_stream(settings, &prepared.messages, cancelled, &mut on_delta) {
        Ok(answer) => Ok(answer),
        Err(error) if is_template_error(&error) && !cancelled.load(Ordering::Relaxed) => {
            request_chat_stream(
                settings,
                &prepared.fallback_without_system,
                cancelled,
                &mut on_delta,
            )
            .map_err(|fallback| error.context(format!("Reintento compatible fallido: {fallback}")))
        }
        Err(error) => Err(error),
    }
}

fn request_chat_stream(
    settings: &Settings,
    messages: &[ApiMessage],
    cancelled: &AtomicBool,
    on_delta: &mut impl FnMut(&str),
) -> Result<String> {
    let response = client(Duration::from_secs(300))?
        .post(settings.chat_url()?)
        .json(&ChatRequest {
            model: "local",
            messages,
            temperature: settings.temperature,
            max_tokens: settings.max_tokens,
            stream: true,
            chat_template_kwargs: ChatTemplateKwargs {
                enable_thinking: false,
            },
        })
        .send()
        .context("No se pudo conectar con el servidor de chat")?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!(
            "El servidor de chat respondio {status}: {}",
            response.text()?
        );
    }

    let mut reader = BufReader::new(response);
    let mut line = String::new();
    let mut answer = String::new();
    loop {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        if let Some(delta) = parse_stream_data(data)? {
            answer.push_str(&delta);
            on_delta(&delta);
        }
    }
    if answer.is_empty() && !cancelled.load(Ordering::Relaxed) {
        anyhow::bail!("El servidor no envio contenido en streaming");
    }
    Ok(answer)
}

fn parse_stream_data(data: &str) -> Result<Option<String>> {
    let parsed: StreamResponse =
        serde_json::from_str(data).context("Fragmento JSON de streaming invalido")?;
    Ok(parsed
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.delta.content))
}

pub fn parse_chat_response(body: &str) -> Result<String> {
    let parsed: ChatResponse =
        serde_json::from_str(body).context("Respuesta JSON de chat invalida")?;
    parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .context("La respuesta no contiene opciones")
}

pub fn embeddings_with_timeout(
    settings: &Settings,
    inputs: &[String],
    timeout: Duration,
) -> Result<Vec<Vec<f32>>> {
    let response = client(timeout)?
        .post(settings.embedding_url()?)
        .json(&EmbeddingRequest {
            model: "local",
            input: inputs,
        })
        .send()
        .context("No se pudo conectar con el servidor de embeddings")?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        anyhow::bail!("El servidor de embeddings respondio {status}: {body}");
    }
    let mut data: Vec<EmbeddingData> = serde_json::from_str::<EmbeddingResponse>(&body)
        .context("Respuesta JSON de embeddings invalida")?
        .data;
    data.sort_by_key(|item| item.index);
    if data.len() != inputs.len() {
        anyhow::bail!(
            "Se esperaban {} embeddings y llegaron {}",
            inputs.len(),
            data.len()
        );
    }
    Ok(data.into_iter().map(|item| item.embedding).collect())
}

pub fn trim_history(
    system: String,
    history: &[Message],
    question: &str,
    context_size: usize,
    max_tokens: usize,
) -> Vec<ApiMessage> {
    conversation::trim_history(system, history, question, context_size, max_tokens)
}

pub fn is_template_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    [
        "conversation roles must alternate",
        "roles must alternate",
        "chat template",
        "template",
        "system role",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::atomic::AtomicBool,
        thread,
    };
    #[test]
    fn parses_http_response() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"hola"}}]}"#;
        assert_eq!(parse_chat_response(body).unwrap(), "hola");
    }
    #[test]
    fn parses_streaming_delta() {
        let body = r#"{"choices":[{"delta":{"content":"hola"}}]}"#;
        assert_eq!(parse_stream_data(body).unwrap().as_deref(), Some("hola"));
    }
    #[test]
    fn consumes_openai_compatible_sse_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let size = socket.read(&mut request).unwrap_or_default();
                let request = String::from_utf8_lossy(&request[..size]);
                if request.contains("/props") {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"chat_template_caps\":{\"supports_system_role\":true}}",
                        )
                        .unwrap();
                } else {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"Ku\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"znor\"}}]}\n\ndata: [DONE]\n\n",
                        )
                        .unwrap();
                }
            }
        });
        let settings = Settings {
            chat_port: port,
            ..Settings::default()
        };
        let mut deltas = Vec::new();
        let answer = chat_stream(
            &settings,
            &[ApiMessage {
                role: "user".into(),
                content: "hola".into(),
            }],
            &AtomicBool::new(false),
            |delta| deltas.push(delta.to_owned()),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(answer, "Kuznor");
        assert_eq!(deltas, ["Ku", "znor"]);
    }
    #[test]
    fn keeps_current_question_and_trims_oldest() {
        let history = (0..8)
            .map(|id| Message {
                id,
                chat_id: 1,
                role: "user".into(),
                content: "x".repeat(300),
                sources: vec![],
                created_at: String::new(),
            })
            .collect::<Vec<_>>();
        let out = trim_history("sys".into(), &history, "actual", 300, 100);
        assert!(out.iter().any(|message| message.content.contains("actual")));
        assert_eq!(out.last().unwrap().role, "user");
        assert!(out.len() < history.len() + 2);
    }
}
