use crate::{
    ai::client::ApiMessage,
    models::{Chunk, Message, Profile, Source},
    rag::search::{DocumentOverviewInput, ScoredChunk},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentIntent {
    Meta,
    LibraryOverview,
    DocumentQuery,
}

pub const OVERVIEW_ANALYSIS_MAX_TOKENS: usize = 256;
pub const OVERVIEW_SYNTHESIS_MAX_TOKENS: usize = 512;

pub fn general_system_prompt(model_path: &str) -> String {
    let mut prompt = Profile::General.system_prompt().to_owned();
    let configured_model = std::path::Path::new(model_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty());
    if let Some(model) = configured_model {
        prompt.push_str(&format!(
            " Metadata real disponible: el archivo de modelo local configurado es {model}. Presentalo como motor subyacente, nunca como la identidad principal."
        ));
    } else {
        prompt.push_str(" La aplicacion no conoce con certeza el nombre del modelo subyacente conectado; no lo inventes.");
    }
    prompt
}

pub fn document_history_without_evidence(history: &[Message]) -> Vec<Message> {
    let mut sanitized = Vec::new();
    let mut pending_user: Option<Message> = None;
    for message in history {
        match message.role.as_str() {
            "user" => pending_user = Some(message.clone()),
            "assistant" => {
                if let Some(mut user) = pending_user.take() {
                    if document_intent(&user.content) == DocumentIntent::Meta
                        && message.sources.is_empty()
                    {
                        user.content = format!(
                            "[HISTORIAL CONVERSACIONAL; NO ES EVIDENCIA DOCUMENTAL]\n{}",
                            user.content
                        );
                        let mut assistant = message.clone();
                        assistant.content = format!(
                            "[HISTORIAL CONVERSACIONAL; NO ES EVIDENCIA DOCUMENTAL]\n{}",
                            assistant.content
                        );
                        sanitized.push(user);
                        sanitized.push(assistant);
                    } else {
                        user.content =
                            "[Consulta documental anterior omitida del contexto actual.]".into();
                        let mut assistant = message.clone();
                        assistant.content = "[Respuesta documental anterior omitida: no constituye evidencia para este turno.]".into();
                        assistant.sources.clear();
                        sanitized.push(user);
                        sanitized.push(assistant);
                    }
                }
            }
            _ => {}
        }
    }
    sanitized
}

pub fn document_intent(question: &str) -> DocumentIntent {
    let normalized = question.trim().to_lowercase();
    let searchable = normalized
        .replace('\u{00e1}', "a")
        .replace('\u{00e9}', "e")
        .replace('\u{00ed}', "i")
        .replace('\u{00f3}', "o")
        .replace('\u{00fa}', "u");
    let compact = normalized
        .chars()
        .filter(|character| character.is_alphanumeric() || character.is_whitespace())
        .collect::<String>();
    let words = compact.split_whitespace().collect::<Vec<_>>();
    let meta = normalized == "hola"
        || normalized == "hola!"
        || normalized == "gracias"
        || normalized == "gracias!"
        || searchable.contains("que sabes hacer")
        || normalized.contains("qué sabes hacer")
        || searchable.contains("que puedes hacer")
        || normalized.contains("qué puedes hacer")
        || searchable.contains("como funcionas")
        || normalized.contains("cómo funcionas")
        || searchable.contains("como funciona kuznor")
        || searchable.contains("como funciona este modo")
        || searchable.contains("que puedo preguntarte")
        || searchable.contains("que puedo hacer aqui")
        || searchable.contains("como uso documentos")
        || searchable.contains("como funciona documentos")
        || searchable.contains("como uso este modo")
        || normalized.contains("cómo funciona kuznor")
        || normalized == "ayuda"
        || (words.len() <= 3 && words.first().is_some_and(|word| *word == "hola"));
    let compares_library = searchable.contains("compara")
        && (searchable.contains("todos los archivos")
            || searchable.contains("todos los documentos")
            || searchable.contains("los 3 archivos")
            || searchable.contains("los tres archivos")
            || searchable.contains("los archivos de esta biblioteca")
            || searchable.contains("los documentos de esta biblioteca"));
    let mentions_library = searchable.contains("esta biblioteca")
        || searchable.contains("la biblioteca")
        || searchable.contains("documentos de aqui")
        || searchable.contains("archivos de aqui");
    let asks_global_summary = mentions_library
        && (searchable.contains("resume")
            || searchable.contains("resumen")
            || searchable.contains("temas principales")
            || searchable.contains("analiza los documentos")
            || searchable.contains("analiza los archivos"));
    let library_overview = compares_library
        || asks_global_summary
        || [
            "analiza los archivos que tienes",
            "analiza los documentos que tienes",
            "archivos disponibles",
            "documentos disponibles",
            "todos los archivos",
            "todos los documentos",
            "resume la biblioteca",
            "resumen de la biblioteca",
            "que archivos tienes",
            "qué archivos tienes",
        ]
        .iter()
        .any(|phrase| searchable.contains(phrase));
    if meta {
        DocumentIntent::Meta
    } else if library_overview {
        DocumentIntent::LibraryOverview
    } else {
        DocumentIntent::DocumentQuery
    }
}

pub fn document_meta_prompt() -> String {
    "Estas usando el modo Documentos de Kuznor, un asistente local y privado. Documentos significa este modo de Kuznor, no otro producto. Explica exclusivamente sus capacidades reales: consultar, resumir, comparar y analizar documentos locales compatibles, y mostrar las fuentes utilizadas. No menciones ni describas servicios cloud, suites ofimaticas o productos externos. No afirmes que puedes editar archivos.".into()
}

pub fn rag_library_overview_prompt(
    chunks: &[ScoredChunk],
    documents: &[(i64, String)],
) -> (String, Vec<Source>) {
    let instruction = "Analiza la biblioteca usando exclusivamente la lista y evidencia proporcionadas. \
        La lista de archivos proviene de SQLite y es la única lista válida: no inventes, renombres ni omitas archivos. \
        Mantén el idioma de la pregunta del usuario. Distingue hechos explícitos, inferencias y encabezados o metadatos. \
        No establezcas relaciones entre documentos independientes sin evidencia. Si un archivo no tiene contexto suficiente, indícalo.";
    let instruction = format!(
        "{instruction} Una pagina o fragmento nunca es un documento separado. Produce una sola entidad o fila por encabezado DOCUMENTO."
    );
    grouped_document_prompt(&instruction, chunks, documents, false)
}

pub fn document_overview_analysis_prompt(
    document: &DocumentOverviewInput,
    question: &str,
) -> String {
    let mut prompt = format!(
        "Crea una ficha interna compacta de un solo documento local. DOCUMENTO: {}. Pregunta: {question}. Usa solo la evidencia incluida. No escribas introduccion, tabla, conclusion, URLs ni secciones por fragmento. OBJETIVO, TIPO, PUBLICO e INCERTIDUMBRE pueden sintetizar o parafrasear el conjunto de evidencia sin copiarlo literalmente ni agregar una referencia a cada campo. DATO 1, DATO 2 y DATO 3 deben ser afirmaciones breves, concretas y representativas, cada una terminada con una o varias referencias validas como [C1] o [C1][C2]. No uses encabezados, preguntas ni fragmentos truncados como datos. Si no hay respaldo, escribe SIN EVIDENCIA SUFICIENTE. No conviertas inferencias en hechos. Devuelve exactamente estas siete lineas: OBJETIVO, TIPO, PUBLICO, DATO 1, DATO 2, DATO 3, INCERTIDUMBRE.\n\nEVIDENCIA DEL DOCUMENTO:\n",
        document.filename
    );
    for (index, item) in document.representative_chunks.iter().enumerate() {
        prompt.push_str(&format!(
            "[C{}] pagina {}:\n{}\n",
            index + 1,
            item.chunk
                .page_number
                .map(|page| page.to_string())
                .unwrap_or_else(|| "-".into()),
            item.chunk.content
        ));
    }
    if document.representative_chunks.is_empty() {
        prompt.push_str("[Sin evidencia recuperada para este documento.]\n");
    }
    prompt
}

pub fn validate_overview_analysis(document: &DocumentOverviewInput, analysis: &str) -> String {
    const FIELDS: [&str; 7] = [
        "OBJETIVO",
        "TIPO",
        "PUBLICO",
        "DATO 1",
        "DATO 2",
        "DATO 3",
        "INCERTIDUMBRE",
    ];
    let mut validated = FIELDS
        .iter()
        .map(|field| {
            let value = analysis.lines().find_map(|line| {
                let clean = line
                    .trim()
                    .trim_start_matches(['-', '*', ' '])
                    .trim_matches('*');
                let (label, value) = clean.split_once(':')?;
                (normalize_field_label(label) == normalize_field_label(field))
                    .then_some(value.trim())
            });
            let Some(value) = value else {
                return format!("{field}: SIN EVIDENCIA SUFICIENTE");
            };
            if value.eq_ignore_ascii_case("SIN EVIDENCIA SUFICIENTE") {
                return format!("{field}: SIN EVIDENCIA SUFICIENTE");
            }
            let evidence_kind = overview_evidence_kind(field);
            let references = short_evidence_references(value);
            let items = if references.is_empty() {
                if evidence_kind == EvidenceKind::Factual {
                    return format!("{field}: SIN EVIDENCIA SUFICIENTE");
                }
                document.representative_chunks.iter().collect::<Vec<_>>()
            } else {
                let Some(items) = references
                    .iter()
                    .map(|reference| {
                        reference
                            .checked_sub(1)
                            .and_then(|index| document.representative_chunks.get(index))
                    })
                    .collect::<Option<Vec<_>>>()
                else {
                    return format!("{field}: SIN EVIDENCIA SUFICIENTE");
                };
                items
            };
            if items.is_empty() {
                return format!("{field}: SIN EVIDENCIA SUFICIENTE");
            }
            if items
                .iter()
                .any(|item| item.chunk.document_id != document.document_id)
            {
                return format!("{field}: SIN EVIDENCIA SUFICIENTE");
            }
            let claim = strip_short_reference(value);
            let claim = claim.trim();
            let evidence = items
                .iter()
                .map(|item| item.chunk.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let supported = match evidence_kind {
                EvidenceKind::Factual => factual_claim_supported(claim, &evidence),
                EvidenceKind::Synthesis => synthesis_claim_supported(claim, &evidence),
            };
            if claim.is_empty() || contains_url(claim) || !supported {
                return format!("{field}: SIN EVIDENCIA SUFICIENTE");
            }
            let chunk_ids = items
                .iter()
                .map(|item| item.chunk.id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("{field}: {claim} [chunk_id={chunk_ids}]")
        })
        .collect::<Vec<_>>();
    if validated
        .iter()
        .all(|line| line.ends_with("SIN EVIDENCIA SUFICIENTE"))
    {
        for (offset, (fact, chunk_id)) in safe_factual_fallback(document).into_iter().enumerate() {
            validated[3 + offset] = format!("DATO {}: {fact} [chunk_id={chunk_id}]", offset + 1);
        }
    }
    validated.join("\n")
}

pub fn library_overview_synthesis_prompt(
    analyses: &[(i64, String, String)],
    question: &str,
) -> String {
    let output_rule = if overview_requires_document_rows(question) {
        "Si pidio una tabla, devuelve una sola tabla y, como maximo, una nota breve. Incluye exactamente una fila o entidad por documento con su nombre real."
    } else {
        "La consulta pide una sintesis global: usa las fichas de todos los documentos, responde en el numero de puntos solicitado y no la conviertas obligatoriamente en una tabla o seccion por archivo."
    };
    let mut prompt = format!(
        "El usuario pregunto: {question}\n\nProduce unicamente la respuesta final solicitada usando las fichas internas. {output_rule} No muestres las fichas internas, chunk_id ni secciones por fragmento. No generes URLs o enlaces de fuentes; Kuznor muestra las fuentes reales por separado. No inventes relaciones ni datos sin evidencia. No digas que la biblioteca carece de informacion si alguna ficha contiene datos validados. SIN EVIDENCIA SUFICIENTE debe conservarse solo para criterios realmente no respaldados.\n\nFICHAS INTERNAS:\n"
    );
    for (document_id, filename, analysis) in analyses {
        prompt.push_str(&format!(
            "\n[document_id={document_id}; filename={filename}]\n{analysis}\n"
        ));
    }
    prompt
}

pub fn render_structured_library_overview(
    analyses: &[(i64, String, String)],
    synthesis: &str,
    question: &str,
) -> String {
    let cleaned = sanitize_document_output(synthesis);
    if overview_requires_document_rows(question) {
        if analyses
            .iter()
            .all(|(_, filename, _)| cleaned.matches(filename).count() == 1)
            && !overview_false_negative(&cleaned, analyses)
        {
            cleaned
        } else {
            render_compact_overview_fallback(analyses)
        }
    } else if cleaned.trim().is_empty() || overview_false_negative(&cleaned, analyses) {
        render_global_summary_fallback(analyses)
    } else {
        cleaned
    }
}

fn overview_requires_document_rows(question: &str) -> bool {
    let normalized = normalize_for_validation(question);
    normalized.contains("compara")
        || normalized.contains("comparacion")
        || normalized.contains("tabla")
}

fn overview_false_negative(answer: &str, analyses: &[(i64, String, String)]) -> bool {
    let has_valid_evidence = analyses
        .iter()
        .any(|(_, _, analysis)| analysis.contains("[chunk_id="));
    let normalized = normalize_for_validation(answer);
    has_valid_evidence
        && (normalized.contains("no hay informacion")
            || normalized.contains("no contienen informacion")
            || normalized.contains("sin evidencia suficiente")
            || normalized.contains("no encontre informacion")
            || (normalized.contains("informacion") && normalized.contains("limitada")))
}

fn render_global_summary_fallback(analyses: &[(i64, String, String)]) -> String {
    let mut points = Vec::new();
    for (_, filename, analysis) in analyses {
        for field in ["OBJETIVO", "TIPO", "DATO 1", "DATO 2", "DATO 3"] {
            let Some(value) = analysis.lines().find_map(|line| {
                let (label, value) = line.split_once(':')?;
                label
                    .trim()
                    .eq_ignore_ascii_case(field)
                    .then(|| strip_supporting_marker(value.trim()).trim().to_owned())
            }) else {
                continue;
            };
            if value.eq_ignore_ascii_case("SIN EVIDENCIA SUFICIENTE") {
                continue;
            }
            points.push(format!("- **{filename}:** {value}"));
            break;
        }
        if points.len() >= 4 {
            break;
        }
    }
    if points.is_empty() {
        "No se obtuvo evidencia documental validada para elaborar el resumen.".into()
    } else {
        points.join("\n")
    }
}

fn render_compact_overview_fallback(analyses: &[(i64, String, String)]) -> String {
    let mut output = String::from(
        "| Archivo | Objetivo principal | Tipo de informacion | Publico | Datos verificables | Que no se puede concluir |\n|---|---|---|---|---|---|\n",
    );
    for (_, filename, analysis) in analyses {
        let field = |name: &str| {
            analysis
                .lines()
                .find_map(|line| {
                    let (label, value) = line.split_once(':')?;
                    label.trim().eq_ignore_ascii_case(name).then(|| {
                        strip_supporting_marker(value.trim())
                            .replace('|', "/")
                            .trim()
                            .to_owned()
                    })
                })
                .unwrap_or_else(|| "SIN EVIDENCIA SUFICIENTE".into())
        };
        let facts = [field("DATO 1"), field("DATO 2"), field("DATO 3")]
            .into_iter()
            .filter(|fact| !fact.eq_ignore_ascii_case("SIN EVIDENCIA SUFICIENTE"))
            .collect::<Vec<_>>();
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            filename.replace('|', "/"),
            field("OBJETIVO"),
            field("TIPO"),
            field("PUBLICO"),
            if facts.is_empty() {
                "SIN EVIDENCIA SUFICIENTE".into()
            } else {
                facts.join("; ")
            },
            field("INCERTIDUMBRE")
        ));
    }
    output.trim().to_owned()
}

fn strip_supporting_marker(value: &str) -> &str {
    value
        .find("[chunk_id=")
        .map(|position| value[..position].trim_end())
        .unwrap_or(value)
}

pub fn rag_system_prompt(profile: Profile, chunks: &[ScoredChunk]) -> (String, Vec<Source>) {
    rag_system_prompt_with_documents(profile, chunks, &[])
}

pub fn rag_system_prompt_with_documents(
    profile: Profile,
    chunks: &[ScoredChunk],
    documents: &[(i64, String)],
) -> (String, Vec<Source>) {
    let instruction = if profile == Profile::Documentation {
        "Responde usando unicamente la evidencia documental proporcionada. Puedes resumir, combinar y sacar conclusiones evidentes de varios fragmentos. No es necesario que la respuesta aparezca literalmente en una sola oracion. Si los fragmentos permiten responder razonablemente, responde. Solo di que no hay informacion cuando realmente no haya evidencia suficiente. Distingue hechos explicitos del documento, inferencias y encabezados, metadatos o notas. No conviertas plazos, periodos, condiciones o atributos en otros hechos que el documento no respalde. Presenta toda inferencia como inferencia. No inventes nombres, cifras, fechas ni hechos. Mantén el idioma de la pregunta del usuario."
    } else {
        profile.system_prompt()
    };
    let instruction = format!(
        "{instruction} Solo uses comillas para texto presente literalmente en los fragmentos; nunca fabriques citas. No generes URLs ni enlaces de fuentes; Kuznor mostrara las fuentes reales por separado. Cuando el documento contenga una respuesta u opcion explicita, conserva su significado y priorizala sobre conocimiento previo o inferencias. En preguntas de si o no, la explicacion debe ser consistente con la respuesta inicial. No transformes una condicion documentada en otra distinta ni presentes una inferencia como hecho. Cuando se pregunte por un concepto concreto, prioriza evidencia explicita y semanticamente directa; no fuerces a participar documentos relacionados solo de manera abstracta. Si la pregunta pide afirmar ausencia en toda la biblioteca pero la evidencia no cubre todos sus documentos, limita la conclusion a los fragmentos recuperados. Cuando se pidan temas, conceptos, ideas, areas o categorias, sintetiza nombres conceptuales en vez de copiar preguntas o fragmentos crudos. Corrige separaciones defectuosas al redactar sin inventar contenido."
    );
    let comparison = documents.len() > 1;
    if comparison {
        return grouped_document_prompt(&instruction, chunks, documents, true);
    }
    let mut prompt = if comparison {
        format!(
            "{instruction}\n\nCompara únicamente los documentos identificados. \
             No trates cada fragmento como una fuente o documento independiente. \
             Agrupa los fragmentos según el encabezado DOCUMENTO al que pertenecen. \
             Las páginas y fragmentos pertenecen al documento cuyo encabezado los contiene. \
             Si un documento no aporta evidencia para un criterio, indícalo. \
             No infieras propósito, público objetivo u otros datos sin respaldo.\n\n\
             EVIDENCIA DOCUMENTAL AGRUPADA:\n"
        )
    } else {
        format!("{instruction}\n\nEVIDENCIA DOCUMENTAL:\n")
    };
    if chunks.is_empty() {
        prompt.push_str("[No se recuperaron fragmentos documentales.]\n");
    }
    let mut sources = Vec::new();
    if comparison {
        for (position, (document_id, document_name)) in documents.iter().enumerate() {
            prompt.push_str(&format!(
                "\nDOCUMENTO {}: {document_name}\n",
                document_label(position)
            ));
            let document_chunks = chunks
                .iter()
                .filter(|item| item.chunk.document_id == *document_id);
            let mut found = false;
            for (fragment_index, item) in document_chunks.enumerate() {
                found = true;
                append_chunk(&mut prompt, fragment_index, item, &mut sources);
            }
            if !found {
                prompt.push_str("Sin evidencia suficiente recuperada para este criterio.\n");
            }
        }
    } else {
        for (position, item) in chunks.iter().enumerate() {
            append_chunk_with_position(&mut prompt, position, item, &mut sources);
        }
    }
    (prompt, sources)
}

fn grouped_document_prompt(
    instruction: &str,
    chunks: &[ScoredChunk],
    documents: &[(i64, String)],
    comparison: bool,
) -> (String, Vec<Source>) {
    let purpose = if comparison {
        "Compara únicamente los documentos identificados."
    } else {
        "Analiza los documentos reales de la biblioteca."
    };
    let mut prompt = format!(
        "{instruction}\n\n{purpose} \
         No trates cada fragmento como una fuente o documento independiente. \
         Agrupa los fragmentos según el encabezado DOCUMENTO al que pertenecen. \
         Usa únicamente estos nombres reales de archivo:\n"
    );
    prompt.push_str(&format!(
        "Hay exactamente {} documentos reales. Conserva exactamente una entidad por cada encabezado DOCUMENTO. Nunca presentes una pagina o fragmento como documento separado.\n",
        documents.len()
    ));
    for (_, name) in documents {
        prompt.push_str(&format!("- {name}\n"));
    }
    prompt.push_str("\nEVIDENCIA DOCUMENTAL AGRUPADA:\n");
    let mut sources = Vec::new();
    for (position, (document_id, document_name)) in documents.iter().enumerate() {
        prompt.push_str(&format!(
            "\nDOCUMENTO {}: {document_name}\n",
            document_label(position)
        ));
        let document_chunks = chunks
            .iter()
            .filter(|item| item.chunk.document_id == *document_id);
        let mut found = false;
        for (fragment_index, item) in document_chunks.enumerate() {
            found = true;
            append_chunk(&mut prompt, fragment_index, item, &mut sources);
        }
        if !found {
            prompt.push_str("Sin evidencia suficiente recuperada para este criterio.\n");
        }
    }
    (prompt, sources)
}

fn document_label(position: usize) -> &'static str {
    match position {
        0 => "A",
        1 => "B",
        2 => "C",
        3 => "D",
        _ => "ADICIONAL",
    }
}

