use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::Path};

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

pub fn file_hash(path: &Path) -> Result<String> {
    reject_sensitive(path)?;
    let bytes =
        std::fs::read(path).with_context(|| format!("No se pudo leer {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub fn parse(path: &Path) -> Result<ParsedDocument> {
    reject_sensitive(path)?;
    let extension = path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "txt" | "md" => parse_text(path),
        "pdf" => parse_pdf(path),
        _ => anyhow::bail!("Formato no soportado. Usa TXT, Markdown o PDF."),
    }
}

fn parse_text(path: &Path) -> Result<ParsedDocument> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("No se pudo leer {} como UTF-8", path.display()))?;
    if text.trim().is_empty() {
        anyhow::bail!("El documento esta vacio.");
    }
    let title = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("documento")
        .to_owned();
    Ok(ParsedDocument {
        title,
        sections: vec![ParsedSection {
            text,
            page_number: None,
            title: None,
        }],
        metadata: HashMap::new(),
    })
}

fn parse_pdf(path: &Path) -> Result<ParsedDocument> {
    let pdf = lopdf::Document::load(path).context("El PDF esta danado o no es valido")?;
    let mut sections = Vec::new();
    for (page_number, page_id) in pdf.get_pages() {
        let _ = page_id;
        let text = pdf.extract_text(&[page_number]).unwrap_or_default();
        if !text.trim().is_empty() {
            sections.push(ParsedSection {
                text,
                page_number: Some(page_number),
                title: None,
            });
        }
    }
    if sections.iter().map(|s| s.text.trim().len()).sum::<usize>() < 20 {
        anyhow::bail!("Este PDF parece requerir OCR. OCR no esta incluido en V1.");
    }
    Ok(ParsedDocument {
        title: path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("documento.pdf")
            .into(),
        sections,
        metadata: HashMap::new(),
    })
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
    fn parses_txt_and_markdown() {
        for ext in ["txt", "md"] {
            let mut file = tempfile::Builder::new()
                .suffix(&format!(".{ext}"))
                .tempfile_in("target")
                .unwrap();
            write!(file, "# Titulo\ncontenido conocido").unwrap();
            let doc = parse(file.path()).unwrap();
            assert!(doc.sections[0].text.contains("conocido"));
        }
    }
    #[test]
    fn hash_deduplicates_equal_content() {
        let mut a = tempfile::NamedTempFile::new_in("target").unwrap();
        let mut b = tempfile::NamedTempFile::new_in("target").unwrap();
        write!(a, "igual").unwrap();
        write!(b, "igual").unwrap();
        assert_eq!(file_hash(a.path()).unwrap(), file_hash(b.path()).unwrap());
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
    fn parses_text_pdf_and_keeps_page() {
        let mut pdf = lopdf::Document::with_version("1.5");
        let pages_id = pdf.new_object_id();
        let font_id = pdf.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = pdf.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
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
        let page_id = pdf.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        pdf.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = pdf.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
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
}
