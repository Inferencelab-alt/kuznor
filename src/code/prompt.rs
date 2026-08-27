use crate::{
    ai::client::ApiMessage,
    models::{Message, Source},
};

use super::{search::ScoredCodeChunk, types::CodeFile};

pub const CODE_SYSTEM_PROMPT: &str = "Eres un asistente de analisis de codigo en modo solo lectura. Analiza el codigo y contexto proporcionados cuando la pregunta dependa del proyecto. Puedes explicar, detectar posibles errores, sugerir mejoras, refactorizar y generar codigo. Distingue lo observado de tus recomendaciones. No afirmes que ejecutaste, compilaste o probaste codigo. No modifiques archivos. Si propones codigo, entregalo para que el usuario decida si lo copia y aplica manualmente. Los prefijos numericos con | pertenecen a referencias de lineas: no los incluyas dentro de bloques de codigo propuesto para copiar.";

pub fn code_history_without_evidence(history: &[Message]) -> Vec<Message> {
    let mut sanitized = Vec::new();
    let mut pending_user: Option<Message> = None;
    for message in history {
        match message.role.as_str() {
            "user" => pending_user = Some(message.clone()),
            "assistant" => {
                if let Some(mut user) = pending_user.take() {
                    user.content =
                        "[Consulta de codigo anterior omitida del contexto tecnico actual.]".into();
                    user.sources.clear();
                    let mut assistant = message.clone();
                    assistant.content = "[Respuesta anterior omitida: el historial conversacional no constituye evidencia de codigo para este turno.]".into();
                    assistant.sources.clear();
                    sanitized.push(user);
                    sanitized.push(assistant);
                }
            }
            _ => {}
        }
    }
    sanitized
}

pub fn code_prompt(
    chunks: &[ScoredCodeChunk],
    project_name: &str,
    selected_file: Option<&CodeFile>,
) -> (String, Vec<Source>) {
    let mut prompt = CODE_SYSTEM_PROMPT.to_owned();
    prompt.push_str(&format!(
        "\n\nSCOPE TECNICO ACTUAL\nProyecto activo: {project_name}\nArchivo seleccionado: {}\nEste scope fue capturado al iniciar el turno. Solo CONTEXTO DE CODIGO es evidencia tecnica actual; el historial conversacional no lo es.",
        selected_file
            .map(|file| format!("{} [file_id={}]", file.relative_path, file.id))
            .unwrap_or_else(|| "Ninguno".into())
    ));
    if chunks.is_empty() {
        prompt.push_str("\n\nNo se recupero contexto del proyecto. Indicalo si la pregunta depende de archivos no proporcionados.");
        return (prompt, Vec::new());
    }
    prompt.push_str("\n\nCONTEXTO DE CODIGO:\n");
    let mut sources = Vec::new();
    for item in chunks {
        let chunk = &item.chunk;
        prompt.push_str(&format!(
            "\n[{} - lineas {}-{} | score {:.3} | lenguaje {}]\n{}\n",
            chunk.relative_path,
            chunk.line_start,
            chunk.line_end,
            item.score,
            chunk.language,
            chunk.content
        ));
        sources.push(Source {
            library_id: 0,
            project_id: chunk.project_id,
            file_id: chunk.file_id,
            document_id: chunk.file_id,
            chunk_id: chunk.id,
            score: item.score,
            document_name: chunk.relative_path.clone(),
            chunk_index: chunk.chunk_index,
            page_number: None,
            preview: chunk.content.chars().take(360).collect(),
            relative_path: chunk.relative_path.clone(),
            line_start: Some(chunk.line_start),
            line_end: Some(chunk.line_end),
        });
    }
    (prompt, sources)
}

pub fn code_prompt_diagnostic(
    system: &str,
    question: &str,
    project_name: &str,
    selected_file: Option<&CodeFile>,
    chunks: &[ScoredCodeChunk],
    messages: &[ApiMessage],
) -> String {
    let mut output = format!(
        "SYSTEM PROMPT\n{system}\n\nPREGUNTA DEL USUARIO\n{question}\n\nPROYECTO ACTIVO\n{project_name}\n\nARCHIVO SELECCIONADO\n{}\n\nCHUNKS DE CODIGO RECUPERADOS\n",
        selected_file
            .map(|file| format!("file_id={} | {}", file.id, file.relative_path))
            .unwrap_or_else(|| "Ninguno".into())
    );
    for item in chunks {
        output.push_str(&format!(
            "\n{} | lineas {}-{} | score {:.4}\n{}\n",
            item.chunk.relative_path,
            item.chunk.line_start,
            item.chunk.line_end,
            item.score,
            item.chunk.content
        ));
    }
    output.push_str("\nMENSAJES FINALES ENVIADOS AL ENDPOINT DE CHAT\n");
    for message in messages {
        output.push_str(&format!("\n[{}]\n{}\n", message.role, message.content));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::{search::ScoredCodeChunk, types::CodeChunk};

    #[test]
    fn prompt_is_read_only_and_has_line_sources() {
        let selected = CodeFile {
            id: 2,
            project_id: 1,
            relative_path: "src/main.rs".into(),
            hash: String::new(),
            extension: "rs".into(),
            language: "rust".into(),
            size_bytes: 10,
            error: None,
        };
        let (prompt, sources) = code_prompt(
            &[ScoredCodeChunk {
                chunk: CodeChunk {
                    id: 1,
                    project_id: 1,
                    file_id: 2,
                    relative_path: "src/main.rs".into(),
                    extension: "rs".into(),
                    language: "rust".into(),
                    chunk_index: 0,
                    line_start: 4,
                    line_end: 9,
                    content: "fn main() {}".into(),
                    embedding: vec![],
                },
                score: 0.9,
            }],
            "kuznor",
            Some(&selected),
        );
        assert!(prompt.contains("modo solo lectura"));
        assert!(prompt.contains("No modifiques archivos"));
        assert!(prompt.contains("Proyecto activo: kuznor"));
        assert!(prompt.contains("src/main.rs [file_id=2]"));
        assert!(prompt.contains("historial conversacional no lo es"));
        assert_eq!(sources[0].relative_path, "src/main.rs");
        assert_eq!(sources[0].line_start, Some(4));
        assert_eq!(sources[0].project_id, 1);
        assert_eq!(sources[0].file_id, 2);
    }

    #[test]
    fn prior_code_answers_never_become_current_evidence() {
        let history = vec![
            Message {
                id: 1,
                chat_id: 1,
                role: "user".into(),
                content: "Explica file_a.py".into(),
                sources: vec![],
                created_at: String::new(),
            },
            Message {
                id: 2,
                chat_id: 1,
                role: "assistant".into(),
                content: "calculate_invoice procesa facturas".into(),
                sources: vec![],
                created_at: String::new(),
            },
        ];
        let sanitized = code_history_without_evidence(&history);
        assert_eq!(sanitized.len(), 2);
        assert!(
            sanitized
                .iter()
                .all(|message| !message.content.contains("file_a.py"))
        );
        assert!(
            sanitized
                .iter()
                .all(|message| !message.content.contains("calculate_invoice"))
        );
    }
}
