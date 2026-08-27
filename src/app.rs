use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender};

use crate::{
    ai::{
        client::{self},
        process::{ProcessManager, ServiceKind, ServiceStatus, resolve_llama_command},
        prompts::{
            DocumentIntent, OVERVIEW_ANALYSIS_MAX_TOKENS, OVERVIEW_SYNTHESIS_MAX_TOKENS,
            document_history_without_evidence, document_intent, document_meta_prompt,
            document_overview_analysis_prompt, general_system_prompt,
            library_overview_synthesis_prompt, prompt_diagnostic, rag_library_overview_prompt,
            rag_system_prompt, render_structured_library_overview, sanitize_document_output,
            validate_overview_analysis,
        },
    },
    code::{
        indexer::{
            CodeLocalProvider, chunk_code, content_hash, embed_code_chunks_cancellable,
            needs_reindex, read_source,
        },
        prompt::{code_history_without_evidence, code_prompt, code_prompt_diagnostic},
        scanner::{scan_project_with_cancel, scan_single_file},
        search::{resolve_file_scope, search_code_scoped, targets_selected_file},
        types::{CodeFile, CodeProject, PROJECT_CANCELLED, PROJECT_FAILED, PROJECT_READY},
    },
    config::Settings,
    db::Database,
    documents,
    models::{Chat, Document, Library, Message, Profile, Source},
    performance::{self, ProcessSampler},
    rag::{
        chunking::chunk_document,
        embeddings::{
            EmbeddingProvider, NomicLocalProvider, embed_chunks_with_retry,
            prepare_embedding_chunks,
        },
        search::{
            build_library_overview, global_literal_terms, lexical_matches, mentioned_documents,
            restrict_to_documents, retrieve, retrieve_comparison,
        },
    },
    ui,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Chat,
    Code,
    Libraries,
    Settings,
}

fn startup_view(
    chat_reachable: bool,
    chat_model_available: bool,
    llama_command_available: bool,
) -> View {
    if chat_reachable || (chat_model_available && llama_command_available) {
        View::Chat
    } else {
        View::Settings
    }
}
enum Event {
    ChatStatus(ServiceStatus),
    EmbeddingStatus(ServiceStatus),
    HealthSnapshot {
        chat_reachable: bool,
        embedding_reachable: bool,
        initial: bool,
        started_at: Instant,
    },
    ChatRequestSucceeded,
    ChatRequestStarted(Duration),
    ChatFirstToken(Duration),
    ChatDelta(i64, String),
    ChatFinished(Result<(i64, String, Vec<Source>), String>),
    PromptDiagnostic(String),
    GenerationStage(Option<String>),
    OverviewMetrics(String),
    ChatCancelled,
    IndexProgress(String),
    IndexFinished(Result<String, String>),
    CodeIndexProgress(String),
    CodeIndexFinished(Result<(i64, String), String>),
    CodeIndexCancelled(Option<i64>),
    #[allow(dead_code)] // Conserved for internal diagnostics; hidden from the normal UX.
    ConfigResult(String),
}

fn mode_key(profile: Profile) -> &'static str {
    match profile {
        Profile::Documentation => "last_chat_id_documentos",
        Profile::Code => "last_chat_id_codigo",
        _ => "last_chat_id_general",
    }
}

fn active_profile(profile: Profile) -> Profile {
    match profile {
        Profile::Documentation => Profile::Documentation,
        Profile::Code => Profile::Code,
        _ => Profile::General,
    }
}

fn chat_view_for_profile(profile: Profile) -> View {
    if profile == Profile::Code {
        View::Code
    } else {
        View::Chat
    }
}

fn active_library_for_sidebar(mode: Profile, active_library_id: Option<i64>) -> Option<i64> {
    (mode == Profile::Documentation)
        .then_some(active_library_id)
        .flatten()
}

fn status_after_health(current: &ServiceStatus, reachable: bool) -> ServiceStatus {
    if reachable {
        if matches!(current, ServiceStatus::Busy) {
            ServiceStatus::Busy
        } else {
            ServiceStatus::Ready
        }
    } else if matches!(current, ServiceStatus::Busy) {
        ServiceStatus::Busy
    } else {
        ServiceStatus::Disconnected
    }
}

fn status_after_request(error: Option<&str>) -> ServiceStatus {
    error
        .map(|message| ServiceStatus::Error(message.to_owned()))
        .unwrap_or(ServiceStatus::Ready)
}

fn chat_status_after_health(
    current: &ServiceStatus,
    reachable: bool,
    check_started: Instant,
    last_success: Option<Instant>,
) -> ServiceStatus {
    if last_success.is_some_and(|success| success > check_started) {
        ServiceStatus::Ready
    } else {
        status_after_health(current, reachable)
    }
}

fn valid_selected_code_file(selected: Option<i64>, files: &[CodeFile]) -> Option<i64> {
    selected.filter(|id| files.iter().any(|file| file.id == *id))
}

fn captured_code_file(
    project_id: i64,
    selected_file_id: Option<i64>,
    files: &[CodeFile],
) -> Option<CodeFile> {
    let selected_file_id = selected_file_id?;
    files
        .iter()
        .find(|file| {
            file.id == selected_file_id && file.project_id == project_id && file.error.is_none()
        })
        .cloned()
}

fn sources_for_code_scope(
    sources: Vec<Source>,
    project_id: i64,
    allowed_file_ids: Option<&[i64]>,
) -> Vec<Source> {
    let mut seen = std::collections::HashSet::new();
    sources
        .into_iter()
        .filter(|source| source.project_id == project_id)
        .filter(|source| allowed_file_ids.is_none_or(|file_ids| file_ids.contains(&source.file_id)))
        .filter(|source| seen.insert((source.project_id, source.file_id, source.chunk_id)))
        .collect()
}

fn sources_for_library(
    sources: Vec<Source>,
    library_id: i64,
    allowed_document_ids: &std::collections::HashSet<i64>,
) -> Vec<Source> {
    let mut seen = std::collections::HashSet::new();
    sources
        .into_iter()
        .filter(|source| allowed_document_ids.contains(&source.document_id))
        .filter(|source| seen.insert((source.document_id, source.page_number, source.chunk_id)))
        .map(|mut source| {
            source.library_id = library_id;
            source
        })
        .collect()
}

fn database_path() -> Result<PathBuf> {
    let directory = std::env::current_dir().context("No se pudo determinar la carpeta de datos")?;
    database_path_in(&directory)
}

fn database_path_in(directory: &Path) -> Result<PathBuf> {
    let current = directory.join("kuznor.db");
    let legacy = directory.join("inference_local.db");
    if current.exists() || !legacy.exists() {
        return Ok(current);
    }
    std::fs::copy(&legacy, &current).with_context(|| {
        format!(
            "No se pudo migrar la base anterior {} a {}",
            legacy.display(),
            current.display()
        )
    })?;
    Ok(current)
}

pub struct KuznorApp {
    db: Database,
    settings: Settings,
    chats: Vec<Chat>,
    messages: Vec<Message>,
    libraries: Vec<Library>,
    documents: Vec<Document>,
    code_projects: Vec<CodeProject>,
    code_files: Vec<CodeFile>,
    logo: egui::TextureHandle,
    active_code_project: Option<i64>,
    selected_code_file: Option<i64>,
    code_status: String,
    code_indexing: bool,
    code_index_cancel: Option<Arc<AtomicBool>>,
    current_chat: Option<i64>,
    mode: Profile,
    active_library_id: Option<i64>,
    view: View,
    input: String,
    new_name: String,
    rename_chat: Option<i64>,
    rename_text: String,
    rename_library: Option<i64>,
    rename_library_text: String,
    pending_delete: Option<i64>,
    pending_delete_library: Option<i64>,
    pending_remove_code_project: Option<i64>,
    chat_search: String,
    notice: Option<String>,
    notice_details: Option<String>,
    prompt_diagnostic: Option<String>,
    prompt_diagnostic_open: bool,
    progress: Option<String>,
    generating: bool,
    generation_cancel: Option<Arc<AtomicBool>>,
    generation_chat_id: Option<i64>,
    streaming_text: String,
    streaming_buffer: String,
    streaming_last_flush: Instant,
    generation_started: Option<Instant>,
    request_started_ms: Option<u128>,
    first_token_ms: Option<u128>,
    generation_ui_updates: u64,
    generation_metrics: Option<String>,
    generation_stage: Option<String>,
    performance_sampler: ProcessSampler,
    performance_sampled_at: Instant,
    performance_metrics: Option<String>,
    indexing: bool,
    chat_status: ServiceStatus,
    embedding_status: ServiceStatus,
    health_check_in_flight: bool,
    health_checked_at: Instant,
    last_chat_success: Option<Instant>,
    events_tx: Sender<Event>,
    events_rx: Receiver<Event>,
    processes: Arc<Mutex<ProcessManager>>,
}

