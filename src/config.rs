use std::{collections::HashMap, net::IpAddr};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::db::Database;

pub const DEFAULT_LLAMA_COMMAND: &str = "llama";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    pub chat_model_path: String,
    pub embedding_model_path: String,
    pub chat_host: String,
    pub chat_port: u16,
    pub embedding_host: String,
    pub embedding_port: u16,
    pub llama_command: String,
    pub temperature: f32,
    pub max_tokens: usize,
    pub context_size: usize,
    pub inference_threads: usize,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub top_k: usize,
    pub diagnostic_mode: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            chat_model_path: String::new(),
            embedding_model_path: String::new(),
            chat_host: "127.0.0.1".into(),
            chat_port: 8080,
            embedding_host: "127.0.0.1".into(),
            embedding_port: 8081,
            llama_command: DEFAULT_LLAMA_COMMAND.into(),
            temperature: 0.3,
            max_tokens: 1024,
            context_size: 4096,
            inference_threads: 0,
            chunk_size: 600,
            chunk_overlap: 80,
            top_k: 4,
            diagnostic_mode: false,
        }
    }
}

impl Settings {
    pub fn load(db: &Database) -> Result<Self> {
        let mut settings = Self::default();
        let values: HashMap<String, String> = db.load_settings()?;
        macro_rules! text_setting {
            ($field:ident) => {
                if let Some(value) = values.get(stringify!($field)) {
                    settings.$field = value.clone();
                }
            };
        }
        macro_rules! parsed_setting {
            ($field:ident) => {
                if let Some(value) = values.get(stringify!($field)) {
                    settings.$field = value.parse().with_context(|| {
                        format!("Configuracion invalida: {}", stringify!($field))
                    })?;
                }
            };
        }
        text_setting!(chat_model_path);
        text_setting!(embedding_model_path);
        text_setting!(chat_host);
        text_setting!(embedding_host);
        text_setting!(llama_command);
        parsed_setting!(chat_port);
        parsed_setting!(embedding_port);
        parsed_setting!(temperature);
        parsed_setting!(max_tokens);
        parsed_setting!(context_size);
        parsed_setting!(inference_threads);
        parsed_setting!(chunk_size);
        parsed_setting!(chunk_overlap);
        parsed_setting!(top_k);
        parsed_setting!(diagnostic_mode);
        Ok(settings)
    }

    pub fn save(&self, db: &Database) -> Result<()> {
        self.validate_local_only()?;
        let value = serde_json::to_value(self)?;
        let object = value.as_object().context("Settings no es un objeto")?;
        for (key, value) in object {
            let text = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            db.save_setting(key, &text)?;
        }
        Ok(())
    }

    pub fn validate_local_only(&self) -> Result<()> {
        validate_loopback_host(&self.chat_host).context("Host de chat no permitido")?;
        validate_loopback_host(&self.embedding_host).context("Host de embeddings no permitido")?;
        Ok(())
    }

    pub fn chat_url(&self) -> Result<String> {
        endpoint_url(&self.chat_host, self.chat_port, "/v1/chat/completions")
    }

    pub fn embedding_url(&self) -> Result<String> {
        endpoint_url(&self.embedding_host, self.embedding_port, "/v1/embeddings")
    }

    pub fn chat_props_url(&self) -> Result<String> {
        endpoint_url(&self.chat_host, self.chat_port, "/props")
    }
}

pub fn effective_inference_threads(configured: usize) -> usize {
    if configured > 0 {
        return configured;
    }
    let available = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(2);
    available.saturating_sub(2).max(1)
}

pub fn validate_loopback_host(host: &str) -> Result<()> {
    let candidate = host.trim().trim_start_matches('[').trim_end_matches(']');
    if candidate.eq_ignore_ascii_case("localhost")
        || candidate
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
    {
        return Ok(());
    }
    anyhow::bail!("Kuznor V1 solo permite servidores locales: usa localhost, 127.0.0.1 o ::1")
}

pub fn endpoint_url(host: &str, port: u16, path: &str) -> Result<String> {
    validate_loopback_host(host)?;
    let candidate = host.trim().trim_start_matches('[').trim_end_matches(']');
    let candidate = if candidate.eq_ignore_ascii_case("localhost") {
        "127.0.0.1"
    } else {
        candidate
    };
    let formatted = if candidate.contains(':') {
        format!("[{candidate}]")
    } else {
        candidate.to_owned()
    };
    Ok(format!("http://{formatted}:{port}{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir_in("target").unwrap();
        let db = Database::open(dir.path().join("test.db")).unwrap();
        let settings = Settings {
            top_k: 7,
            temperature: 0.7,
            ..Default::default()
        };
        settings.save(&db).unwrap();
        assert_eq!(Settings::load(&db).unwrap(), settings);
    }

    #[test]
    fn defaults_are_portable() {
        let settings = Settings::default();
        assert!(settings.chat_model_path.is_empty());
        assert!(settings.embedding_model_path.is_empty());
        assert_eq!(settings.llama_command, "llama");
        assert_eq!(settings.inference_threads, 0);
    }

    #[test]
    fn automatic_threads_leave_capacity_for_the_system() {
        assert!(effective_inference_threads(0) >= 1);
        assert_eq!(effective_inference_threads(5), 5);
    }

    #[test]
    fn accepts_only_explicit_loopback_hosts() {
        for host in [
            "localhost",
            "LOCALHOST",
            "127.0.0.1",
            "127.12.34.56",
            "::1",
            "[::1]",
        ] {
            validate_loopback_host(host).unwrap();
        }
        for host in ["example.com", "192.168.1.20", "10.0.0.2", "0.0.0.0", "::"] {
            assert!(
                validate_loopback_host(host).is_err(),
                "{host} debe rechazarse"
            );
        }
    }

    #[test]
    fn formats_ipv4_and_ipv6_local_urls() {
        assert_eq!(
            endpoint_url("127.0.0.1", 8080, "/health").unwrap(),
            "http://127.0.0.1:8080/health"
        );
        assert_eq!(
            endpoint_url("::1", 8081, "/health").unwrap(),
            "http://[::1]:8081/health"
        );
        assert_eq!(
            endpoint_url("localhost", 8080, "/health").unwrap(),
            "http://127.0.0.1:8080/health"
        );
    }

    #[test]
    fn refuses_to_persist_remote_hosts() {
        let dir = tempfile::tempdir_in("target").unwrap();
        let db = Database::open(dir.path().join("test.db")).unwrap();
        let settings = Settings {
            chat_host: "api.example.com".into(),
            ..Default::default()
        };
        assert!(settings.save(&db).is_err());
    }
}