fn append_chunk(
    prompt: &mut String,
    fragment_index: usize,
    item: &ScoredChunk,
    sources: &mut Vec<Source>,
) {
    let location = item
        .chunk
        .page_number
        .map(|page| format!("pagina {page}"))
        .unwrap_or_else(|| format!("fragmento {}", item.chunk.chunk_index + 1));
    prompt.push_str(&format!(
        "Fragmento {} - {location} | relevancia {:.3}:\n{}\n",
        fragment_index + 1,
        item.score,
        item.chunk.content
    ));
    sources.push(source_from_chunk(item));
}

fn append_chunk_with_position(
    prompt: &mut String,
    position: usize,
    item: &ScoredChunk,
    sources: &mut Vec<Source>,
) {
    let chunk: &Chunk = &item.chunk;
    let location = chunk
        .page_number
        .map(|page| format!("pagina {page}"))
        .unwrap_or_else(|| format!("fragmento {}", chunk.chunk_index + 1));
    prompt.push_str(&format!(
        "\n[Fuente {}: {} - {} | relevancia {:.3}]\n{}\n",
        position + 1,
        chunk.document_name,
        location,
        item.score,
        chunk.content
    ));
    sources.push(source_from_chunk(item));
}

fn source_from_chunk(item: &ScoredChunk) -> Source {
    Source {
        library_id: 0,
        project_id: 0,
        file_id: 0,
        document_id: item.chunk.document_id,
        chunk_id: item.chunk.id,
        score: item.score,
        document_name: item.chunk.document_name.clone(),
        chunk_index: item.chunk.chunk_index,
        page_number: item.chunk.page_number,
        preview: item.chunk.content.chars().take(280).collect(),
        relative_path: String::new(),
        line_start: None,
        line_end: None,
    }
}

