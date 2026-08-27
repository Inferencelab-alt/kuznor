use std::{
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use crate::{
    ai::client::health,
    config::{Settings, effective_inference_threads, validate_loopback_host},
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSpec {
    program: String,
    prefix_args: Vec<String>,
}

fn command_spec(value: &str) -> Result<CommandSpec> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("No hay un comando de llama.cpp configurado");
    }
    let path = std::path::Path::new(value);
    let (program, mut prefix_args) = if path.exists() {
        (value.to_owned(), Vec::new())
    } else {
        let mut parts = value.split_whitespace();
        let program = parts
            .next()
            .context("No hay un comando de llama.cpp configurado")?
            .to_owned();
        (program, parts.map(str::to_owned).collect())
    };
    let executable_name = std::path::Path::new(&program)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(&program)
        .to_ascii_lowercase();
    if executable_name == "llama" && prefix_args.is_empty() {
        prefix_args.push("serve".into());
    }
    Ok(CommandSpec {
        program,
        prefix_args,
    })
}

pub fn resolve_llama_command(configured: &str) -> Option<String> {
    let mut candidates = Vec::new();
    if !configured.trim().is_empty() {
        candidates.push(configured.trim().to_owned());
    }
    for fallback in ["llama", "llama-server"] {
        if !candidates.iter().any(|candidate| candidate == fallback) {
            candidates.push(fallback.into());
        }
    }
    candidates.into_iter().find(|candidate| {
        let Ok(spec) = command_spec(candidate) else {
            return false;
        };
        Command::new(spec.program)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Chat,
    Embedding,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStatus {
    Disconnected,
    Starting,
    Ready,
    Busy,
    Error(String),
}

#[derive(Default)]
pub struct ProcessManager {
    chat: Option<Child>,
    embedding: Option<Child>,
}

impl ProcessManager {
    pub fn ensure(&mut self, kind: ServiceKind, settings: &Settings) -> Result<bool> {
        let (host, port, model) = match kind {
            ServiceKind::Chat => (
                &settings.chat_host,
                settings.chat_port,
                &settings.chat_model_path,
            ),
            ServiceKind::Embedding => (
                &settings.embedding_host,
                settings.embedding_port,
                &settings.embedding_model_path,
            ),
        };
        validate_loopback_host(host)?;
        if health(host, port) {
            return Ok(false);
        }
        if !std::path::Path::new(model).exists() {
            anyhow::bail!("No se encontro el modelo: {model}");
        }
        let resolved = resolve_llama_command(&settings.llama_command)
            .unwrap_or_else(|| settings.llama_command.clone());
        let spec = command_spec(&resolved)?;
        let mut command = Command::new(&spec.program);
        command.args(&spec.prefix_args);
        command.args([
            "-m",
            model,
            "--host",
            host,
            "--port",
            &port.to_string(),
            "-c",
            &settings.context_size.to_string(),
            "-t",
            &effective_inference_threads(settings.inference_threads).to_string(),
        ]);
        if kind == ServiceKind::Embedding {
            command.arg("--embedding");
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().with_context(|| {
            format!(
                "No se pudo ejecutar '{}'. Configura 'llama', 'llama serve' o la ruta de llama-server.",
                resolved
            )
        })?;
        *match kind {
            ServiceKind::Chat => &mut self.chat,
            ServiceKind::Embedding => &mut self.embedding,
        } = Some(child);
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(90) {
            if health(host, port) {
                return Ok(true);
            }
            let slot = match kind {
                ServiceKind::Chat => &mut self.chat,
                ServiceKind::Embedding => &mut self.embedding,
            };
            if let Some(child) = slot {
                if let Some(code) = child.try_wait()? {
                    anyhow::bail!("El servidor llama.cpp termino durante el arranque: {code}");
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
        anyhow::bail!("Tiempo de espera agotado iniciando el servidor en {host}:{port}")
    }
    pub fn stop_owned(&mut self, kind: ServiceKind) -> Result<bool> {
        let slot = match kind {
            ServiceKind::Chat => &mut self.chat,
            ServiceKind::Embedding => &mut self.embedding,
        };
        if let Some(mut child) = slot.take() {
            child
                .kill()
                .context("No se pudo detener el servidor llama.cpp")?;
            let _ = child.wait();
            return Ok(true);
        }
        Ok(false)
    }
    pub fn owns(&self, kind: ServiceKind) -> bool {
        match kind {
            ServiceKind::Chat => self.chat.is_some(),
            ServiceKind::Embedding => self.embedding.is_some(),
        }
    }

    pub fn owned_count(&self) -> usize {
        usize::from(self.chat.is_some()) + usize::from(self.embedding.is_some())
    }
}
impl Drop for ProcessManager {
    fn drop(&mut self) {
        let _ = self.stop_owned(ServiceKind::Chat);
        let _ = self.stop_owned(ServiceKind::Embedding);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llama_cli_uses_serve_subcommand() {
        assert_eq!(
            command_spec("llama").unwrap(),
            CommandSpec {
                program: "llama".into(),
                prefix_args: vec!["serve".into()],
            }
        );
        assert_eq!(command_spec("llama serve").unwrap().prefix_args, ["serve"]);
    }

    #[test]
    fn llama_server_does_not_add_subcommand() {
        assert!(command_spec("llama-server").unwrap().prefix_args.is_empty());
    }

    #[test]
    fn process_manager_counts_only_owned_processes() {
        let manager = ProcessManager::default();
        assert_eq!(manager.owned_count(), 0);
    }
}