impl KuznorApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self> {
        ui::apply_theme(&cc.egui_ctx);
        let db_path = database_path()?;
        let db = Database::open(db_path)?;
        let settings = Settings::load(&db)?;
        let all_chats = db.list_chats()?;
        for chat in &all_chats {
            if matches!(
                chat.profile,
                Profile::Programming | Profile::Business | Profile::Study
            ) {
                db.update_chat(chat.id, &chat.title, Profile::General, chat.library_id)?;
            }
        }
        let mode = Profile::General;
        let chats = db.list_chats_for_profile(mode)?;
        let current_chat = None;
        let messages = Vec::new();
        let libraries = db.list_libraries()?;
        db.mark_interrupted_code_projects()?;
        let code_projects = db.list_code_projects()?;
        let active_code_project = db
            .load_settings()?
            .get("active_code_project_id")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|id| {
                code_projects.iter().any(|project| {
                    project.id == *id && matches!(project.status.as_str(), "ready" | "listo")
                })
            })
            .or_else(|| {
                code_projects
                    .iter()
                    .find(|project| matches!(project.status.as_str(), "ready" | "listo"))
                    .map(|project| project.id)
            });
        let code_files = active_code_project
            .map(|project_id| db.list_code_files(project_id))
            .transpose()?
            .unwrap_or_default();
        let active_library_id = None;
        let logo = cc.egui_ctx.load_texture(
            "kuznor-brand-logo",
            egui::ColorImage::from(crate::kuznor_icon_data()),
            egui::TextureOptions::LINEAR,
        );
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let mut app = Self {
            db,
            settings,
            chats,
            messages,
            libraries,
            documents: vec![],
            code_projects,
            code_files,
            logo,
            active_code_project,
            selected_code_file: None,
            code_status: "Selecciona un archivo o carpeta".into(),
            code_indexing: false,
            code_index_cancel: None,
            current_chat,
            mode,
            active_library_id,
            view: View::Chat,
            input: String::new(),
            new_name: String::new(),
            rename_chat: None,
            rename_text: String::new(),
            rename_library: None,
            rename_library_text: String::new(),
            pending_delete: None,
            pending_delete_library: None,
            pending_remove_code_project: None,
            chat_search: String::new(),
            notice: None,
            notice_details: None,
            prompt_diagnostic: None,
            prompt_diagnostic_open: false,
            progress: None,
            generating: false,
            generation_cancel: None,
            generation_chat_id: None,
            streaming_text: String::new(),
            streaming_buffer: String::new(),
            streaming_last_flush: Instant::now(),
            generation_started: None,
            request_started_ms: None,
            first_token_ms: None,
            generation_ui_updates: 0,
            generation_metrics: None,
            generation_stage: None,
            performance_sampler: ProcessSampler::default(),
            performance_sampled_at: Instant::now(),
            performance_metrics: None,
            indexing: false,
            chat_status: ServiceStatus::Starting,
            embedding_status: ServiceStatus::Starting,
            health_check_in_flight: false,
            health_checked_at: Instant::now(),
            last_chat_success: None,
            events_tx,
            events_rx,
            processes: Arc::new(Mutex::new(ProcessManager::default())),
        };
        app.schedule_health_check(true);
        Ok(app)
    }

    fn schedule_health_check(&mut self, initial: bool) {
        if self.health_check_in_flight {
            return;
        }
        self.health_check_in_flight = true;
        if initial {
            self.chat_status = ServiceStatus::Starting;
            self.embedding_status = ServiceStatus::Starting;
        }
        let tx = self.events_tx.clone();
        let settings = self.settings.clone();
        let started_at = Instant::now();
        thread::spawn(move || {
            let chat_reachable = client::health(&settings.chat_host, settings.chat_port);
            let embedding_reachable =
                client::health(&settings.embedding_host, settings.embedding_port);
            let _ = tx.send(Event::HealthSnapshot {
                chat_reachable,
                embedding_reachable,
                initial,
                started_at,
            });
        });
    }

    fn reload(&mut self) -> Result<()> {
        self.chats = self.db.list_chats_for_profile(self.mode)?;
        if self
            .current_chat
            .is_some_and(|id| !self.chats.iter().any(|chat| chat.id == id))
        {
            self.current_chat = self.chats.first().map(|chat| chat.id);
        }
        self.libraries = self.db.list_libraries()?;
        self.messages = self
            .current_chat
            .map(|id| self.db.messages(id))
            .transpose()?
            .unwrap_or_default();
        if self
            .active_library_id
            .is_some_and(|id| !self.libraries.iter().any(|library| library.id == id))
        {
            self.active_library_id = None;
        }
        if self.mode == Profile::Documentation {
            if self.active_library_id.is_none() {
                self.active_library_id = self
                    .current()
                    .and_then(|chat| chat.library_id)
                    .filter(|id| self.libraries.iter().any(|library| library.id == *id));
            }
        }
        self.documents = self
            .active_library_id
            .map(|id| self.db.list_documents(id))
            .transpose()?
            .unwrap_or_default();
        self.code_projects = self.db.list_code_projects()?;
        self.code_files = self
            .active_code_project
            .map(|project_id| self.db.list_code_files(project_id))
            .transpose()?
            .unwrap_or_default();
        self.selected_code_file =
            valid_selected_code_file(self.selected_code_file, &self.code_files);
        Ok(())
    }

    fn set_active_library(&mut self, library_id: Option<i64>) -> Result<()> {
        if let Some(id) = library_id {
            if !self.libraries.iter().any(|library| library.id == id) {
                anyhow::bail!("La biblioteca seleccionada ya no existe");
            }
        }
        self.active_library_id = library_id;
        self.documents = library_id
            .map(|id| self.db.list_documents(id))
            .transpose()?
            .unwrap_or_default();
        self.prompt_diagnostic = None;
        Ok(())
    }
    fn error(&mut self, err: impl std::fmt::Display) {
        let details = err.to_string();
        let lower = details.to_lowercase();
        self.notice = Some(
            if lower.contains("roles must alternate")
                || lower.contains("chat template")
                || lower.contains("template")
            {
                "El modelo rechazo el formato de conversacion. Kuznor conservo el historial y no guardo una respuesta fallida."
            } else if lower.contains("sqlite") || lower.contains("database") {
                "No se pudo acceder a los datos locales."
            } else if lower.contains("embedding") {
                "No se pudo procesar la busqueda documental."
            } else if lower.contains("modelo") || lower.contains("llama") {
                "No se pudo iniciar el modelo local."
            } else if lower.contains("conectar") || lower.contains("http") {
                "No se pudo conectar con el servidor de IA."
            } else if lower.contains("pdf") || lower.contains("documento") {
                "No se pudo procesar el documento."
            } else {
                "Kuznor no pudo completar la operacion."
            }
            .into(),
        );
        self.notice_details = Some(details);
    }
    fn current(&self) -> Option<&Chat> {
        self.current_chat
            .and_then(|id| self.chats.iter().find(|c| c.id == id))
    }

    fn generation_visible_for_current_chat(&self) -> bool {
        self.generating && generation_belongs_to_chat(self.current_chat, self.generation_chat_id)
    }

    fn streaming_for_current_chat(&self) -> &str {
        if self.current_chat == self.generation_chat_id {
            &self.streaming_text
        } else {
            ""
        }
    }

    fn switch_mode(&mut self, profile: Profile) {
        let profile = active_profile(profile);
        if profile != self.mode {
            self.input.clear();
        }
        self.mode = profile;
        self.current_chat = self
            .db
            .load_settings()
            .ok()
            .and_then(|settings| settings.get(mode_key(profile)).cloned())
            .and_then(|id| id.parse::<i64>().ok())
            .filter(|id| {
                self.db
                    .list_chats_for_profile(profile)
                    .map(|chats| chats.iter().any(|chat| chat.id == *id))
                    .unwrap_or(false)
            });
        if let Err(error) = self.reload() {
            self.error(error);
            return;
        }
        if let Some(chat_id) = self.current_chat {
            let _ = self
                .db
                .save_setting(mode_key(profile), &chat_id.to_string());
        }
        let _ = self.db.save_setting("active_chat_mode", profile.label());
        self.view = if profile == Profile::Code {
            View::Code
        } else {
            View::Chat
        };
    }

    fn remove_code_project(&mut self, project_id: i64) {
        if self.code_indexing {
            self.notice = Some("Espera a que termine o cancela la indexacion actual.".into());
            return;
        }
        if let Err(error) = self.db.remove_code_project(project_id) {
            self.error(error);
            return;
        }
        let removed_active = self.active_code_project == Some(project_id);
        let projects = match self.db.list_code_projects() {
            Ok(projects) => projects,
            Err(error) => {
                self.error(error);
                return;
            }
        };
        if removed_active
            || self
                .active_code_project
                .is_some_and(|active| !projects.iter().any(|project| project.id == active))
        {
            self.active_code_project = projects
                .iter()
                .find(|project| matches!(project.status.as_str(), "ready" | "listo"))
                .map(|project| project.id);
            self.selected_code_file = None;
        }
        let active_setting = self
            .active_code_project
            .map(|id| id.to_string())
            .unwrap_or_default();
        let _ = self
            .db
            .save_setting("active_code_project_id", &active_setting);
        self.code_status = if self.active_code_project.is_some() {
            "Proyecto activo actualizado".into()
        } else {
            "Selecciona un archivo o carpeta".into()
        };
        if let Err(error) = self.reload() {
            self.error(error);
            return;
        }
        self.notice = Some(
            "Proyecto quitado de Kuznor. Los archivos originales no fueron modificados.".into(),
        );
    }

    #[allow(dead_code)] // Managed-service support remains available outside the simplified UI.
    fn start_service(&mut self, kind: ServiceKind) {
        let tx = self.events_tx.clone();
        let settings = self.settings.clone();
        let processes = self.processes.clone();
        thread::spawn(move || {
            let status_event = |s| match kind {
                ServiceKind::Chat => Event::ChatStatus(s),
                ServiceKind::Embedding => Event::EmbeddingStatus(s),
            };
            let _ = tx.send(status_event(ServiceStatus::Starting));
            let result = processes
                .lock()
                .map_err(|_| anyhow::anyhow!("Gestor de procesos bloqueado"))
                .and_then(|mut p| p.ensure(kind, &settings));
            let _ = tx.send(status_event(match result {
                Ok(_) => ServiceStatus::Ready,
                Err(e) => ServiceStatus::Error(e.to_string()),
            }));
        });
    }
    #[allow(dead_code)] // Managed-service support remains available outside the simplified UI.
    fn stop_service(&mut self, kind: ServiceKind) {
        let result = self
            .processes
            .lock()
            .map_err(|_| anyhow::anyhow!("Gestor de procesos bloqueado"))
            .and_then(|mut p| p.stop_owned(kind));
        match result {
            Ok(true) => match kind {
                ServiceKind::Chat => self.chat_status = ServiceStatus::Disconnected,
                ServiceKind::Embedding => self.embedding_status = ServiceStatus::Disconnected,
            },
            Ok(false) => {
                self.notice =
                    Some("Ese servidor no fue iniciado por la aplicacion y no se cerrara.".into())
            }
            Err(e) => self.error(e),
        }
    }

    fn send(&mut self) {
        let question = self.input.trim().to_owned();
        if question.is_empty() || self.generating {
            return;
        }
        if self.mode == Profile::Code {
            let Some(project_id) = self.active_code_project else {
                self.notice = Some("No hay un proyecto valido seleccionado.".into());
                return;
            };
            if targets_selected_file(&question)
                && captured_code_file(project_id, self.selected_code_file, &self.code_files)
                    .is_none()
            {
                self.notice = Some("No hay un archivo valido seleccionado.".into());
                return;
            }
        }
        let turn_library_id = (self.mode == Profile::Documentation)
            .then_some(self.active_library_id)
            .flatten();
        let chat = if let Some(chat) = self.current().cloned() {
            chat
        } else {
            let id = match self.db.create_chat("Nuevo chat", self.mode) {
                Ok(id) => id,
                Err(error) => {
                    self.error(error);
                    return;
                }
            };
            self.current_chat = Some(id);
            let _ = self.db.save_setting(mode_key(self.mode), &id.to_string());
            match self.db.list_chats_for_profile(self.mode).and_then(|chats| {
                chats
                    .into_iter()
                    .find(|candidate| candidate.id == id)
                    .context("El chat recién creado no está disponible")
            }) {
                Ok(chat) => chat,
                Err(error) => {
                    self.error(error);
                    return;
                }
            }
        };
        let history = match chat.profile {
            Profile::Documentation => document_history_without_evidence(&self.messages),
            Profile::Code => code_history_without_evidence(&self.messages),
            _ => self.messages.clone(),
        };
        if let Err(e) = self.db.add_message(chat.id, "user", &question, &[]) {
            self.error(e);
            return;
        }
        if chat.title == "Nuevo chat" {
            let title = automatic_title(&question);
            if let Err(e) = self
                .db
                .update_chat(chat.id, &title, chat.profile, chat.library_id)
            {
                self.error(e);
            }
        }
        self.input.clear();
        let _ = self.reload();
        self.generating = true;
        self.streaming_text.clear();
        self.streaming_buffer.clear();
        self.streaming_last_flush = Instant::now();
        self.generation_started = Some(Instant::now());
        self.request_started_ms = None;
        self.first_token_ms = None;
        self.generation_ui_updates = 0;
        self.generation_metrics = None;
        self.generation_stage = None;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.generation_cancel = Some(cancelled.clone());
        self.generation_chat_id = Some(chat.id);
        let tx = self.events_tx.clone();
        let db = self.db.clone();
        let settings = self.settings.clone();
        let processes = self.processes.clone();
        let active_code_project = self.active_code_project;
        let selected_code_file = self.selected_code_file;
        let code_files = self.code_files.clone();
        let captured_selected_code_file = active_code_project
            .and_then(|project_id| captured_code_file(project_id, selected_code_file, &code_files));
        let generation_started = self.generation_started.unwrap_or_else(Instant::now);
        let code_project_name = active_code_project
            .and_then(|project_id| {
                self.code_projects
                    .iter()
                    .find(|project| project.id == project_id)
                    .map(|project| project.name.clone())
            })
            .unwrap_or_else(|| "Sin proyecto".into());
        let library_name = turn_library_id
            .and_then(|library_id| {
                self.libraries
                    .iter()
                    .find(|library| library.id == library_id)
                    .map(|library| library.name.clone())
            })
            .unwrap_or_else(|| "Sin biblioteca".into());
        thread::spawn(move || {
            let result = (|| -> Result<(i64, String, Vec<Source>)> {
                let mut retrieved = Vec::new();
                let mut retrieved_code = Vec::new();
                let mut overview_inputs = Vec::new();
                let mut overview_retrieval_ms = None;
                let mut direct_answer = None;
                let mut document_turn_uses_evidence = false;
                let mut allowed_document_ids = std::collections::HashSet::new();
                let mut code_allowed_file_ids = None;
                let (mut system, sources) = if chat.profile == Profile::Code {
                    if let Some(project_id) = active_code_project {
                        tx.send(Event::EmbeddingStatus(ServiceStatus::Starting))
                            .ok();
                        processes
                            .lock()
                            .map_err(|_| anyhow::anyhow!("Gestor de procesos bloqueado"))?
                            .ensure(ServiceKind::Embedding, &settings)?;
                        tx.send(Event::EmbeddingStatus(ServiceStatus::Busy)).ok();
                        let provider = NomicLocalProvider {
                            settings: &settings,
                        };
                        let query_embedding = provider.embed_query(&question)?;
                        let file_scope =
                            resolve_file_scope(&code_files, &question, selected_code_file);
                        code_allowed_file_ids = file_scope.clone();
                        let found = search_code_scoped(
                            db.code_chunks(project_id)?,
                            &query_embedding,
                            &question,
                            settings.top_k,
                            file_scope.as_deref(),
                        );
                        let prompt = code_prompt(
                            &found,
                            &code_project_name,
                            captured_selected_code_file.as_ref(),
                        );
                        retrieved_code = found;
                        if processes
                            .lock()
                            .map(|manager| manager.owns(ServiceKind::Embedding))
                            .unwrap_or(false)
                        {
                            let _ = processes
                                .lock()
                                .map(|mut manager| manager.stop_owned(ServiceKind::Embedding));
                            tx.send(Event::EmbeddingStatus(ServiceStatus::Disconnected))
                                .ok();
                        } else {
                            tx.send(Event::EmbeddingStatus(ServiceStatus::Ready)).ok();
                        }
                        prompt
                    } else {
                        code_prompt(&[], &code_project_name, None)
                    }
                } else if let Some(library_id) =
                    turn_library_id.filter(|_| chat.profile == Profile::Documentation)
                {
                    allowed_document_ids = db
                        .list_documents(library_id)?
                        .into_iter()
                        .filter(|document| document.status == "listo")
                        .map(|document| document.id)
                        .collect();
                    match document_intent(&question) {
                        DocumentIntent::Meta => (document_meta_prompt(), Vec::new()),
                        DocumentIntent::LibraryOverview => {
                            document_turn_uses_evidence = true;
                            let retrieval_started = Instant::now();
                            let documents = db
                                .list_documents(library_id)?
                                .into_iter()
                                .filter(|document| document.status == "listo")
                                .map(|document| (document.id, document.name))
                                .collect::<Vec<_>>();
                            let overview = build_library_overview(
                                db.library_chunks(library_id)?,
                                &documents,
                                3,
                            );
                            let found = overview
                                .iter()
                                .flat_map(|document| document.representative_chunks.clone())
                                .collect::<Vec<_>>();
                            let prompt = rag_library_overview_prompt(&found, &documents);
                            retrieved = found;
                            overview_inputs = overview;
                            overview_retrieval_ms = Some(retrieval_started.elapsed().as_millis());
                            prompt
                        }
                        DocumentIntent::DocumentQuery => {
                            document_turn_uses_evidence = true;
                            let library_chunks = db.library_chunks(library_id)?;
                            let literal_terms = global_literal_terms(&question);
                            let (found, prompt) = if !literal_terms.is_empty() {
                                let found = lexical_matches(library_chunks, &literal_terms);
                                if found.is_empty() {
                                    direct_answer = Some(format!(
                                        "No se encontro el termino exacto '{}' tras revisar los {} documentos disponibles en la biblioteca {}.",
                                        literal_terms.join("', '"),
                                        allowed_document_ids.len(),
                                        library_name
                                    ));
                                }
                                let prompt = rag_system_prompt(chat.profile, &found);
                                (found, prompt)
                            } else {
                                tx.send(Event::EmbeddingStatus(ServiceStatus::Starting))
                                    .ok();
                                processes
                                    .lock()
                                    .map_err(|_| anyhow::anyhow!("Gestor de procesos bloqueado"))?
                                    .ensure(ServiceKind::Embedding, &settings)?;
                                tx.send(Event::EmbeddingStatus(ServiceStatus::Busy)).ok();
                                let provider = NomicLocalProvider {
                                    settings: &settings,
                                };
                                let query_embedding = provider.embed_query(&question)?;
                                let mentioned = mentioned_documents(&library_chunks, &question);
                                let found = if mentioned.len() > 1 {
                                    retrieve_comparison(
                                        library_chunks,
                                        &query_embedding,
                                        &question,
                                        settings.top_k,
                                        &mentioned,
                                    )
                                } else if !mentioned.is_empty() {
                                    retrieve(
                                        restrict_to_documents(library_chunks, &mentioned),
                                        &query_embedding,
                                        &question,
                                        settings.top_k,
                                    )
                                } else {
                                    retrieve(
                                        library_chunks,
                                        &query_embedding,
                                        &question,
                                        settings.top_k,
                                    )
                                };
                                let prompt = if mentioned.len() > 1 {
                                    crate::ai::prompts::rag_system_prompt_with_documents(
                                        chat.profile,
                                        &found,
                                        &mentioned,
                                    )
                                } else {
                                    rag_system_prompt(chat.profile, &found)
                                };
                                (found, prompt)
                            };
                            retrieved = found;
                            if literal_terms.is_empty()
                                && processes
                                    .lock()
                                    .map(|p| p.owns(ServiceKind::Embedding))
                                    .unwrap_or(false)
                            {
                                let _ = processes
                                    .lock()
                                    .map(|mut p| p.stop_owned(ServiceKind::Embedding));
                                tx.send(Event::EmbeddingStatus(ServiceStatus::Disconnected))
                                    .ok();
                            } else {
                                tx.send(Event::EmbeddingStatus(ServiceStatus::Ready)).ok();
                            }
                            prompt
                        }
                    }
                } else {
                    let prompt = if chat.profile == Profile::General {
                        general_system_prompt(&settings.chat_model_path)
                    } else {
                        chat.profile.system_prompt().into()
                    };
                    (prompt, vec![])
                };
                let sources = if let Some(project_id) =
                    active_code_project.filter(|_| chat.profile == Profile::Code)
                {
                    sources_for_code_scope(sources, project_id, code_allowed_file_ids.as_deref())
                } else if let Some(library_id) = turn_library_id {
                    sources_for_library(sources, library_id, &allowed_document_ids)
                } else {
                    sources
                };
                if let Some(library_id) = turn_library_id.filter(|_| document_turn_uses_evidence) {
                    let filenames = db
                        .list_documents(library_id)?
                        .into_iter()
                        .filter(|document| allowed_document_ids.contains(&document.id))
                        .map(|document| format!("- {}", document.name))
                        .collect::<Vec<_>>()
                        .join("\n");
                    system.push_str(&format!(
                        "\n\nCONTEXTO DOCUMENTAL ACTUAL\nBiblioteca actual: {library_name}\nDocumentos permitidos:\n{filenames}\nSolo este contexto y sus fragmentos son evidencia documental para el turno actual. El historial conversacional no es una fuente y no puede atribuirse a estos archivos."
                    ));
                }
                if let Some(answer) = direct_answer {
                    let answer = sanitize_document_output(&answer);
                    tx.send(Event::ChatRequestStarted(generation_started.elapsed()))
                        .ok();
                    db.add_message(chat.id, "assistant", &answer, &sources)?;
                    tx.send(Event::ChatRequestSucceeded).ok();
                    return Ok((chat.id, answer, sources));
                }
                tx.send(Event::ChatStatus(ServiceStatus::Starting)).ok();
                processes
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Gestor de procesos bloqueado"))?
                    .ensure(ServiceKind::Chat, &settings)?;
                tx.send(Event::ChatStatus(ServiceStatus::Busy)).ok();
                tx.send(Event::ChatRequestStarted(generation_started.elapsed()))
                    .ok();
                let mut overview_analyses = Vec::new();
                let overview_started = Instant::now();
                let overview_preparation_ms = generation_started.elapsed().as_millis();
                let mut overview_document_timings = Vec::new();
                let mut synthesis_started = None;
                if !overview_inputs.is_empty() {
                    for (index, document) in overview_inputs.iter().enumerate() {
                        if cancelled.load(Ordering::Relaxed) {
                            anyhow::bail!("Generacion cancelada");
                        }
                        tx.send(Event::GenerationStage(Some(format!(
                            "Analizando documento {} de {}...",
                            index + 1,
                            overview_inputs.len()
                        ))))
                        .ok();
                        let document_started = Instant::now();
                        let analysis_system =
                            document_overview_analysis_prompt(document, &question);
                        let mut analysis_settings = settings.clone();
                        analysis_settings.max_tokens =
                            settings.max_tokens.min(OVERVIEW_ANALYSIS_MAX_TOKENS);
                        let analysis_messages = client::trim_history(
                            analysis_system,
                            &[],
                            "Produce el analisis individual de este documento.",
                            analysis_settings.context_size,
                            analysis_settings.max_tokens,
                        );
                        let analysis_prepared =
                            client::prepare_messages(&analysis_settings, &analysis_messages)?;
                        let analysis = client::chat_stream_prepared(
                            &analysis_settings,
                            &analysis_prepared,
                            &cancelled,
                            |_| {},
                        )?;
                        overview_analyses.push((
                            document.document_id,
                            document.filename.clone(),
                            validate_overview_analysis(document, &analysis),
                        ));
                        overview_document_timings.push(format!(
                            "documento {}/{}: {} ms",
                            index + 1,
                            overview_inputs.len(),
                            document_started.elapsed().as_millis()
                        ));
                    }
                    tx.send(Event::GenerationStage(Some("Sintetizando...".into())))
                        .ok();
                    synthesis_started = Some(Instant::now());
                    system = library_overview_synthesis_prompt(&overview_analyses, &question);
                }
                let final_history = if overview_analyses.is_empty() {
                    history.clone()
                } else {
                    Vec::new()
                };
                let final_question = if overview_analyses.is_empty() {
                    question.clone()
                } else {
                    "Produce la sintesis global sin repetir las secciones por documento.".into()
                };
                let mut final_settings = settings.clone();
                if !overview_analyses.is_empty() {
                    final_settings.max_tokens =
                        settings.max_tokens.min(OVERVIEW_SYNTHESIS_MAX_TOKENS);
                }
                let messages = client::trim_history(
                    system,
                    &final_history,
                    &final_question,
                    final_settings.context_size,
                    final_settings.max_tokens,
                );
                let prepared = client::prepare_messages(&final_settings, &messages)?;
                if settings.diagnostic_mode {
                    let diagnostic = if chat.profile == Profile::Code {
                        Some(code_prompt_diagnostic(
                            &prepared.messages[0].content,
                            &question,
                            &code_project_name,
                            captured_selected_code_file.as_ref(),
                            &retrieved_code,
                            &prepared.messages,
                        ))
                    } else if turn_library_id.is_some() && chat.profile == Profile::Documentation {
                        Some(prompt_diagnostic(
                            &prepared.messages[0].content,
                            &question,
                            &library_name,
                            &retrieved,
                            &prepared.messages,
                        ))
                    } else {
                        None
                    };
                    if let Some(diagnostic) = diagnostic {
                        tx.send(Event::PromptDiagnostic(diagnostic)).ok();
                    }
                }
                let mut received_stream = false;
                let mut first_token_sent = false;
                let streamed =
                    client::chat_stream_prepared(&final_settings, &prepared, &cancelled, |delta| {
                        received_stream = true;
                        if !first_token_sent {
                            first_token_sent = true;
                            tx.send(Event::ChatFirstToken(generation_started.elapsed()))
                                .ok();
                        }
                        tx.send(Event::ChatDelta(chat.id, delta.to_owned())).ok();
                    });
                let answer = match streamed {
                    Ok(answer) => answer,
                    Err(error)
                        if !received_stream
                            && !cancelled.load(Ordering::Relaxed)
                            && !client::is_template_error(&error) =>
                    {
                        client::chat_prepared(&final_settings, &prepared)?
                    }
                    Err(error) => return Err(error),
                };
                let answer = if overview_analyses.is_empty() {
                    if chat.profile == Profile::Documentation {
                        sanitize_document_output(&answer)
                    } else {
                        answer
                    }
                } else {
                    render_structured_library_overview(&overview_analyses, &answer, &question)
                };
                if !overview_analyses.is_empty() {
                    let synthesis_ms = synthesis_started
                        .map(|started| started.elapsed().as_millis())
                        .unwrap_or_default();
                    tx.send(Event::OverviewMetrics(format!(
                        "overview: preparacion {} ms | retrieval representativo {} ms | {} | sintesis {} ms | total {} ms | limite ficha {} tokens | limite sintesis {} tokens",
                        overview_preparation_ms,
                        overview_retrieval_ms.unwrap_or_default(),
                        overview_document_timings.join(" | "),
                        synthesis_ms,
                        overview_started.elapsed().as_millis(),
                        settings.max_tokens.min(OVERVIEW_ANALYSIS_MAX_TOKENS),
                        final_settings.max_tokens
                    )))
                    .ok();
                }
                if cancelled.load(Ordering::Relaxed) {
                    tx.send(Event::ChatStatus(ServiceStatus::Ready)).ok();
                    return Ok((chat.id, String::new(), Vec::new()));
                }
                db.add_message(chat.id, "assistant", &answer, &sources)?;
                tx.send(Event::ChatRequestSucceeded).ok();
                Ok((chat.id, answer, sources))
            })();
            if cancelled.load(Ordering::Relaxed) {
                let _ = tx.send(Event::ChatCancelled);
            } else {
                let result = result.map_err(|error| error.to_string());
                if let Err(error) = &result {
                    let _ = tx.send(Event::ChatStatus(status_after_request(Some(error))));
                }
                let _ = tx.send(Event::ChatFinished(result));
            }
        });
    }

    fn cancel_generation(&mut self) {
        if let Some(cancelled) = &self.generation_cancel {
            cancelled.store(true, Ordering::Relaxed);
            self.notice = Some("Cancelando la respuesta actual...".into());
        }
    }

    fn index_path(&mut self, library_id: i64, path: PathBuf, reindex_id: Option<i64>) {
        if self.indexing {
            return;
        }
        self.indexing = true;
        let tx = self.events_tx.clone();
        let db = self.db.clone();
        let settings = self.settings.clone();
        let processes = self.processes.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String> {
                tx.send(Event::IndexProgress("Extrayendo texto...".into()))
                    .ok();
                let hash = documents::file_hash(&path)?;
                let name = path
                    .file_name()
                    .and_then(|v| v.to_str())
                    .context("Nombre de archivo invalido")?
                    .to_owned();
                let kind = path
                    .extension()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let parsed = documents::parse(&path)?;
                let document_id = if let Some(id) = reindex_id {
                    db.replace_document(id, &path.to_string_lossy(), &name, &kind)?;
                    id
                } else if db.document_by_hash(library_id, &hash)?.is_some() {
                    anyhow::bail!(
                        "Este documento ya existe en la biblioteca. Usa Reindexar para reemplazar sus chunks."
                    );
                } else {
                    db.create_document(library_id, &name, &path.to_string_lossy(), &hash, &kind)?
                };
                let work = (|| -> Result<usize> {
                    tx.send(Event::IndexProgress("Dividiendo documento...".into()))
                        .ok();
                    let mut chunks =
                        chunk_document(&parsed, settings.chunk_size, settings.chunk_overlap);
                    if chunks.is_empty() {
                        anyhow::bail!("El documento no produjo fragmentos utiles.");
                    }
                    for chunk in &mut chunks {
                        chunk.document_id = document_id;
                    }
                    chunks = prepare_embedding_chunks(chunks);
                    tx.send(Event::EmbeddingStatus(ServiceStatus::Starting))
                        .ok();
                    processes
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Gestor de procesos bloqueado"))?
                        .ensure(ServiceKind::Embedding, &settings)?;
                    tx.send(Event::EmbeddingStatus(ServiceStatus::Busy)).ok();
                    let total_chunks = chunks.len();
                    let mut embedded_chunks = Vec::with_capacity(total_chunks);
                    for (batch_index, batch) in chunks.chunks(8).enumerate() {
                        let done = (batch_index * 8).min(total_chunks) + batch.len();
                        tx.send(Event::IndexProgress(format!(
                            "Generando embeddings {done}/{}...",
                            total_chunks
                        )))
                        .ok();
                        let provider = NomicLocalProvider {
                            settings: &settings,
                        };
                        embedded_chunks.extend(embed_chunks_with_retry(&provider, batch.to_vec())?);
                    }
                    for (chunk_index, chunk) in embedded_chunks.iter_mut().enumerate() {
                        chunk.chunk_index = chunk_index;
                    }
                    tx.send(Event::IndexProgress("Guardando...".into())).ok();
                    db.save_chunks(document_id, &embedded_chunks)?;
                    Ok(embedded_chunks.len())
                })();
                if let Err(e) = &work {
                    db.set_document_status(document_id, "error", Some(&e.to_string()))
                        .ok();
                }
                if processes
                    .lock()
                    .map(|p| p.owns(ServiceKind::Embedding))
                    .unwrap_or(false)
                {
                    let _ = processes
                        .lock()
                        .map(|mut p| p.stop_owned(ServiceKind::Embedding));
                    tx.send(Event::EmbeddingStatus(ServiceStatus::Disconnected))
                        .ok();
                } else {
                    tx.send(Event::EmbeddingStatus(ServiceStatus::Ready)).ok();
                }
                let count = work?;
                Ok(format!("Documento listo: {name} ({count} fragmentos)."))
            })();
            let _ = tx.send(Event::IndexFinished(result.map_err(|e| e.to_string())));
        });
    }

    fn index_code_path(&mut self, path: PathBuf, single_file: bool) {
        if self.code_indexing {
            return;
        }
        self.code_indexing = true;
        self.code_status = "Escaneando proyecto...".into();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.code_index_cancel = Some(cancelled.clone());
        let tx = self.events_tx.clone();
        let db = self.db.clone();
        let settings = self.settings.clone();
        let processes = self.processes.clone();
        thread::spawn(move || {
            let mut indexed_project_id = None;
            let result = (|| -> Result<(i64, String)> {
                let root = if single_file {
                    path.parent()
                        .context("El archivo no tiene carpeta")?
                        .to_path_buf()
                } else {
                    path.clone()
                };
                let root = root.canonicalize()?;
                let name = root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Proyecto")
                    .to_owned();
                let project_id = db.upsert_code_project(&name, &root.to_string_lossy())?;
                indexed_project_id = Some(project_id);
                let report = if single_file {
                    crate::code::types::ScanReport {
                        files: vec![scan_single_file(&path)?],
                        skipped: 0,
                        errors: Vec::new(),
                    }
                } else {
                    scan_project_with_cancel(&root, &cancelled)?
                };
                if cancelled.load(Ordering::Relaxed) {
                    anyhow::bail!("Indexacion cancelada");
                }
                tx.send(Event::CodeIndexProgress(format!(
                    "Indexando... {} archivos detectados",
                    report.files.len()
                )))
                .ok();
                let existing = db
                    .list_code_files(project_id)?
                    .into_iter()
                    .map(|file| (file.relative_path.clone(), file))
                    .collect::<HashMap<_, _>>();
                let mut pending = Vec::new();
                let mut unchanged = 0;
                for file in &report.files {
                    if cancelled.load(Ordering::Relaxed) {
                        anyhow::bail!("Indexacion cancelada");
                    }
                    match read_source(&file.absolute_path) {
                        Ok(content) => {
                            let hash = content_hash(&content);
                            let previous = existing.get(&file.relative_path);
                            if previous.is_some_and(|old| {
                                old.error.is_none() && !needs_reindex(Some(&old.hash), &hash)
                            }) {
                                unchanged += 1;
                            } else {
                                pending.push((file.clone(), content, hash));
                            }
                        }
                        Err(error) => {
                            db.upsert_code_file(
                                project_id,
                                &file.relative_path,
                                "",
                                &file.extension,
                                &file.language,
                                file.size_bytes,
                                Some(&error.to_string()),
                            )?;
                        }
                    }
                }
                if !pending.is_empty() {
                    tx.send(Event::EmbeddingStatus(ServiceStatus::Starting))
                        .ok();
                    processes
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Gestor de procesos bloqueado"))?
                        .ensure(ServiceKind::Embedding, &settings)?;
                    tx.send(Event::EmbeddingStatus(ServiceStatus::Busy)).ok();
                }
                let provider = CodeLocalProvider {
                    settings: settings.clone(),
                    cancelled: cancelled.clone(),
                };
                let mut failed = report.errors.len();
                for (position, (file, content, hash)) in pending.into_iter().enumerate() {
                    if cancelled.load(Ordering::Relaxed) {
                        anyhow::bail!("Indexacion cancelada");
                    }
                    tx.send(Event::CodeIndexProgress(format!(
                        "Indexando... {} ({}/{})",
                        file.relative_path,
                        position + 1,
                        report.files.len().saturating_sub(unchanged)
                    )))
                    .ok();
                    let file_id = db.upsert_code_file(
                        project_id,
                        &file.relative_path,
                        &hash,
                        &file.extension,
                        &file.language,
                        file.size_bytes,
                        None,
                    )?;
                    let chunks = chunk_code(
                        project_id,
                        file_id,
                        &file.relative_path,
                        &file.extension,
                        &file.language,
                        &content,
                    );
                    match embed_code_chunks_cancellable(&provider, chunks, &cancelled) {
                        Ok(chunks) => {
                            if cancelled.load(Ordering::Relaxed) {
                                anyhow::bail!("Indexacion cancelada");
                            }
                            db.save_code_chunks(file_id, &chunks)?;
                        }
                        Err(error) => {
                            failed += 1;
                            db.upsert_code_file(
                                project_id,
                                &file.relative_path,
                                &hash,
                                &file.extension,
                                &file.language,
                                file.size_bytes,
                                Some(&error.to_string()),
                            )?;
                        }
                    }
                }
                if !single_file {
                    if cancelled.load(Ordering::Relaxed) {
                        anyhow::bail!("Indexacion cancelada");
                    }
                    db.delete_missing_code_files(
                        project_id,
                        &report
                            .files
                            .iter()
                            .map(|file| file.relative_path.clone())
                            .collect::<Vec<_>>(),
                    )?;
                }
                db.set_code_project_status(project_id, PROJECT_READY)?;
                if processes
                    .lock()
                    .map(|manager| manager.owns(ServiceKind::Embedding))
                    .unwrap_or(false)
                {
                    let _ = processes
                        .lock()
                        .map(|mut manager| manager.stop_owned(ServiceKind::Embedding));
                    tx.send(Event::EmbeddingStatus(ServiceStatus::Disconnected))
                        .ok();
                } else if !report.files.is_empty() {
                    tx.send(Event::EmbeddingStatus(ServiceStatus::Ready)).ok();
                }
                let chunks = db.code_chunks(project_id)?.len();
                Ok((
                    project_id,
                    format!(
                        "Proyecto listo: {} archivos, {} fragmentos, {} sin cambios, {} errores",
                        report.files.len(),
                        chunks,
                        unchanged,
                        failed
                    ),
                ))
            })();
            if cancelled.load(Ordering::Relaxed) {
                if let Some(project_id) = indexed_project_id {
                    let _ = db.set_code_project_status(project_id, PROJECT_CANCELLED);
                }
                let _ = tx.send(Event::CodeIndexCancelled(indexed_project_id));
            } else {
                if result.is_err() {
                    if let Some(project_id) = indexed_project_id {
                        let _ = db.set_code_project_status(project_id, PROJECT_FAILED);
                    }
                }
                let _ = tx.send(Event::CodeIndexFinished(
                    result.map_err(|error| error.to_string()),
                ));
            }
            if processes
                .lock()
                .map(|manager| manager.owns(ServiceKind::Embedding))
                .unwrap_or(false)
            {
                let _ = processes
                    .lock()
                    .map(|mut manager| manager.stop_owned(ServiceKind::Embedding));
                tx.send(Event::EmbeddingStatus(ServiceStatus::Disconnected))
                    .ok();
            }
        });
    }

    fn cancel_code_indexing(&mut self) {
        if let Some(cancelled) = &self.code_index_cancel {
            cancelled.store(true, Ordering::Relaxed);
            self.code_status = "Cancelando indexacion...".into();
        }
    }

    fn finish_generation_metrics(&mut self) {
        let total_ms = self
            .generation_started
            .map(|started| started.elapsed().as_millis());
        if !self.settings.diagnostic_mode {
            self.generation_started = None;
            return;
        }
        let metrics = format!(
            "request desde Enviar: {} ms | primer token: {} ms | total: {} ms | actualizaciones UI: {}",
            self.request_started_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "n/d".into()),
            self.first_token_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "n/d".into()),
            total_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "n/d".into()),
            self.generation_ui_updates
        );
        self.generation_metrics = Some(metrics.clone());
        if let Some(diagnostic) = &mut self.prompt_diagnostic {
            diagnostic.push_str("\n\nDIAGNOSTICO DE LATENCIA LOCAL\n");
            diagnostic.push_str(&metrics);
            diagnostic.push('\n');
        }
        self.generation_started = None;
    }

    fn update_performance_metrics(&mut self) {
        if !self.settings.diagnostic_mode
            || !performance::should_sample(self.performance_sampled_at)
        {
            return;
        }
        self.performance_sampled_at = Instant::now();
        let sample = self.performance_sampler.sample();
        let owned_processes = self
            .processes
            .lock()
            .map(|processes| processes.owned_count())
            .unwrap_or(0);
        self.performance_metrics = Some(format!(
            "CPU Kuznor: {} | RAM Kuznor: {} | llama.cpp administrados: {} | \
             IA principal: {} | Búsqueda semántica: {} | indexaciones activas: {}",
            sample
                .cpu_percent
                .map(|value| format!("{value:.1}%"))
                .unwrap_or_else(|| "n/d".into()),
            sample
                .memory_bytes
                .map(performance::format_memory)
                .unwrap_or_else(|| "n/d".into()),
            owned_processes,
            service_status_label(&self.chat_status),
            service_status_label(&self.embedding_status),
            usize::from(self.indexing) + usize::from(self.code_indexing),
        ));
    }

    fn process_events(&mut self, ctx: &egui::Context) {
        self.update_performance_metrics();
        let mut immediate_repaint = false;
        while let Ok(event) = self.events_rx.try_recv() {
            if !matches!(&event, Event::ChatDelta(..) | Event::ChatFirstToken(..)) {
                immediate_repaint = true;
            }
            match event {
                Event::ChatStatus(s) => self.chat_status = s,
                Event::EmbeddingStatus(s) => self.embedding_status = s,
                Event::ChatRequestSucceeded => {
                    self.last_chat_success = Some(Instant::now());
                    self.chat_status = status_after_request(None);
                }
                Event::HealthSnapshot {
                    chat_reachable,
                    embedding_reachable,
                    initial,
                    started_at,
                } => {
                    self.health_check_in_flight = false;
                    self.health_checked_at = Instant::now();
                    self.chat_status = chat_status_after_health(
                        &self.chat_status,
                        chat_reachable,
                        started_at,
                        self.last_chat_success,
                    );
                    self.embedding_status =
                        status_after_health(&self.embedding_status, embedding_reachable);
                    if initial {
                        let chat_model_available =
                            Path::new(&self.settings.chat_model_path).exists();
                        let llama_command_available =
                            resolve_llama_command(&self.settings.llama_command).is_some();
                        if startup_view(
                            chat_reachable,
                            chat_model_available,
                            llama_command_available,
                        ) == View::Settings
                            && self.current_chat.is_none()
                            && self.mode == Profile::General
                            && self.view == View::Chat
                        {
                            self.view = View::Settings;
                        }
                    }
                }
                Event::ChatRequestStarted(elapsed) => {
                    self.request_started_ms = Some(elapsed.as_millis());
                }
                Event::ChatFirstToken(elapsed) => {
                    self.first_token_ms = Some(elapsed.as_millis());
                }
                Event::PromptDiagnostic(diagnostic) => self.prompt_diagnostic = Some(diagnostic),
                Event::GenerationStage(stage) => self.generation_stage = stage,
                Event::OverviewMetrics(metrics) => {
                    if self.settings.diagnostic_mode {
                        let diagnostic = self.prompt_diagnostic.get_or_insert_with(String::new);
                        diagnostic.push_str("\n\nDIAGNOSTICO DE OVERVIEW LOCAL\n");
                        diagnostic.push_str(&metrics);
                        diagnostic.push('\n');
                    }
                }
                Event::ChatDelta(chat_id, delta) => {
                    if self.generation_chat_id == Some(chat_id) {
                        self.streaming_buffer.push_str(&delta);
                    }
                }
                Event::ChatFinished(result) => {
                    self.finish_generation_metrics();
                    self.generating = false;
                    self.generation_cancel = None;
                    self.generation_chat_id = None;
                    self.generation_stage = None;
                    self.streaming_buffer.clear();
                    self.streaming_text.clear();
                    match result {
                        Ok((chat_id, _, _)) => {
                            if self.current_chat == Some(chat_id) {
                                if let Err(e) = self.reload() {
                                    self.error(e);
                                }
                            }
                        }
                        Err(e) => self.error(e),
                    }
                }
                Event::ChatCancelled => {
                    self.finish_generation_metrics();
                    self.generating = false;
                    self.generation_cancel = None;
                    self.generation_chat_id = None;
                    self.generation_stage = None;
                    self.streaming_buffer.clear();
                    self.streaming_text.clear();
                    self.notice = Some("Generacion detenida. La respuesta no fue guardada.".into());
                }
                Event::IndexProgress(p) => self.progress = Some(p),
                Event::IndexFinished(result) => {
                    self.indexing = false;
                    self.progress = None;
                    match result {
                        Ok(message) => self.notice = Some(message),
                        Err(e) => self.error(e),
                    }
                    if let Err(e) = self.reload() {
                        self.error(e);
                    }
                }
                Event::CodeIndexProgress(progress) => self.code_status = progress,
                Event::CodeIndexFinished(result) => {
                    self.code_indexing = false;
                    self.code_index_cancel = None;
                    match result {
                        Ok((project_id, status)) => {
                            self.active_code_project = Some(project_id);
                            self.code_status = status;
                            let _ = self
                                .db
                                .save_setting("active_code_project_id", &project_id.to_string());
                            if let Err(error) = self.reload() {
                                self.error(error);
                            }
                        }
                        Err(error) => {
                            self.code_status = "No se pudo indexar el proyecto".into();
                            self.error(error);
                        }
                    }
                }
                Event::CodeIndexCancelled(project_id) => {
                    self.code_indexing = false;
                    self.code_index_cancel = None;
                    if let Some(project_id) = project_id {
                        self.active_code_project = Some(project_id);
                    }
                    self.code_status = "Indexacion cancelada".into();
                    self.notice = Some(
                        "Indexacion cancelada. El proyecto quedo marcado como incompleto.".into(),
                    );
                    if let Err(error) = self.reload() {
                        self.error(error);
                    }
                }
                Event::ConfigResult(message) => self.notice = Some(message),
            }
        }
        if self.generating
            && !self.streaming_buffer.is_empty()
            && self.streaming_last_flush.elapsed() >= Duration::from_millis(40)
        {
            self.streaming_text.push_str(&self.streaming_buffer);
            self.streaming_buffer.clear();
            self.streaming_last_flush = Instant::now();
            self.generation_ui_updates += 1;
        }
        if !self.health_check_in_flight
            && !self.generating
            && !self.indexing
            && !self.code_indexing
            && self.health_checked_at.elapsed() >= Duration::from_secs(15)
        {
            self.schedule_health_check(false);
        }
        if immediate_repaint {
            ctx.request_repaint();
        } else if self.generating {
            ctx.request_repaint_after(Duration::from_millis(40));
        } else {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    #[allow(dead_code)] // Retained for internal configuration diagnostics.
    fn test_config(&mut self) {
        let tx = self.events_tx.clone();
        let settings = self.settings.clone();
        let (chat_owned, embedding_owned) = self
            .processes
            .lock()
            .map(|processes| {
                (
                    processes.owns(ServiceKind::Chat),
                    processes.owns(ServiceKind::Embedding),
                )
            })
            .unwrap_or((false, false));
        thread::spawn(move || {
            if let Err(error) = settings.validate_local_only() {
                let _ = tx.send(Event::ConfigResult(format!(
                    "Configuracion rechazada por privacidad: {error}"
                )));
                return;
            }
            let chat_model = Path::new(&settings.chat_model_path).exists();
            let embedding_model = Path::new(&settings.embedding_model_path).exists();
            let chat_reachable = client::health(&settings.chat_host, settings.chat_port);
            let embedding_reachable =
                client::health(&settings.embedding_host, settings.embedding_port);
            let command = resolve_llama_command(&settings.llama_command);
            let service_description =
                |reachable: bool, owned: bool, model_available: bool, host: &str, port: u16| {
                    if reachable && owned {
                        format!("servidor administrado conectado ({host}:{port})")
                    } else if reachable {
                        format!("servidor externo conectado ({host}:{port})")
                    } else if model_available && command.is_some() {
                        "listo para inicio administrado".to_owned()
                    } else {
                        "sin servidor accesible ni modelo local utilizable".to_owned()
                    }
                };
            let _ = tx.send(Event::ConfigResult(format!(
                "IA principal: {}\nBúsqueda semántica: {}\nllama.cpp: {}",
                service_description(
                    chat_reachable,
                    chat_owned,
                    chat_model,
                    &settings.chat_host,
                    settings.chat_port,
                ),
                service_description(
                    embedding_reachable,
                    embedding_owned,
                    embedding_model,
                    &settings.embedding_host,
                    settings.embedding_port,
                ),
                command
                    .as_deref()
                    .unwrap_or("no disponible; configura llama, llama serve o llama-server")
            )));
        });
    }
}

impl eframe::App for KuznorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events(ctx);
        let (chat_owned, embedding_owned) = self
            .processes
            .lock()
            .map(|processes| {
                (
                    processes.owns(ServiceKind::Chat),
                    processes.owns(ServiceKind::Embedding),
                )
            })
            .unwrap_or((false, false));
        egui::TopBottomPanel::top("top")
            .frame(ui::theme::panel_frame().inner_margin(egui::Margin::symmetric(14, 8)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::new(&self.logo)
                            .fit_to_exact_size(egui::vec2(22.0, 22.0))
                            .maintain_aspect_ratio(true),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("KUZNOR")
                            .strong()
                            .size(19.0)
                            .color(ui::theme::TEXT),
                    );
                    ui.add_space(16.0);
                    let current_profile = self.mode;
                    let mut requested_mode = None;
                    for profile in Profile::ALL {
                        let selected = current_profile == profile;
                        let button = egui::Button::new(profile.label())
                            .selected(selected)
                            .fill(if selected {
                                ui::theme::ACCENT_SOFT
                            } else {
                                ui::theme::PANEL
                            })
                            .stroke(egui::Stroke::new(
                                1.0,
                                if selected {
                                    ui::theme::ACCENT
                                } else {
                                    ui::theme::BORDER
                                },
                            ));
                        if ui.add(button).clicked() {
                            requested_mode = Some(profile);
                        }
                    }
                    if let Some(profile) = requested_mode {
                        self.switch_mode(profile);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        compact_status(
                            ui,
                            "Búsqueda semántica",
                            &self.embedding_status,
                            &self.settings.embedding_model_path,
                            self.settings.embedding_port,
                            embedding_owned,
                        );
                        compact_status(
                            ui,
                            "IA principal",
                            &self.chat_status,
                            &self.settings.chat_model_path,
                            self.settings.chat_port,
                            chat_owned,
                        );
                    });
                });
            });
        if self.settings.diagnostic_mode {
            if let Some(metrics) = &self.performance_metrics {
                egui::TopBottomPanel::top("diagnostic_metrics")
                    .frame(ui::theme::panel_frame().inner_margin(egui::Margin::symmetric(14, 3)))
                    .show(ctx, |ui| {
                        ui.weak(metrics);
                    });
            }
        }
        egui::SidePanel::left("sidebar")
            .resizable(false)
            .exact_width(ui::theme::SIDEBAR_WIDTH)
            .frame(ui::theme::panel_frame())
            .show(ctx, |ui| {
                let active_library = active_library_for_sidebar(self.mode, self.active_library_id);
                let show_libraries = self.mode == Profile::Documentation;
                if let Some(action) = ui::sidebar::show(
                    ui,
                    &self.chats,
                    &self.libraries,
                    self.current_chat,
                    active_library,
                    &mut self.chat_search,
                    show_libraries,
                ) {
                    use ui::sidebar::SidebarAction::*;
                    match action {
                        NewChat => {
                            self.input.clear();
                            self.current_chat = None;
                            self.messages.clear();
                            self.prompt_diagnostic = None;
                            self.generation_metrics = None;
                        }
                        SelectChat(id) => {
                            if self.chats.iter().any(|chat| chat.id == id) {
                                if self.current_chat != Some(id) {
                                    self.input.clear();
                                }
                                self.current_chat = Some(id);
                                let _ = self.db.save_setting(mode_key(self.mode), &id.to_string());
                                let _ = self.reload();
                                self.view = chat_view_for_profile(self.mode);
                            }
                        }
                        DeleteChat(id) => self.pending_delete = Some(id),
                        RenameChat(id) => {
                            self.rename_chat = Some(id);
                            self.rename_text = self
                                .chats
                                .iter()
                                .find(|c| c.id == id)
                                .map(|c| c.title.clone())
                                .unwrap_or_default();
                        }
                        SelectLibrary(id) => {
                            if let Err(error) = self.set_active_library(id) {
                                self.error(error);
                            }
                        }
                        OpenLibraries => self.view = View::Libraries,
                        OpenSettings => self.view = View::Settings,
                    }
                }
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui::theme::BACKGROUND)
                    .inner_margin(egui::Margin::same(ui::theme::CONTENT_PADDING)),
            )
            .show(ctx, |ui| {
                match self.view {
                    View::Chat => {
                        let current = self.current().cloned();
                        let active_library_name = (self.mode == Profile::Documentation)
                            .then(|| {
                                self.active_library_id.and_then(|id| {
                                    self.libraries
                                        .iter()
                                        .find(|library| library.id == id)
                                        .map(|library| library.name.clone())
                                })
                            })
                            .flatten();
                        if let Some(chat) = &current {
                            ui.horizontal(|ui| {
                                ui.heading(&chat.title);
                                if let Some(library_name) = &active_library_name {
                                    ui.colored_label(
                                        ui::theme::ACCENT_HOVER,
                                        format!("Biblioteca: {library_name}"),
                                    );
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("Limpiar").clicked() {
                                            let _ = self.db.clear_chat(chat.id);
                                            let _ = self.reload();
                                        }
                                        if self.settings.diagnostic_mode
                                            && self.prompt_diagnostic.is_some()
                                            && ui
                                                .small_button("Ver contexto enviado al modelo")
                                                .clicked()
                                        {
                                            self.prompt_diagnostic_open = true;
                                        }
                                    },
                                );
                            });
                        } else {
                            ui.horizontal_wrapped(|ui| {
                                ui.heading("Nuevo chat");
                                if let Some(library_name) = &active_library_name {
                                    ui.colored_label(
                                        ui::theme::ACCENT_HOVER,
                                        format!("Biblioteca: {library_name}"),
                                    );
                                }
                            });
                        }
                        if self.settings.diagnostic_mode {
                            if let Some(metrics) = &self.generation_metrics {
                                ui.weak(format!("Diagnostico: {metrics}"));
                            }
                        }
                        ui.separator();

                        let composer_layout =
                            ui::chat::composer_layout(&self.input, ui.available_height());
                        let composer_id = egui::Id::new((
                            "chat_composer",
                            mode_key(self.mode),
                            current.as_ref().map(|chat| chat.id).unwrap_or_default(),
                        ));
                        let composer = egui::TopBottomPanel::bottom(composer_id.with("footer"))
                            .exact_height(composer_layout.footer_height)
                            .show_separator_line(true)
                            .frame(egui::Frame::new())
                            .show_inside(ui, |ui| {
                                ui::chat::composer(
                                    ui,
                                    &mut self.input,
                                    composer_id,
                                    composer_layout,
                                    self.generating,
                                    "Escribe un mensaje...",
                                )
                            })
                            .inner;
                        egui::CentralPanel::default()
                            .frame(egui::Frame::new())
                            .show_inside(ui, |ui| {
                                ui::chat::messages(
                                    ui,
                                    &self.messages,
                                    self.generation_visible_for_current_chat(),
                                    self.streaming_for_current_chat(),
                                    self.generation_stage.as_deref(),
                                    self.settings.diagnostic_mode,
                                    (self.mode == Profile::Documentation)
                                        .then_some(self.active_library_id)
                                        .flatten(),
                                );
                            });
                        if composer.send {
                            self.send();
                        }
                        if composer.stop {
                            self.cancel_generation();
                        }
                    }
                    View::Code => {
                        let code_action = egui::SidePanel::left("code_project_panel")
                            .default_width(320.0)
                            .width_range(280.0..=420.0)
                            .resizable(true)
                            .show_separator_line(false)
                            .frame(ui::theme::surface_frame())
                            .show_inside(ui, |ui| {
                                ui::code::show_project_panel(
                                    ui,
                                    &self.code_projects,
                                    self.active_code_project,
                                    &self.code_files,
                                    self.selected_code_file,
                                    &self.code_status,
                                    self.code_indexing,
                                )
                            })
                            .inner;
                        let mut composer_action = ui::chat::ComposerAction::default();
                        egui::CentralPanel::default()
                            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 0)))
                            .show_inside(ui, |ui| {
                                if self.code_indexing {
                                    ui.vertical_centered(|ui| {
                                        ui.add_space(100.0);
                                        ui.spinner();
                                        ui.heading("Indexando proyecto");
                                        ui.weak(&self.code_status);
                                        if ui.button("Cancelar").clicked() {
                                            self.cancel_code_indexing();
                                        }
                                    });
                                } else if self.active_code_project.is_none()
                                    && self.selected_code_file.is_none()
                                {
                                    ui.vertical_centered(|ui| {
                                        ui.add_space(120.0);
                                        ui.weak("Selecciona un archivo o carpeta para comenzar.");
                                    });
                                } else {
                                    ui.horizontal(|ui| {
                                        ui.heading("Analisis de codigo");
                                        if self.settings.diagnostic_mode
                                            && self.prompt_diagnostic.is_some()
                                            && ui
                                                .small_button("Ver contexto enviado al modelo")
                                                .clicked()
                                        {
                                            self.prompt_diagnostic_open = true;
                                        }
                                    });
                                    ui.weak("Kuznor propone; tu decides que copiar y aplicar.");
                                    ui.separator();

                                    let composer_layout = ui::chat::composer_layout(
                                        &self.input,
                                        ui.available_height(),
                                    );
                                    let composer_id = egui::Id::new((
                                        "code_composer",
                                        self.current_chat.unwrap_or_default(),
                                        self.active_code_project.unwrap_or_default(),
                                        self.selected_code_file.unwrap_or_default(),
                                    ));
                                    composer_action =
                                        egui::TopBottomPanel::bottom(composer_id.with("footer"))
                                            .exact_height(composer_layout.footer_height)
                                            .show_separator_line(true)
                                            .frame(egui::Frame::new())
                                            .show_inside(ui, |ui| {
                                                ui::chat::composer(
                                                    ui,
                                                    &mut self.input,
                                                    composer_id,
                                                    composer_layout,
                                                    self.generating,
                                                    "Pregunta sobre el archivo o proyecto...",
                                                )
                                            })
                                            .inner;
                                    egui::CentralPanel::default()
                                        .frame(egui::Frame::new())
                                        .show_inside(ui, |ui| {
                                            ui::chat::messages(
                                                ui,
                                                &self.messages,
                                                self.generation_visible_for_current_chat(),
                                                self.streaming_for_current_chat(),
                                                self.generation_stage.as_deref(),
                                                self.settings.diagnostic_mode,
                                                None,
                                            );
                                        });
                                }
                            });
                        if composer_action.send {
                            self.send();
                        }
                        if composer_action.stop {
                            self.cancel_generation();
                        }
                        if let Some(action) = code_action {
                            use ui::code::CodeAction::*;
                            match action {
                                OpenFile => {
                                    if let Some(path) = rfd::FileDialog::new()
                                        .add_filter(
                                            "Codigo y texto",
                                            &[
                                                "rs", "py", "sql", "js", "ts", "tsx", "jsx",
                                                "html", "css", "json", "toml", "yaml", "yml", "md",
                                                "txt", "c", "h", "cpp", "hpp", "java", "cs", "go",
                                            ],
                                        )
                                        .pick_file()
                                    {
                                        self.index_code_path(path, true);
                                    }
                                }
                                OpenFolder => {
                                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                        self.index_code_path(path, false);
                                    }
                                }
                                CancelIndexing => self.cancel_code_indexing(),
                                SelectProject(project_id) => {
                                    self.active_code_project = Some(project_id);
                                    self.selected_code_file = None;
                                    let _ = self.db.save_setting(
                                        "active_code_project_id",
                                        &project_id.to_string(),
                                    );
                                    let _ = self.reload();
                                }
                                ReindexProject(project_id) => {
                                    if let Some(project) = self
                                        .code_projects
                                        .iter()
                                        .find(|project| project.id == project_id)
                                    {
                                        self.index_code_path(
                                            PathBuf::from(&project.root_path),
                                            false,
                                        );
                                    }
                                }
                                RemoveProject(project_id) => {
                                    self.pending_remove_code_project = Some(project_id);
                                }
                                SelectFile(file_id) => self.selected_code_file = Some(file_id),
                                QuickPrompt(action) => {
                                    self.input = action.prompt().into();
                                    self.send();
                                }
                            }
                        }
                    }
                    View::Libraries => {
                        if let Some(action) = ui::libraries::show(
                            ui,
                            &self.libraries,
                            self.active_library_id,
                            &self.documents,
                            &mut self.new_name,
                        ) {
                            use ui::libraries::LibraryAction::*;
                            match action {
                                BackToChat => {
                                    self.view = chat_view_for_profile(self.mode);
                                }
                                Create => match self.db.create_library(&self.new_name) {
                                    Ok(id) => {
                                        self.new_name.clear();
                                        let _ = self.reload();
                                        if let Err(error) = self.set_active_library(Some(id)) {
                                            self.error(error);
                                        }
                                    }
                                    Err(e) => self.error(e),
                                },
                                Select(id) => {
                                    if let Err(error) = self.set_active_library(Some(id)) {
                                        self.error(error);
                                    }
                                }
                                Rename(id) => {
                                    self.rename_library_text = self
                                        .libraries
                                        .iter()
                                        .find(|library| library.id == id)
                                        .map(|library| library.name.clone())
                                        .unwrap_or_default();
                                    self.rename_library = Some(id);
                                }
                                Delete(id) => {
                                    self.pending_delete_library = Some(id);
                                }
                                AddDocument => {
                                    if let (Some(id), Some(path)) = (
                                        self.active_library_id,
                                        rfd::FileDialog::new()
                                            .add_filter("Documentos", &["txt", "md", "pdf"])
                                            .pick_file(),
                                    ) {
                                        self.index_path(id, path, None);
                                    }
                                }
                                DeleteDocument(id) => {
                                    if let Err(e) = self.db.delete_document(id) {
                                        self.error(e)
                                    } else {
                                        let _ = self.reload();
                                    }
                                }
                                Reindex(id) => {
                                    if let Some(doc) =
                                        self.documents.iter().find(|d| d.id == id).cloned()
                                    {
                                        self.index_path(
                                            doc.library_id,
                                            PathBuf::from(doc.original_path),
                                            Some(id),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    View::Settings => {
                        let mut back_to_chat = false;
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Volver").clicked() {
                                back_to_chat = true;
                            }
                            ui.heading("Configuracion");
                        });
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .id_salt("settings_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                egui::Frame::new()
                                    .inner_margin(egui::Margin::symmetric(3, 2))
                                    .show(ui, |ui| {
                                        if ui::settings::show(
                                            ui,
                                            &mut self.settings,
                                            matches!(
                                                self.chat_status,
                                                ServiceStatus::Ready | ServiceStatus::Busy
                                            ),
                                            matches!(
                                                self.embedding_status,
                                                ServiceStatus::Ready | ServiceStatus::Busy
                                            ),
                                            chat_owned,
                                            embedding_owned,
                                            self.performance_metrics.as_deref(),
                                        ) {
                                            match self.settings.save(&self.db) {
                                                Ok(_) => {
                                                    self.notice =
                                                        Some("Configuracion guardada.".into());
                                                    self.health_checked_at =
                                                        Instant::now() - Duration::from_secs(15);
                                                }
                                                Err(e) => self.error(e),
                                            }
                                        }
                                    });
                            });
                        if back_to_chat {
                            self.view = chat_view_for_profile(self.mode);
                        }
                    }
                }
                if let Some(progress) = &self.progress {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(progress);
                    });
                }
            });
        if let Some(id) = self.rename_chat {
            egui::Window::new("Renombrar chat")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.text_edit_singleline(&mut self.rename_text);
                    ui.horizontal(|ui| {
                        if ui.button("Guardar").clicked() {
                            if let Some(chat) = self.chats.iter().find(|c| c.id == id).cloned() {
                                let _ = self.db.update_chat(
                                    id,
                                    &self.rename_text,
                                    chat.profile,
                                    chat.library_id,
                                );
                                let _ = self.reload();
                            }
                            self.rename_chat = None;
                        }
                        if ui.button("Cancelar").clicked() {
                            self.rename_chat = None;
                        }
                    });
                });
        }
        if let Some(id) = self.rename_library {
            egui::Window::new("Renombrar biblioteca")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Nuevo nombre");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.rename_library_text)
                            .desired_width(320.0),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Cancelar").clicked() {
                            self.rename_library = None;
                        }
                        if ui
                            .add(ui::theme::primary_button("Guardar nombre"))
                            .clicked()
                        {
                            match self.db.rename_library(id, &self.rename_library_text) {
                                Ok(()) => {
                                    self.rename_library_text =
                                        self.rename_library_text.trim().to_owned();
                                    if let Err(error) = self.reload() {
                                        self.error(error);
                                    } else {
                                        self.rename_library = None;
                                    }
                                }
                                Err(error) => self.error(error),
                            }
                        }
                    });
                });
        }
        if let Some(id) = self.pending_delete {
            egui::Window::new("Eliminar chat")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Esta conversacion y sus mensajes se eliminaran permanentemente.");
                    ui.horizontal(|ui| {
                        if ui.button("Cancelar").clicked() {
                            self.pending_delete = None;
                        }
                        if ui
                            .add(egui::Button::new("Eliminar").fill(ui::theme::ERROR))
                            .clicked()
                        {
                            match self.db.delete_chat(id) {
                                Ok(()) => {
                                    self.current_chat = None;
                                    let _ = self.db.save_setting(mode_key(self.mode), "");
                                    let _ = self.reload();
                                }
                                Err(error) => self.error(error),
                            }
                            self.pending_delete = None;
                        }
                    });
                });
        }
        if let Some(id) = self.pending_delete_library {
            egui::Window::new("Eliminar biblioteca")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Se eliminaran el registro y los indices internos. Los archivos originales no se borraran.");
                    ui.horizontal(|ui| {
                        if ui.button("Cancelar").clicked() {
                            self.pending_delete_library = None;
                        }
                        if ui
                            .add(egui::Button::new("Eliminar").fill(ui::theme::ERROR))
                            .clicked()
                        {
                            match self.db.delete_library(id) {
                                Ok(()) => {
                                    if self.active_library_id == Some(id) {
                                        self.active_library_id = None;
                                        self.documents.clear();
                                    }
                                    let _ = self.reload();
                                }
                                Err(error) => self.error(error),
                            }
                            self.pending_delete_library = None;
                        }
                    });
                });
        }
        if let Some(id) = self.pending_remove_code_project {
            let project_name = self
                .code_projects
                .iter()
                .find(|project| project.id == id)
                .map(|project| project.name.clone())
                .unwrap_or_else(|| "Proyecto".into());
            egui::Window::new("Quitar proyecto de Kuznor")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.strong(project_name);
                    ui.label("Se eliminara el indice local de este proyecto de Kuznor.");
                    ui.label("Los archivos originales no seran modificados ni eliminados.");
                    ui.horizontal(|ui| {
                        if ui.button("Cancelar").clicked() {
                            self.pending_remove_code_project = None;
                        }
                        if ui.button("Quitar proyecto").clicked() {
                            self.remove_code_project(id);
                            self.pending_remove_code_project = None;
                        }
                    });
                });
        }
        if let Some(message) = self.notice.clone() {
            egui::Window::new("Aviso")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(message);
                    if let Some(details) = self.notice_details.clone() {
                        ui.collapsing("Ver detalles tecnicos", |ui| {
                            ui.monospace(details);
                        });
                    }
                    if ui.button("Cerrar").clicked() {
                        self.notice = None;
                        self.notice_details = None;
                    }
                });
        }
        if self.settings.diagnostic_mode && self.prompt_diagnostic_open {
            if let Some(diagnostic) = self.prompt_diagnostic.clone() {
                let mut diagnostic_text = diagnostic;
                egui::Window::new("Contexto enviado al modelo")
                    .resizable(true)
                    .default_size(egui::vec2(760.0, 620.0))
                    .show(ctx, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut diagnostic_text)
                                .desired_rows(30)
                                .desired_width(f32::INFINITY),
                        );
                        if ui.button("Cerrar").clicked() {
                            self.prompt_diagnostic_open = false;
                        }
                    });
            }
        }
    }
}