pub fn sanitize_document_output(answer: &str) -> String {
    let mut clean = String::with_capacity(answer.len());
    let mut index = 0;
    while index < answer.len() {
        let rest = &answer[index..];
        if let Some(prefix_len) = ["https://", "http://"]
            .iter()
            .find_map(|prefix| rest.starts_with(prefix).then_some(prefix.len()))
        {
            index += prefix_len;
            while index < answer.len() {
                let character = answer[index..].chars().next().unwrap();
                if character.is_whitespace() || matches!(character, ')' | ']' | '>' | ',') {
                    break;
                }
                index += character.len_utf8();
            }
            continue;
        }
        let character = rest.chars().next().unwrap();
        clean.push(character);
        index += character.len_utf8();
    }
    clean
        .replace("]()", "]")
        .replace("[]", "")
        .trim()
        .to_owned()
}

fn contains_url(value: &str) -> bool {
    value.contains("http://") || value.contains("https://")
}

fn normalize_for_validation(value: &str) -> String {
    value
        .to_lowercase()
        .replace('á', "a")
        .replace('é', "e")
        .replace('í', "i")
        .replace('ó', "o")
        .replace('ú', "u")
        .replace('ü', "u")
}

fn normalize_field_label(value: &str) -> String {
    normalize_for_validation(value)
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceKind {
    Factual,
    Synthesis,
}

fn overview_evidence_kind(field: &str) -> EvidenceKind {
    if matches!(
        normalize_field_label(field).as_str(),
        "dato1" | "dato2" | "dato3"
    ) {
        EvidenceKind::Factual
    } else {
        EvidenceKind::Synthesis
    }
}

fn short_evidence_references(value: &str) -> Vec<usize> {
    let mut references = value
        .split(|character: char| !character.is_alphanumeric())
        .filter_map(|token| {
            let digits = token
                .strip_prefix('C')
                .or_else(|| token.strip_prefix('c'))?;
            (!digits.is_empty() && digits.chars().all(|character| character.is_ascii_digit()))
                .then(|| digits.parse().ok())
                .flatten()
        })
        .collect::<Vec<_>>();
    references.sort_unstable();
    references.dedup();
    references
}

fn strip_short_reference(value: &str) -> String {
    let mut cleaned = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(open) = rest.find('[') {
        cleaned.push_str(&rest[..open]);
        let Some(close) = rest[open + 1..].find(']') else {
            cleaned.push_str(&rest[open..]);
            return cleaned;
        };
        let close = open + 1 + close;
        let marker = &rest[open + 1..close];
        let marker_tokens = marker
            .split(|character: char| !character.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        let is_reference_marker = !marker_tokens.is_empty()
            && marker_tokens.iter().all(|token| {
                token
                    .strip_prefix('C')
                    .or_else(|| token.strip_prefix('c'))
                    .is_some_and(|digits| {
                        !digits.is_empty()
                            && digits.chars().all(|character| character.is_ascii_digit())
                    })
            });
        if !is_reference_marker {
            cleaned.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    cleaned.push_str(rest);
    cleaned.trim().to_owned()
}

fn synthesis_claim_supported(claim: &str, evidence: &str) -> bool {
    !claim.trim().is_empty() && concrete_claim_supported(claim, evidence)
}

fn factual_claim_supported(claim: &str, evidence: &str) -> bool {
    useful_factual_claim(claim)
        && concrete_claim_supported(claim, evidence)
        && claim_supported_by_chunk(claim, evidence)
}

fn useful_factual_claim(claim: &str) -> bool {
    if claim.contains('\n') || claim.chars().count() > 180 {
        return false;
    }
    if !concrete_terms(claim).is_empty() {
        return true;
    }
    const FACTUAL_VERBS: [&str; 17] = [
        "es",
        "son",
        "incluye",
        "incluyen",
        "contiene",
        "contienen",
        "requiere",
        "requieren",
        "evalua",
        "evaluan",
        "presenta",
        "presentan",
        "explica",
        "explican",
        "indica",
        "indican",
        "establece",
    ];
    normalize_for_validation(claim)
        .split(|character: char| !character.is_alphanumeric())
        .any(|term| FACTUAL_VERBS.contains(&term))
}

fn concrete_claim_supported(claim: &str, evidence: &str) -> bool {
    let evidence_terms = concrete_terms(evidence)
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    concrete_terms(claim)
        .iter()
        .all(|term| evidence_terms.contains(term))
}

fn concrete_terms(value: &str) -> Vec<String> {
    normalize_for_validation(value)
        .split(|character: char| !character.is_alphanumeric())
        .filter_map(|term| {
            if term.chars().any(|character| character.is_ascii_digit()) {
                return Some(term.to_owned());
            }
            let canonical = match term {
                "mxn" | "usd" | "eur" => term,
                "semana" | "semanas" | "semanal" | "semanales" => "periodo:semana",
                "quincena" | "quincenas" | "quincenal" | "quincenales" => "periodo:quincena",
                "dia" | "dias" | "diario" | "diaria" | "diarios" | "diarias" => "periodo:dia",
                "mes" | "meses" | "mensual" | "mensuales" => "periodo:mes",
                "ano" | "anos" | "anual" | "anuales" => "periodo:ano",
                _ => return None,
            };
            Some(canonical.to_owned())
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

fn claim_supported_by_chunk(claim: &str, evidence: &str) -> bool {
    let claim_terms = evidence_terms(claim);
    if claim_terms.is_empty() {
        return false;
    }
    let evidence_terms = evidence_terms(evidence)
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let matched = claim_terms
        .iter()
        .filter(|term| evidence_terms.contains(*term))
        .count();
    if claim_terms.len() <= 2 {
        matched == claim_terms.len()
    } else {
        matched >= 2 && matched * 2 >= claim_terms.len()
    }
}

fn evidence_terms(value: &str) -> Vec<String> {
    const STOPWORDS: [&str; 24] = [
        "para",
        "como",
        "este",
        "esta",
        "estos",
        "estas",
        "documento",
        "archivo",
        "sobre",
        "entre",
        "desde",
        "hasta",
        "tiene",
        "indica",
        "incluye",
        "principal",
        "tipo",
        "dato",
        "publico",
        "objetivo",
        "informacion",
        "evidencia",
        "suficiente",
        "chunk",
    ];
    normalize_for_validation(value)
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.len() >= 3 || term.chars().all(|character| character.is_ascii_digit()))
        .filter(|term| !STOPWORDS.contains(term))
        .map(str::to_owned)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

fn safe_factual_fallback(document: &DocumentOverviewInput) -> Vec<(String, i64)> {
    let mut facts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in &document.representative_chunks {
        for sentence in item
            .chunk
            .content
            .split(['\n', '.', ';'])
            .map(str::trim)
            .filter(|sentence| sentence.chars().count() >= 8)
        {
            if concrete_terms(sentence).is_empty() || sentence.chars().count() > 140 {
                continue;
            }
            let normalized = normalize_for_validation(sentence);
            if seen.insert(normalized) {
                facts.push((sentence.to_owned(), item.chunk.id));
            }
            if facts.len() == 3 {
                return facts;
            }
        }
    }
    facts
}

pub fn prompt_diagnostic(
    system: &str,
    question: &str,
    library_name: &str,
    chunks: &[ScoredChunk],
    messages: &[ApiMessage],
) -> String {
    let mut output = format!(
        "SYSTEM PROMPT\n{system}\n\nPREGUNTA DEL USUARIO\n{question}\n\nBIBLIOTECA\n{library_name}\n\nDOCUMENTOS Y CHUNKS RECUPERADOS\n"
    );
    for (index, item) in chunks.iter().enumerate() {
        output.push_str(&format!(
            "\n[{index}] documento={} | document_id={} | chunk_id={} | pagina={} | chunk_index={} | score={:.4}\n{}\n",
            item.chunk.document_name,
            item.chunk.document_id,
            item.chunk.id,
            item.chunk
                .page_number
                .map(|page| page.to_string())
                .unwrap_or_else(|| "-".into()),
            item.chunk.chunk_index,
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

    fn message(id: i64, role: &str, content: &str, sources: Vec<Source>) -> Message {
        Message {
            id,
            chat_id: 1,
            role: role.into(),
            content: content.into(),
            sources,
            created_at: String::new(),
        }
    }

    #[test]
    fn general_identity_is_kuznor_and_model_metadata_never_replaces_it() {
        let known = general_system_prompt("C:/models/gemma-3-4b.gguf");
        assert!(known.contains("Eres Kuznor"));
        assert!(known.contains("Inference Lab desarrollo Kuznor"));
        assert!(known.contains("no afirmes que entreno el modelo subyacente"));
        assert!(known.contains("gemma-3-4b.gguf"));
        assert!(known.contains("motor subyacente"));
        assert!(!known.contains("marca de ropa"));

        let unknown = general_system_prompt("");
        assert!(unknown.contains("no lo inventes"));
        assert!(unknown.contains("Kuznor"));
    }

    #[test]
    fn documentary_history_removes_prior_rag_evidence_but_keeps_meta_as_history() {
        let prior_source = Source {
            library_id: 1,
            project_id: 0,
            file_id: 0,
            document_id: 10,
            chunk_id: 100,
            score: 0.9,
            document_name: "sql.pdf".into(),
            chunk_index: 0,
            page_number: Some(1),
            preview: "SELECT WHERE primary keys".into(),
            relative_path: String::new(),
            line_start: None,
            line_end: None,
        };
        let history = vec![
            message(1, "user", "Explica primary keys y SQL", vec![]),
            message(
                2,
                "assistant",
                "SQL usa SELECT y WHERE para consultar primary keys",
                vec![prior_source],
            ),
            message(3, "user", "Hola", vec![]),
            message(4, "assistant", "Soy Kuznor en Documentos", vec![]),
        ];

        let sanitized = document_history_without_evidence(&history);
        let text = sanitized
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("primary keys"));
        assert!(!text.contains("SELECT"));
        assert!(text.contains("Respuesta documental anterior omitida"));
        assert!(text.contains("HISTORIAL CONVERSACIONAL"));
        assert!(text.contains("Soy Kuznor en Documentos"));
        assert!(sanitized.iter().all(|message| message.sources.is_empty()));
    }

    #[test]
    fn document_meta_messages_skip_rag() {
        assert_eq!(document_intent("Hola"), DocumentIntent::Meta);
        assert_eq!(document_intent("¿Qué sabes hacer?"), DocumentIntent::Meta);
        assert_eq!(document_intent("¿Cómo funcionas?"), DocumentIntent::Meta);
        assert_eq!(document_intent("Gracias"), DocumentIntent::Meta);
        assert_eq!(
            document_intent("Hola, \u{00bf}qu\u{00e9} sabes hacer?"),
            DocumentIntent::Meta
        );
        assert_eq!(
            document_intent("\u{00bf}C\u{00f3}mo funciona este modo?"),
            DocumentIntent::Meta
        );
        assert_eq!(
            document_intent("\u{00bf}Qu\u{00e9} puedo preguntarte?"),
            DocumentIntent::Meta
        );
        assert_eq!(
            document_intent("\u{00bf}C\u{00f3}mo uso Documentos?"),
            DocumentIntent::Meta
        );
        assert_eq!(
            document_intent("Que puedo hacer aqui"),
            DocumentIntent::Meta
        );
        assert_eq!(
            document_intent("Como funciona Documentos"),
            DocumentIntent::Meta
        );
        assert_eq!(document_intent("Como uso este modo"), DocumentIntent::Meta);
        let prompt = document_meta_prompt();
        assert!(prompt.contains("modo Documentos de Kuznor"));
        assert!(prompt.contains("documentos locales"));
        assert!(!prompt.contains("Google"));
        assert!(prompt.contains("no otro producto"));
    }

    #[test]
    fn document_questions_keep_document_rag() {
        assert_eq!(
            document_intent("¿Qué contiene el PDF de INEGI?"),
            DocumentIntent::DocumentQuery
        );
        assert_eq!(
            document_intent("Compara los dos documentos"),
            DocumentIntent::DocumentQuery
        );
        assert_eq!(
            document_intent("Analiza los archivos que tienes"),
            DocumentIntent::LibraryOverview
        );
        assert_eq!(
            document_intent("Compara los 3 archivos"),
            DocumentIntent::LibraryOverview
        );
        assert_eq!(
            document_intent("Resume en 4 puntos los temas principales de esta biblioteca"),
            DocumentIntent::LibraryOverview
        );
        assert_eq!(
            document_intent("Analiza los documentos de aqui"),
            DocumentIntent::LibraryOverview
        );
        assert_eq!(
            document_intent("¿Que documentos de esta biblioteca hablan de SQL?"),
            DocumentIntent::DocumentQuery
        );
    }

    #[test]
    fn library_overview_contains_exactly_the_real_sqlite_filenames() {
        let documents: Vec<(i64, String)> = vec![
            (1, "ventas.pdf".into()),
            (2, "examen.pdf".into()),
            (3, "cotizacion.pdf".into()),
        ];
        let chunks = documents
            .iter()
            .enumerate()
            .map(|(index, (document_id, name))| ScoredChunk {
                chunk: Chunk {
                    id: index as i64 + 1,
                    document_id: *document_id,
                    document_name: name.clone(),
                    chunk_index: 0,
                    content: format!("contenido de {name}"),
                    page_number: Some(1),
                    section: None,
                    embedding: vec![],
                },
                score: 1.0,
            })
            .collect::<Vec<_>>();
        let (prompt, _) = rag_library_overview_prompt(&chunks, &documents);
        for (_, name) in &documents {
            assert!(prompt.contains(name));
        }
        assert!(!prompt.contains("Notas_proyecto.pdf"));
        assert_eq!(prompt.matches("DOCUMENTO A:").count(), 1);
        assert_eq!(prompt.matches("DOCUMENTO B:").count(), 1);
        assert_eq!(prompt.matches("DOCUMENTO C:").count(), 1);
        assert!(!prompt.contains("DOCUMENTO D:"));
        assert!(prompt.contains("Hay exactamente 3 documentos reales"));
    }

    #[test]
    fn overview_keeps_many_pages_under_exactly_three_document_groups() {
        let documents: Vec<(i64, String)> = vec![
            (1, "A.pdf".into()),
            (2, "B.pdf".into()),
            (3, "C.pdf".into()),
        ];
        let chunks = [(1_i64, 5_i64), (2, 2), (3, 10)]
            .into_iter()
            .flat_map(|(document_id, pages)| {
                (1..=pages).map(move |page| ScoredChunk {
                    chunk: Chunk {
                        id: document_id * 100 + page,
                        document_id,
                        document_name: format!("{}.pdf", (b'A' + document_id as u8 - 1) as char),
                        chunk_index: page as usize - 1,
                        content: format!("contenido pagina {page}"),
                        page_number: Some(page as u32),
                        section: None,
                        embedding: vec![],
                    },
                    score: 1.0,
                })
            })
            .collect::<Vec<_>>();

        let (prompt, sources) = rag_library_overview_prompt(&chunks, &documents);

        assert_eq!(prompt.matches("DOCUMENTO A:").count(), 1);
        assert_eq!(prompt.matches("DOCUMENTO B:").count(), 1);
        assert_eq!(prompt.matches("DOCUMENTO C:").count(), 1);
        assert!(!prompt.contains("DOCUMENTO D:"));
        assert_eq!(
            sources
                .iter()
                .map(|source| source.document_id)
                .collect::<std::collections::HashSet<_>>(),
            [1, 2, 3].into_iter().collect()
        );
    }

    #[test]
    fn overview_output_contains_only_final_synthesis() {
        let analyses = vec![
            (1, "A.pdf".into(), "Analisis A".into()),
            (2, "B.pdf".into(), "Analisis B".into()),
            (3, "C.pdf".into(), "Analisis C".into()),
        ];
        let output = render_structured_library_overview(
            &analyses,
            "| Archivo | Objetivo |\n|---|---|\n| A.pdf | A |\n| B.pdf | B |\n| C.pdf | C |",
            "Compara los documentos en una tabla",
        );
        assert_eq!(output.matches("A.pdf").count(), 1);
        assert_eq!(output.matches("B.pdf").count(), 1);
        assert_eq!(output.matches("C.pdf").count(), 1);
        assert!(!output.contains("Analisis A"));
        assert!(!output.contains("Sintesis global"));
    }

    #[test]
    fn malformed_overview_synthesis_falls_back_to_exactly_one_row_per_document() {
        let analyses = vec![
            (
                1,
                "A.pdf".into(),
                "OBJETIVO: Objetivo A [chunk_id=1]\nTIPO: Informe [chunk_id=1]\nPUBLICO: SIN EVIDENCIA SUFICIENTE\nDATO 1: Dato A [chunk_id=1]\nDATO 2: SIN EVIDENCIA SUFICIENTE\nDATO 3: SIN EVIDENCIA SUFICIENTE\nINCERTIDUMBRE: SIN EVIDENCIA SUFICIENTE".into(),
            ),
            (
                2,
                "B.pdf".into(),
                "OBJETIVO: Objetivo B [chunk_id=2]\nTIPO: Manual [chunk_id=2]\nPUBLICO: SIN EVIDENCIA SUFICIENTE\nDATO 1: Dato B [chunk_id=2]\nDATO 2: SIN EVIDENCIA SUFICIENTE\nDATO 3: SIN EVIDENCIA SUFICIENTE\nINCERTIDUMBRE: SIN EVIDENCIA SUFICIENTE".into(),
            ),
        ];
        let output = render_structured_library_overview(
            &analyses,
            "A.pdf aparece dos veces. A.pdf vuelve a aparecer y B fue omitido.",
            "Compara los documentos",
        );
        assert_eq!(output.matches("A.pdf").count(), 1);
        assert_eq!(output.matches("B.pdf").count(), 1);
        assert_eq!(
            output.lines().filter(|line| line.starts_with('|')).count(),
            4
        );
        assert!(!output.contains("chunk_id"));
    }

    #[test]
    fn global_library_summary_uses_all_validated_documents_without_forcing_a_table() {
        let analyses = vec![
            (1, "A.pdf".into(), "OBJETIVO: Ventas [chunk_id=1]".into()),
            (
                2,
                "B.pdf".into(),
                "OBJETIVO: Estadistica [chunk_id=2]".into(),
            ),
            (
                3,
                "C.pdf".into(),
                "OBJETIVO: Redes sociales [chunk_id=3]".into(),
            ),
        ];
        let output = render_structured_library_overview(
            &analyses,
            "No hay informacion sobre los temas principales.",
            "Resume en 4 puntos los temas principales de esta biblioteca",
        );
        assert!(!output.contains("| Archivo |"));
        assert!(output.contains("A.pdf"));
        assert!(output.contains("B.pdf"));
        assert!(output.contains("C.pdf"));
        assert!(!output.contains("No hay informacion"));
    }

    #[test]
    fn compact_overview_facts_require_real_supporting_chunks() {
        let document = DocumentOverviewInput {
            document_id: 1,
            filename: "cotizacion.pdf".into(),
            representative_chunks: vec![ScoredChunk {
                chunk: Chunk {
                    id: 44,
                    document_id: 1,
                    document_name: "cotizacion.pdf".into(),
                    chunk_index: 0,
                    content: "Cotizacion de servicio de redes. Precio mensual: $1,600 MXN. Vigencia: 7 dias naturales.".into(),
                    page_number: Some(1),
                    section: None,
                    embedding: vec![],
                },
                score: 1.0,
            }],
        };
        let raw = "OBJETIVO: Servicio de redes [C1]\nTIPO: Cotizacion [C1]\nPUBLICO: SIN EVIDENCIA SUFICIENTE\nDATO 1: $1,600 MXN al mes [C1]\nDATO 2: Vigencia de 7 dias [C1]\nDATO 3: El pago es semanal [C1]\nINCERTIDUMBRE: Pago semanal [C999]";
        let validated = validate_overview_analysis(&document, raw);
        assert!(validated.contains("OBJETIVO: Servicio de redes [chunk_id=44]"));
        assert!(validated.contains("DATO 1: $1,600 MXN"));
        assert!(validated.contains("DATO 2: Vigencia de 7 dias"));
        assert!(validated.contains("DATO 3: SIN EVIDENCIA SUFICIENTE"));
        assert!(validated.contains("INCERTIDUMBRE: SIN EVIDENCIA SUFICIENTE"));
    }

    #[test]
    fn verifiable_data_rejects_headings_but_keeps_complete_supported_facts() {
        let document = DocumentOverviewInput {
            document_id: 31,
            filename: "evaluacion.pdf".into(),
            representative_chunks: vec![ScoredChunk {
                chunk: Chunk {
                    id: 310,
                    document_id: 31,
                    document_name: "evaluacion.pdf".into(),
                    chunk_index: 0,
                    content: "Discusion en grupo: Caracteristicas y utilidad. El examen evalua interpretacion de datos y conceptos de estadistica.".into(),
                    page_number: Some(2),
                    section: None,
                    embedding: vec![],
                },
                score: 1.0,
            }],
        };
        let raw = "OBJETIVO: Evaluar conocimientos\nTIPO: Examen\nPUBLICO: Participantes\nDATO 1: Discusion en grupo: Caracteristicas y utilidad [C1]\nDATO 2: El examen evalua interpretacion de datos y estadistica [C1]\nDATO 3: SIN EVIDENCIA SUFICIENTE\nINCERTIDUMBRE: No se conoce el resultado de los participantes";
        let validated = validate_overview_analysis(&document, raw);

        assert!(validated.contains("DATO 1: SIN EVIDENCIA SUFICIENTE"));
        assert!(validated.contains(
            "DATO 2: El examen evalua interpretacion de datos y estadistica [chunk_id=310]"
        ));
        assert!(!validated.contains("DATO 1: Discusion en grupo"));
    }

    #[test]
    fn one_invalid_overview_field_does_not_destroy_six_valid_fields() {
        let document = DocumentOverviewInput {
            document_id: 8,
            filename: "manual.pdf".into(),
            representative_chunks: vec![ScoredChunk {
                chunk: Chunk {
                    id: 987_654_321,
                    document_id: 8,
                    document_name: "manual.pdf".into(),
                    chunk_index: 0,
                    content: "Manual tecnico para analistas. Explica SQL, tablas, consultas y validacion de datos. No contiene fechas.".into(),
                    page_number: Some(3),
                    section: None,
                    embedding: vec![],
                },
                score: 1.0,
            }],
        };
        let raw = "OBJETIVO: Explicar SQL y validacion de datos\nTIPO: Manual tecnico\nPUBLICO: Analistas\nDATO 1: Incluye tablas y consultas [C1]\nDATO 2: Publicado en 2024 [C999]\nDATO 3: No contiene fechas [C1]\nINCERTIDUMBRE: No se puede concluir una fecha de publicacion";
        let validated = validate_overview_analysis(&document, raw);
        assert_eq!(validated.matches("[chunk_id=987654321]").count(), 6);
        assert_eq!(validated.matches("SIN EVIDENCIA SUFICIENTE").count(), 1);
    }

    #[test]
    fn completely_malformed_overview_does_not_turn_raw_chunk_into_a_fact() {
        let document = DocumentOverviewInput {
            document_id: 1,
            filename: "A.pdf".into(),
            representative_chunks: vec![ScoredChunk {
                chunk: Chunk {
                    id: 77,
                    document_id: 1,
                    document_name: "A.pdf".into(),
                    chunk_index: 0,
                    content: "Encabezado real y tema principal del documento.".into(),
                    page_number: Some(1),
                    section: None,
                    embedding: vec![],
                },
                score: 1.0,
            }],
        };
        let validated = validate_overview_analysis(&document, "Respuesta truncada sin formato");
        assert_eq!(validated.matches("SIN EVIDENCIA SUFICIENTE").count(), 7);
        assert!(!validated.contains("Encabezado real y tema principal"));
        assert!(!validated.contains("[chunk_id=77]"));
    }

    #[test]
    fn synthesis_fields_accept_supported_paraphrases_without_per_field_references() {
        let document = DocumentOverviewInput {
            document_id: 12,
            filename: "cotizacion.pdf".into(),
            representative_chunks: vec![
                ScoredChunk {
                    chunk: Chunk {
                        id: 501,
                        document_id: 12,
                        document_name: "cotizacion.pdf".into(),
                        chunk_index: 0,
                        content: "INFERENCE LAB. Presencia digital para negocios locales. COTIZACION DE SERVICIO. Cliente: Barberia.".into(),
                        page_number: Some(1),
                        section: None,
                        embedding: vec![],
                    },
                    score: 1.0,
                },
                ScoredChunk {
                    chunk: Chunk {
                        id: 502,
                        document_id: 12,
                        document_name: "cotizacion.pdf".into(),
                        chunk_index: 1,
                        content: "Servicio: Gestion de redes sociales y edicion basica de reels.".into(),
                        page_number: Some(1),
                        section: None,
                        embedding: vec![],
                    },
                    score: 0.9,
                },
            ],
        };
        let raw = "OBJETIVO: Ofrecer gestion de redes sociales y reels para mantener la presencia digital del negocio\nTIPO: Propuesta profesional\nPUBLICO: Barberias y negocios locales\nDATO 1: SIN EVIDENCIA SUFICIENTE\nDATO 2: SIN EVIDENCIA SUFICIENTE\nDATO 3: SIN EVIDENCIA SUFICIENTE\nINCERTIDUMBRE: No se puede concluir la duracion del servicio";
        let validated = validate_overview_analysis(&document, raw);

        assert!(validated.contains("OBJETIVO: Ofrecer gestion de redes sociales"));
        assert!(validated.contains("[chunk_id=501,502]"));
        assert!(validated.contains("TIPO: Propuesta profesional [chunk_id=501,502]"));
        assert!(validated.contains("PUBLICO: Barberias y negocios locales [chunk_id=501,502]"));
        assert!(validated.contains("INCERTIDUMBRE: No se puede concluir la duracion"));
    }

    #[test]
    fn concrete_contradiction_invalidates_only_its_synthesis_field() {
        let document = DocumentOverviewInput {
            document_id: 4,
            filename: "servicio.pdf".into(),
            representative_chunks: vec![ScoredChunk {
                chunk: Chunk {
                    id: 88,
                    document_id: 4,
                    document_name: "servicio.pdf".into(),
                    chunk_index: 0,
                    content: "Cotizacion mensual de gestion de redes por $1,600 MXN.".into(),
                    page_number: Some(1),
                    section: None,
                    embedding: vec![],
                },
                score: 1.0,
            }],
        };
        let raw = "OBJETIVO: Gestionar redes sociales [C1]\nTIPO: Cotizacion mensual [C1]\nPUBLICO: SIN EVIDENCIA SUFICIENTE\nDATO 1: $1,600 MXN [C1]\nDATO 2: SIN EVIDENCIA SUFICIENTE\nDATO 3: SIN EVIDENCIA SUFICIENTE\nINCERTIDUMBRE: El pago semanal no esta confirmado [C1]";
        let validated = validate_overview_analysis(&document, raw);

        assert!(validated.contains("OBJETIVO: Gestionar redes sociales [chunk_id=88]"));
        assert!(validated.contains("DATO 1: $1,600 MXN [chunk_id=88]"));
        assert!(validated.contains("INCERTIDUMBRE: SIN EVIDENCIA SUFICIENTE"));
    }

    #[test]
    fn malformed_overview_fallback_extracts_only_short_concrete_sentences() {
        let document = DocumentOverviewInput {
            document_id: 9,
            filename: "oferta.pdf".into(),
            representative_chunks: vec![ScoredChunk {
                chunk: Chunk {
                    id: 90,
                    document_id: 9,
                    document_name: "oferta.pdf".into(),
                    chunk_index: 0,
                    content: "ENCABEZADO COMERCIAL MUY GENERAL\nPrecio mensual: $1,600 MXN. Vigencia: 7 dias naturales. Texto descriptivo sin cifras.".into(),
                    page_number: Some(1),
                    section: None,
                    embedding: vec![],
                },
                score: 1.0,
            }],
        };
        let validated = validate_overview_analysis(&document, "salida truncada");

        assert!(validated.contains("DATO 1: Precio mensual: $1,600 MXN"));
        assert!(validated.contains("DATO 2: Vigencia: 7 dias naturales"));
        assert!(!validated.contains("ENCABEZADO COMERCIAL"));
        assert!(validated.contains("DATO 3: SIN EVIDENCIA SUFICIENTE"));
    }

    #[test]
    fn overview_prompt_exposes_only_short_evidence_references() {
        let document = DocumentOverviewInput {
            document_id: 1,
            filename: "A.pdf".into(),
            representative_chunks: vec![ScoredChunk {
                chunk: Chunk {
                    id: 9_876_543_210,
                    document_id: 1,
                    document_name: "A.pdf".into(),
                    chunk_index: 0,
                    content: "Contenido real".into(),
                    page_number: Some(1),
                    section: None,
                    embedding: vec![],
                },
                score: 1.0,
            }],
        };
        let prompt = document_overview_analysis_prompt(&document, "Resume");
        assert!(prompt.contains("[C1] pagina 1"));
        assert!(!prompt.contains("9876543210"));
    }

    #[test]
    fn documentary_output_never_keeps_generated_urls() {
        let output = sanitize_document_output(
            "Fuente falsa: [examen](https://example.com/examen.pdf) y http://fake.local/doc",
        );
        assert!(!output.contains("example.com"));
        assert!(!output.contains("http://"));
        assert!(!output.contains("https://"));
    }

    #[test]
    fn overview_intermediate_work_is_compact_and_not_user_facing() {
        assert!(OVERVIEW_ANALYSIS_MAX_TOKENS <= 256);
        assert!(OVERVIEW_SYNTHESIS_MAX_TOKENS <= 512);
        let synthesis = library_overview_synthesis_prompt(
            &[
                (1, "A.pdf".into(), "OBJETIVO: A [chunk_id=1]".into()),
                (2, "B.pdf".into(), "OBJETIVO: B [chunk_id=2]".into()),
                (3, "C.pdf".into(), "OBJETIVO: C [chunk_id=3]".into()),
            ],
            "Compara los 3 archivos en una tabla",
        );
        assert_eq!(synthesis.matches("filename=").count(), 3);
        assert!(synthesis.contains("una sola tabla"));
        assert!(synthesis.contains("No muestres las fichas internas"));
    }

    #[test]
    fn documentary_prompt_forbids_fake_quotes_and_metadata_conflation() {
        let chunks = [ScoredChunk {
            chunk: Chunk {
                id: 1,
                document_id: 1,
                document_name: "ventas.pdf".into(),
                chunk_index: 0,
                content: "El vendedor orientado al clienteAnalicen y comparen".into(),
                page_number: Some(1),
                section: None,
                embedding: vec![],
            },
            score: 0.8,
        }];
        let (prompt, sources) = rag_system_prompt(Profile::Documentation, &chunks);
        assert_eq!(sources.len(), 1);
        assert!(prompt.contains("nunca fabriques citas"));
        assert!(prompt.contains("No generes URLs"));
        assert!(prompt.contains("No transformes una condicion documentada"));
        assert!(prompt.contains("ni presentes una inferencia como hecho"));
        assert!(prompt.contains("no haya evidencia suficiente"));
        assert!(prompt.contains("sintetiza nombres conceptuales"));
        assert!(prompt.contains("Corrige separaciones defectuosas"));
        assert!(prompt.contains("clienteAnalicen"));
    }
    #[test]
    fn prompt_contains_real_source() {
        let chunk = Chunk {
            id: 1,
            document_id: 1,
            document_name: "manual.md".into(),
            chunk_index: 2,
            content: "dato exacto".into(),
            page_number: None,
            section: None,
            embedding: vec![],
        };
        let (p, s) =
            rag_system_prompt(Profile::Documentation, &[ScoredChunk { chunk, score: 1.0 }]);
        assert!(p.contains("dato exacto"));
        assert_eq!(s[0].document_name, "manual.md");
        assert_eq!(s[0].document_id, 1);
        assert_eq!(s[0].chunk_id, 1);
    }

    #[test]
    fn prompt_allows_synthesis_across_retrieved_fragments() {
        let chunks = [
            ScoredChunk {
                chunk: Chunk {
                    id: 1,
                    document_id: 1,
                    document_name: "examen.pdf".into(),
                    chunk_index: 0,
                    content: "El examen aborda calidad de datos.".into(),
                    page_number: Some(1),
                    section: None,
                    embedding: vec![],
                },
                score: 0.8,
            },
            ScoredChunk {
                chunk: Chunk {
                    id: 2,
                    document_id: 1,
                    document_name: "examen.pdf".into(),
                    chunk_index: 1,
                    content: "Tambien incluye codificacion y bases de datos.".into(),
                    page_number: Some(2),
                    section: None,
                    embedding: vec![],
                },
                score: 0.7,
            },
        ];
        let (prompt, _) = rag_system_prompt(Profile::Documentation, &chunks);
        assert!(prompt.contains("calidad de datos"));
        assert!(prompt.contains("codificacion y bases de datos"));
        assert!(prompt.contains("resumir, combinar y sacar conclusiones"));
        assert!(prompt.contains("Solo di que no hay informacion"));
    }

    #[test]
    fn empty_context_keeps_not_found_as_the_fallback() {
        let (prompt, sources) = rag_system_prompt(Profile::Documentation, &[]);
        assert!(sources.is_empty());
        assert!(prompt.contains("Solo di que no hay informacion"));
    }

    #[test]
    fn comparison_groups_multiple_pages_under_one_document_header() {
        let chunks = [
            ScoredChunk {
                chunk: Chunk {
                    id: 1,
                    document_id: 7,
                    document_name: "uno.pdf".into(),
                    chunk_index: 0,
                    content: "Criterio en pagina uno.".into(),
                    page_number: Some(1),
                    section: None,
                    embedding: vec![],
                },
                score: 0.9,
            },
            ScoredChunk {
                chunk: Chunk {
                    id: 2,
                    document_id: 7,
                    document_name: "uno.pdf".into(),
                    chunk_index: 1,
                    content: "Criterio en pagina cinco.".into(),
                    page_number: Some(5),
                    section: None,
                    embedding: vec![],
                },
                score: 0.8,
            },
            ScoredChunk {
                chunk: Chunk {
                    id: 3,
                    document_id: 8,
                    document_name: "dos.pdf".into(),
                    chunk_index: 0,
                    content: "Evidencia del segundo documento.".into(),
                    page_number: Some(2),
                    section: None,
                    embedding: vec![],
                },
                score: 0.7,
            },
        ];
        let (prompt, sources) = rag_system_prompt_with_documents(
            Profile::Documentation,
            &chunks,
            &[(7, "uno.pdf".into()), (8, "dos.pdf".into())],
        );
        assert_eq!(prompt.matches("DOCUMENTO A: uno.pdf").count(), 1);
        assert_eq!(prompt.matches("DOCUMENTO B: dos.pdf").count(), 1);
        assert_eq!(prompt.matches("DOCUMENTO A:").count(), 1);
        assert_eq!(prompt.matches("DOCUMENTO B:").count(), 1);
        assert!(!prompt.contains("Fuente 1"));
        assert!(!prompt.contains("Fuente 2"));
        assert!(prompt.contains("Fragmento 1 - pagina 1"));
        assert!(prompt.contains("Fragmento 2 - pagina 5"));
        assert!(
            prompt.contains("No trates cada fragmento como una fuente o documento independiente.")
        );
        assert!(prompt.contains("pagina 1"));
        assert!(prompt.contains("pagina 5"));
        assert_eq!(
            sources
                .iter()
                .map(|source| source.document_id)
                .collect::<std::collections::HashSet<_>>(),
            [7, 8].into_iter().collect()
        );
        let (missing_prompt, _) = rag_system_prompt_with_documents(
            Profile::Documentation,
            &chunks[..2],
            &[(7, "uno.pdf".into()), (9, "tres.pdf".into())],
        );
        assert!(missing_prompt.contains("DOCUMENTO B: tres.pdf"));
        assert!(missing_prompt.contains("Sin evidencia suficiente recuperada para este criterio."));
    }
}
