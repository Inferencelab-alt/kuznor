use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, Metadata},
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime},
};

pub const MAX_TEXT_FILE_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_PDF_FILE_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_PDF_PAGES: usize = 500;
pub const MAX_TEXT_PER_PDF_PAGE_CHARS: usize = 250_000;
pub const MAX_NORMALIZED_TEXT_CHARS: usize = 5_000_000;
pub const MAX_CHUNK_BYTES: usize = 12 * 1024;
pub const MAX_DOCUMENT_CHUNKS: usize = 2_000;
pub const EMBEDDING_BATCH_SIZE: usize = 8;
pub const DOCUMENT_PROCESSING_BUDGET: Duration = Duration::from_secs(10 * 60);

const HASH_BUFFER_BYTES: usize = 64 * 1024;
const READ_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct ParsedDocument {
    pub title: String,
    pub sections: Vec<ParsedSection>,
    #[allow(dead_code)]
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ParsedSection {
    pub text: String,
    pub page_number: Option<u32>,
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedDocumentPath {
    canonical_path: PathBuf,
    persistent_path: String,
    kind: String,
    fingerprint: FileFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    len: u64,
    modified: Option<SystemTime>,
}

impl ValidatedDocumentPath {
    pub fn path(&self) -> &Path {
        &self.canonical_path
    }
    pub fn persistent_path(&self) -> &str {
        &self.persistent_path
    }
    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn revalidate(&self) -> Result<()> {
        let current = validate_regular_document(&self.canonical_path)?;
        if current.canonical_path != self.canonical_path || current.fingerprint != self.fingerprint
        {
            anyhow::bail!(
                "El archivo cambio mientras se procesaba: {}",
                self.canonical_path.display()
            );
        }
        Ok(())
    }
}

pub fn is_sensitive_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || [
            "credential",
            "credentials",
            "secret",
            "secrets",
            "private_key",
            "api_key",
            "access_token",
            "auth_token",
            "password",
            "passwd",
        ]
        .iter()
        .any(|marker| name.contains(marker))
        || matches!(
            extension.as_str(),
            "pem" | "key" | "p12" | "pfx" | "crt" | "cer" | "der" | "jks" | "keystore"
        )
}

fn reject_sensitive(path: &Path) -> Result<()> {
    if is_sensitive_path(path) {
        anyhow::bail!("El archivo fue excluido por contener datos potencialmente sensibles");
    }
    Ok(())
}

pub fn validate_document_path(path: &Path) -> Result<ValidatedDocumentPath> {
    reject_sensitive(path)?;
    let mut validated = validate_regular_document(path)?;
    let kind = extension(path)?;
    ensure_file_limit(&validated.fingerprint, &kind)?;
    validated.kind = kind;
    Ok(validated)
}

fn validate_regular_document(path: &Path) -> Result<ValidatedDocumentPath> {
    let link_metadata =
        fs::symlink_metadata(path).map_err(|error| path_access_error(path, error))?;
    if is_link_or_reparse_point(&link_metadata) {
        anyhow::bail!(
            "No se permiten enlaces simbolicos o puntos de reparacion: {}",
            path.display()
        );
    }
    if !link_metadata.is_file() {
        anyhow::bail!(
            "El elemento seleccionado no es un archivo regular: {}",
            path.display()
        );
    }
    let canonical_path = path
        .canonicalize()
        .map_err(|error| path_access_error(path, error))?;
    let metadata =
        fs::metadata(&canonical_path).map_err(|error| path_access_error(&canonical_path, error))?;
    if !metadata.is_file() {
        anyhow::bail!(
            "El elemento seleccionado ya no es un archivo regular: {}",
            path.display()
        );
    }
    let persistent_path = canonical_path
        .to_str()
        .context("La ruta del archivo no puede guardarse sin conversion con perdida")?
        .to_owned();
    Ok(ValidatedDocumentPath {
        canonical_path,
        persistent_path,
        kind: String::new(),
        fingerprint: fingerprint(&metadata),
    })
}

fn extension(path: &Path) -> Result<String> {
    let kind = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match kind.as_str() {
        "txt" | "md" | "pdf" => Ok(kind),
        _ => anyhow::bail!("Formato no soportado. Usa TXT, Markdown o PDF."),
    }
}

fn ensure_file_limit(fingerprint: &FileFingerprint, kind: &str) -> Result<()> {
    let limit = if kind == "pdf" {
        MAX_PDF_FILE_BYTES
    } else {
        MAX_TEXT_FILE_BYTES
    };
    if fingerprint.len > limit {
        anyhow::bail!(
            "El archivo supera el limite de {} MiB para {} ({} bytes).",
            limit / 1024 / 1024,
            kind.to_ascii_uppercase(),
            limit
        );
    }
    Ok(())
}

fn fingerprint(metadata: &Metadata) -> FileFingerprint {
    FileFingerprint {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

fn is_link_or_reparse_point(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn path_access_error(path: &Path, error: std::io::Error) -> anyhow::Error {
    match error.kind() {
        std::io::ErrorKind::NotFound => {
            anyhow::anyhow!("El archivo desaparecio: {}", path.display())
        }
        std::io::ErrorKind::PermissionDenied => {
            anyhow::anyhow!("Acceso denegado al archivo: {}", path.display())
        }
        _ => anyhow::anyhow!("No se pudo acceder a {}: {error}", path.display()),
    }
}

pub fn ensure_active(cancelled: &AtomicBool, deadline: Instant) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        anyhow::bail!("Indexacion de documento cancelada");
    }
    if Instant::now() > deadline {
        anyhow::bail!("La ingesta supero el presupuesto maximo de 10 minutos");
    }
    Ok(())
}

#[cfg(test)]
pub fn file_hash(path: &Path) -> Result<String> {
    let cancelled = AtomicBool::new(false);
    let validated = validate_document_path(path)?;
    streaming_file_hash(
        &validated,
        &cancelled,
        Instant::now() + DOCUMENT_PROCESSING_BUDGET,
    )
}

pub fn streaming_file_hash(
    document: &ValidatedDocumentPath,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<String> {
    ensure_active(cancelled, deadline)?;
    document.revalidate()?;
    let mut file =
        File::open(document.path()).map_err(|error| path_access_error(document.path(), error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    loop {
        ensure_active(cancelled, deadline)?;
        let read = file
            .read(&mut buffer)
            .map_err(|error| path_access_error(document.path(), error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    document.revalidate()?;
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
pub fn parse(path: &Path) -> Result<ParsedDocument> {
    let cancelled = AtomicBool::new(false);
    let document = validate_document_path(path)?;
    parse_validated_document(
        &document,
        &cancelled,
        Instant::now() + DOCUMENT_PROCESSING_BUDGET,
    )
}

pub fn parse_validated_document(
    document: &ValidatedDocumentPath,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<ParsedDocument> {
    ensure_active(cancelled, deadline)?;
    document.revalidate()?;
    let parsed = match document.kind() {
        "txt" | "md" => parse_text(document, cancelled, deadline),
        "pdf" => parse_pdf(document, cancelled, deadline),
        _ => anyhow::bail!("Formato no soportado. Usa TXT, Markdown o PDF."),
    }?;
    ensure_active(cancelled, deadline)?;
    document.revalidate()?;
    Ok(parsed)
}

fn parse_text(
    document: &ValidatedDocumentPath,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<ParsedDocument> {
    let bytes = read_bounded(document, MAX_TEXT_FILE_BYTES, cancelled, deadline)?;
    if bytes.contains(&0) {
        anyhow::bail!("El archivo parece binario; contiene bytes NUL.");
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("El archivo no contiene UTF-8 valido."))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
    ensure_text_limit(text.chars().count())?;
    if text.trim().is_empty() {
        anyhow::bail!("El documento esta vacio.");
    }
    Ok(ParsedDocument {
        title: document_title(document.path(), "documento"),
        sections: vec![ParsedSection {
            text,
            page_number: None,
            title: None,
        }],
        metadata: HashMap::new(),
    })
}

fn read_bounded(
    document: &ValidatedDocumentPath,
    limit: u64,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<Vec<u8>> {
    let mut file =
        File::open(document.path()).map_err(|error| path_access_error(document.path(), error))?;
    let mut bytes =
        Vec::with_capacity((document.fingerprint.len.min(READ_BUFFER_BYTES as u64)) as usize);
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    loop {
        ensure_active(cancelled, deadline)?;
        let read = file
            .read(&mut buffer)
            .map_err(|error| path_access_error(document.path(), error))?;
        if read == 0 {
            break;
        }
        if bytes.len() + read > limit as usize {
            anyhow::bail!("El archivo supera el limite de {} bytes.", limit);
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    Ok(bytes)
}

fn parse_pdf(
    document: &ValidatedDocumentPath,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<ParsedDocument> {
    let pdf = lopdf::Document::load(document.path())
        .context("El PDF esta danado, es invalido o no puede leerse")?;
    ensure_active(cancelled, deadline)?;
    let pages = pdf.get_pages();
    if pages.len() > MAX_PDF_PAGES {
        anyhow::bail!(
            "El PDF tiene {} paginas y supera el limite de {} paginas.",
            pages.len(),
            MAX_PDF_PAGES
        );
    }
    let mut sections = Vec::new();
    let mut total_characters = 0_usize;
    for (page_number, _) in pages {
        ensure_active(cancelled, deadline)?;
        let text = pdf.extract_text(&[page_number]).with_context(|| {
            format!("No se pudo extraer texto de la pagina {page_number} del PDF")
        })?;
        let page_characters = text.chars().count();
        if page_characters > MAX_TEXT_PER_PDF_PAGE_CHARS {
            anyhow::bail!(
                "La pagina {page_number} del PDF supera el limite de {} caracteres extraidos.",
                MAX_TEXT_PER_PDF_PAGE_CHARS
            );
        }
        total_characters = total_characters.saturating_add(page_characters);
        ensure_text_limit(total_characters)?;
        if !text.trim().is_empty() {
            sections.push(ParsedSection {
                text,
                page_number: Some(page_number),
                title: None,
            });
        }
    }
    if sections
        .iter()
        .map(|section| section.text.trim().chars().count())
        .sum::<usize>()
        < 20
    {
        anyhow::bail!("No se encontro texto extraible. OCR no esta disponible en esta version.");
    }
    Ok(ParsedDocument {
        title: document_title(document.path(), "documento.pdf"),
        sections,
        metadata: HashMap::new(),
    })
}

fn ensure_text_limit(characters: usize) -> Result<()> {
    if characters > MAX_NORMALIZED_TEXT_CHARS {
        anyhow::bail!(
            "El texto extraido supera el limite de {} caracteres.",
            MAX_NORMALIZED_TEXT_CHARS
        );
    }
    Ok(())
}

fn document_title(path: &Path, fallback: &str) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(fallback)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{
        Object, Stream,
        content::{Content, Operation},
        dictionary,
    };
    use std::io::Write;

    #[test]
    fn parses_txt_and_markdown_and_normalizes_bom() {
        for ext in ["txt", "md"] {
            let mut file = tempfile::Builder::new()
                .suffix(&format!(".{ext}"))
                .tempfile_in("target")
                .unwrap();
            write!(file, "\u{feff}# Titulo\r\ncontenido conocido").unwrap();
            let doc = parse(file.path()).unwrap();
            assert!(doc.sections[0].text.contains("conocido"));
            assert!(!doc.sections[0].text.starts_with('\u{feff}'));
        }
    }

    #[test]
    fn text_rejects_empty_invalid_utf8_and_binary() {
        let mut empty = tempfile::Builder::new()
            .suffix(".txt")
            .tempfile_in("target")
            .unwrap();
        write!(empty, " \r\n\t").unwrap();
        assert!(
            parse(empty.path())
                .unwrap_err()
                .to_string()
                .contains("vacio")
        );
        let invalid = tempfile::Builder::new()
            .suffix(".txt")
            .tempfile_in("target")
            .unwrap();
        fs::write(invalid.path(), [0xff, 0xfe]).unwrap();
        assert!(
            parse(invalid.path())
                .unwrap_err()
                .to_string()
                .contains("UTF-8")
        );
        let binary = tempfile::Builder::new()
            .suffix(".md")
            .tempfile_in("target")
            .unwrap();
        fs::write(binary.path(), b"texto\0binario").unwrap();
        assert!(
            parse(binary.path())
                .unwrap_err()
                .to_string()
                .contains("NUL")
        );
    }

    #[test]
    fn hash_is_streaming_and_equal_for_equal_content() {
        let mut a = tempfile::Builder::new()
            .suffix(".txt")
            .tempfile_in("target")
            .unwrap();
        let mut b = tempfile::Builder::new()
            .suffix(".txt")
            .tempfile_in("target")
            .unwrap();
        write!(a, "{}", "igual".repeat(20_000)).unwrap();
        write!(b, "{}", "igual".repeat(20_000)).unwrap();
        assert_eq!(file_hash(a.path()).unwrap(), file_hash(b.path()).unwrap());
    }

    #[test]
    fn file_size_limits_and_cancellation_are_enforced_before_parsing() {
        let text = tempfile::Builder::new()
            .suffix(".txt")
            .tempfile_in("target")
            .unwrap();
        text.as_file().set_len(MAX_TEXT_FILE_BYTES + 1).unwrap();
        assert!(
            validate_document_path(text.path())
                .unwrap_err()
                .to_string()
                .contains("10 MiB")
        );

        let file = tempfile::Builder::new()
            .suffix(".txt")
            .tempfile_in("target")
            .unwrap();
        fs::write(file.path(), "contenido").unwrap();
        let validated = validate_document_path(file.path()).unwrap();
        let cancelled = AtomicBool::new(true);
        assert!(streaming_file_hash(&validated, &cancelled, Instant::now()).is_err());
    }

    #[test]
    fn rejects_sensitive_documents_before_reading() {
        for name in [
            ".env",
            ".env.local",
            "credentials.txt",
            "client_secret.md",
            "private_key.pem",
            "passwords.pdf",
        ] {
            let path = Path::new(name);
            assert!(is_sensitive_path(path), "{name} debe considerarse sensible");
            assert!(parse(path).is_err());
            assert!(file_hash(path).is_err());
        }
        assert!(!is_sensitive_path(Path::new("manual_publico.pdf")));
    }

    #[test]
    fn validated_file_change_is_rejected() {
        let file = tempfile::Builder::new()
            .suffix(".txt")
            .tempfile_in("target")
            .unwrap();
        fs::write(file.path(), "contenido inicial").unwrap();
        let validated = validate_document_path(file.path()).unwrap();
        fs::write(file.path(), "contenido que cambio y es mas largo").unwrap();
        assert!(
            validated
                .revalidate()
                .unwrap_err()
                .to_string()
                .contains("cambio")
        );
    }

    #[test]
    fn parses_text_pdf_and_keeps_page() {
        let mut pdf = lopdf::Document::with_version("1.5");
        let pages_id = pdf.new_object_id();
        let font_id = pdf.add_object(
            dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier" },
        );
        let resources_id =
            pdf.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 16.into()]),
                Operation::new("Td", vec![40.into(), 760.into()]),
                Operation::new(
                    "Tj",
                    vec![Object::string_literal("La clave del manual es PDF-42")],
                ),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap();
        let content_id = pdf.add_object(Stream::new(dictionary! {}, content));
        let page_id = pdf.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => content_id, "Resources" => resources_id, "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()] });
        pdf.objects.insert(
            pages_id,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1 },
            ),
        );
        let catalog_id = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        pdf.trailer.set("Root", catalog_id);
        let file = tempfile::Builder::new()
            .suffix(".pdf")
            .tempfile_in("target")
            .unwrap();
        pdf.save(file.path()).unwrap();
        let parsed = parse(file.path()).unwrap();
        assert!(parsed.sections[0].text.contains("PDF-42"));
        assert_eq!(parsed.sections[0].page_number, Some(1));
    }

    #[test]
    fn corrupt_pdf_is_rejected() {
        let corrupt = tempfile::Builder::new()
            .suffix(".pdf")
            .tempfile_in("target")
            .unwrap();
        fs::write(corrupt.path(), b"not a pdf").unwrap();
        assert!(parse(corrupt.path()).is_err());
    }
}