impl Drop for KuznorApp {
    fn drop(&mut self) {
        if let Some(cancelled) = &self.generation_cancel {
            cancelled.store(true, Ordering::Relaxed);
        }
        if let Some(cancelled) = &self.code_index_cancel {
            cancelled.store(true, Ordering::Relaxed);
        }
    }
}

fn generation_belongs_to_chat(current_chat: Option<i64>, generation_chat: Option<i64>) -> bool {
    current_chat.is_some() && current_chat == generation_chat
}

fn automatic_title(question: &str) -> String {
    let compact = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title = compact.chars().take(42).collect::<String>();
    if compact.chars().count() > title.chars().count() {
        title.push_str("...");
    }
    if title.is_empty() {
        "Nuevo chat".into()
    } else {
        title
    }
}

fn compact_status(
    ui: &mut egui::Ui,
    name: &str,
    status: &ServiceStatus,
    model_path: &str,
    port: u16,
    owned: bool,
) {
    let (color, text) = match status {
        ServiceStatus::Disconnected => (ui::theme::TEXT_MUTED, "desconectado"),
        ServiceStatus::Starting => (ui::theme::WARNING, "iniciando"),
        ServiceStatus::Ready => (ui::theme::SUCCESS, "listo"),
        ServiceStatus::Busy => (ui::theme::ACCENT_HOVER, "trabajando"),
        ServiceStatus::Error(_) => (ui::theme::ERROR, "error"),
    };
    let response = ui::status_dot(ui, color, name);
    let process = if owned {
        "iniciado por Kuznor"
    } else if matches!(status, ServiceStatus::Ready | ServiceStatus::Busy) {
        "servidor externo"
    } else {
        "detenido"
    };
    let model = std::path::Path::new(model_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sin configurar");
    response.on_hover_text(format!(
        "{name}: {text}\nModelo: {model}\nPuerto: {port}\nProceso: {process}"
    ));
    if let ServiceStatus::Error(err) = status {
        ui.label("!").on_hover_text(err);
    }
}

fn service_status_label(status: &ServiceStatus) -> &'static str {
    match status {
        ServiceStatus::Disconnected => "detenido",
        ServiceStatus::Starting => "iniciando",
        ServiceStatus::Ready => "listo",
        ServiceStatus::Busy => "trabajando",
        ServiceStatus::Error(_) => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        View, active_library_for_sidebar, automatic_title, captured_code_file,
        chat_status_after_health, chat_view_for_profile, database_path_in,
        generation_belongs_to_chat, sources_for_code_scope, sources_for_library, startup_view,
        status_after_health, status_after_request, valid_selected_code_file,
    };
    use crate::{
        ai::process::ServiceStatus,
        code::types::CodeFile,
        db::Database,
        models::{Profile, Source},
    };

    #[test]
    fn title_uses_first_message_without_becoming_unwieldy() {
        assert_eq!(
            automatic_title("Consulta SQL basica"),
            "Consulta SQL basica"
        );
        assert!(automatic_title(&"x ".repeat(60)).ends_with("..."));
    }

    #[test]
    fn external_chat_server_skips_initial_settings() {
        assert_eq!(startup_view(true, false, false), View::Chat);
    }

    #[test]
    fn managed_chat_configuration_skips_initial_settings() {
        assert_eq!(startup_view(false, true, true), View::Chat);
    }

    #[test]
    fn unusable_chat_configuration_opens_settings() {
        assert_eq!(startup_view(false, false, false), View::Settings);
        assert_eq!(startup_view(false, true, false), View::Settings);
    }

    #[test]
    fn empty_document_chat_uses_the_active_library_id_immediately() {
        assert_eq!(
            active_library_for_sidebar(Profile::Documentation, Some(42)),
            Some(42)
        );
        assert_eq!(active_library_for_sidebar(Profile::General, Some(42)), None);
    }

    #[test]
    fn selecting_a_chat_from_a_subview_opens_its_conversation_view() {
        assert_eq!(chat_view_for_profile(Profile::Documentation), View::Chat);
        assert_eq!(chat_view_for_profile(Profile::General), View::Chat);
        assert_eq!(chat_view_for_profile(Profile::Code), View::Code);
    }

    #[test]
    fn active_library_is_context_not_navigation() {
        let current_chat = Some(17);
        let selected_library = Some(42);

        assert_eq!(current_chat, Some(17));
        assert_eq!(
            active_library_for_sidebar(Profile::Documentation, selected_library),
            Some(42)
        );
    }

    #[test]
    fn service_state_moves_from_checking_to_healthy_and_tracks_requests() {
        assert_eq!(
            status_after_health(&ServiceStatus::Starting, true),
            ServiceStatus::Ready
        );
        assert_eq!(status_after_request(None), ServiceStatus::Ready);
        assert!(matches!(
            status_after_request(Some("fallo local")),
            ServiceStatus::Error(_)
        ));
        assert_eq!(
            status_after_health(&ServiceStatus::Busy, true),
            ServiceStatus::Busy
        );
        let check_started = std::time::Instant::now();
        assert_eq!(
            chat_status_after_health(
                &ServiceStatus::Ready,
                false,
                check_started,
                Some(check_started + std::time::Duration::from_millis(1)),
            ),
            ServiceStatus::Ready
        );
    }

    #[test]
    fn changing_project_drops_a_file_selected_in_the_previous_project() {
        let files = vec![CodeFile {
            id: 20,
            project_id: 2,
            relative_path: "src/b.rs".into(),
            hash: String::new(),
            extension: "rs".into(),
            language: "rust".into(),
            size_bytes: 1,
            error: None,
        }];
        assert_eq!(valid_selected_code_file(Some(10), &files), None);
        assert_eq!(valid_selected_code_file(Some(20), &files), Some(20));
        assert!(files.iter().all(|file| file.project_id == 2));
        assert!(captured_code_file(2, Some(10), &files).is_none());
        assert_eq!(captured_code_file(2, Some(20), &files).unwrap().id, 20);
    }

    #[test]
    fn current_code_sources_reject_other_projects_and_files() {
        let source = |project_id, file_id, chunk_id| Source {
            library_id: 0,
            project_id,
            file_id,
            document_id: file_id,
            chunk_id,
            score: 1.0,
            document_name: format!("file-{file_id}.py"),
            chunk_index: 0,
            page_number: None,
            preview: String::new(),
            relative_path: format!("src/file-{file_id}.py"),
            line_start: Some(1),
            line_end: Some(2),
        };
        let current = sources_for_code_scope(
            vec![source(1, 10, 1), source(2, 10, 2), source(2, 20, 3)],
            2,
            Some(&[20]),
        );
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].project_id, 2);
        assert_eq!(current[0].file_id, 20);
    }

    #[test]
    fn visible_sources_are_rebuilt_for_the_active_library_only() {
        let source = |library_id, document_id, name: &str| Source {
            library_id,
            project_id: 0,
            file_id: 0,
            document_id,
            chunk_id: document_id * 10,
            score: 0.9,
            document_name: name.into(),
            chunk_index: 0,
            page_number: Some(1),
            preview: String::new(),
            relative_path: String::new(),
            line_start: None,
            line_end: None,
        };
        let allowed = [20_i64].into_iter().collect();
        let current = sources_for_library(
            vec![
                source(1, 10, "sql.pdf"),
                source(2, 20, "viajes.pdf"),
                source(2, 20, "viajes.pdf"),
            ],
            2,
            &allowed,
        );
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].library_id, 2);
        assert_eq!(current[0].document_id, 20);
        assert_eq!(current[0].document_name, "viajes.pdf");
    }

    #[test]
    fn legacy_database_is_copied_to_kuznor_name() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let legacy = directory.path().join("inference_local.db");
        let database = Database::open(&legacy).unwrap();
        database
            .create_chat("Conversacion anterior", Profile::General)
            .unwrap();
        drop(database);

        let migrated = database_path_in(directory.path()).unwrap();
        assert_eq!(migrated.file_name().unwrap(), "kuznor.db");
        assert_eq!(
            Database::open(migrated)
                .unwrap()
                .list_chats()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn generation_survives_navigation_and_returns_only_to_its_chat() {
        let generation_chat = Some(41);
        assert!(generation_belongs_to_chat(Some(41), generation_chat));
        assert!(!generation_belongs_to_chat(Some(99), generation_chat));
        assert!(!generation_belongs_to_chat(None, generation_chat));
        assert!(generation_belongs_to_chat(Some(41), generation_chat));
    }

    #[test]
    fn no_chat_or_mode_change_can_reassign_generation_buffer() {
        let generation_chat = Some(7);
        let other_chat = Some(8);
        assert_ne!(generation_chat, other_chat);
        assert!(!generation_belongs_to_chat(other_chat, generation_chat));
        assert!(generation_belongs_to_chat(generation_chat, generation_chat));
    }
}
