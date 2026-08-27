use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result};

use super::types::{ScanReport, ScannedCodeFile};

const MAX_FILES: usize = 1_000;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_TOTAL_BYTES: u64 = 20 * 1024 * 1024;
const MAX_DEPTH: usize = 20;

const SUPPORTED: &[(&str, &str)] = &[
    ("rs", "rust"),
    ("py", "python"),
    ("sql", "sql"),
    ("js", "javascript"),
    ("ts", "typescript"),
    ("tsx", "tsx"),
    ("jsx", "jsx"),
    ("html", "html"),
    ("css", "css"),
    ("json", "json"),
    ("toml", "toml"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("md", "markdown"),
    ("txt", "text"),
    ("c", "c"),
    ("h", "c"),
    ("cpp", "cpp"),
    ("hpp", "cpp"),
    ("java", "java"),
    ("cs", "csharp"),
    ("go", "go"),
];

pub fn supported_language(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    SUPPORTED
        .iter()
        .find_map(|(candidate, language)| (*candidate == extension).then_some(*language))
}

pub fn scan_project_with_cancel(root: &Path, cancelled: &AtomicBool) -> Result<ScanReport> {
    let root = root
        .canonicalize()
        .with_context(|| format!("No se pudo abrir {}", root.display()))?;
    if !root.is_dir() {
        anyhow::bail!("La ruta seleccionada no es una carpeta");
    }
    let mut report = ScanReport {
        files: Vec::new(),
        skipped: 0,
        errors: Vec::new(),
    };
    let mut total_bytes = 0_u64;
    scan_directory(&root, &root, 0, &mut total_bytes, &mut report, cancelled)?;
    report
        .files
        .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(report)
}

pub fn scan_single_file(path: &Path) -> Result<ScannedCodeFile> {
    let absolute_path = path
        .canonicalize()
        .with_context(|| format!("No se pudo abrir {}", path.display()))?;
    let root = absolute_path
        .parent()
        .context("El archivo no tiene carpeta")?;
    scanned_file(root, &absolute_path)?.context("Archivo no soportado o excluido")
}

fn scan_directory(
    root: &Path,
    directory: &Path,
    depth: usize,
    total_bytes: &mut u64,
    report: &mut ScanReport,
    cancelled: &AtomicBool,
) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        return Ok(());
    }
    if depth > MAX_DEPTH || report.files.len() >= MAX_FILES || *total_bytes >= MAX_TOTAL_BYTES {
        report.skipped += 1;
        return Ok(());
    }
    for entry in fs::read_dir(directory)
        .with_context(|| format!("No se pudo leer {}", directory.display()))?
    {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.errors.push(error.to_string());
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                report.errors.push(error.to_string());
                continue;
            }
        };
        if file_type.is_symlink() {
            report.skipped += 1;
            continue;
        }
        if file_type.is_dir() {
            if ignored_directory(&entry.file_name().to_string_lossy())
                || generated_browser_directory(root, &path)
            {
                report.skipped += 1;
            } else if let Err(error) =
                scan_directory(root, &path, depth + 1, total_bytes, report, cancelled)
            {
                report.errors.push(error.to_string());
            }
            continue;
        }
        if report.files.len() >= MAX_FILES || *total_bytes >= MAX_TOTAL_BYTES {
            report.skipped += 1;
            continue;
        }
        match scanned_file(root, &path) {
            Ok(Some(file)) if *total_bytes + file.size_bytes <= MAX_TOTAL_BYTES => {
                *total_bytes += file.size_bytes;
                report.files.push(file);
            }
            Ok(_) => report.skipped += 1,
            Err(error) => report.errors.push(error.to_string()),
        }
    }
    Ok(())
}

fn scanned_file(root: &Path, path: &Path) -> Result<Option<ScannedCodeFile>> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if sensitive_file(name) {
        return Ok(None);
    }
    let Some(language) = supported_language(path) else {
        return Ok(None);
    };
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Ok(None);
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root) {
        return Ok(None);
    }
    let relative_path = canonical
        .strip_prefix(root)?
        .to_string_lossy()
        .replace('\\', "/");
    let extension = canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Ok(Some(ScannedCodeFile {
        absolute_path: canonical,
        relative_path,
        extension,
        language: language.into(),
        size_bytes: metadata.len(),
    }))
}

fn ignored_directory(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        ".git"
            | "target"
            | "node_modules"
            | ".venv"
            | "venv"
            | "dist"
            | "build"
            | ".idea"
            | ".vscode"
            | "__pycache__"
            | ".cache"
            | ".pytest_cache"
            | ".mypy_cache"
            | "logs"
            | "log"
            | "tmp"
            | "temp"
            | "cache"
            | "caches"
            | "coverage"
            | ".coverage"
    )
}

fn generated_browser_directory(root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    components.iter().any(|component| {
        matches!(
            component.as_str(),
            "edge-cdp-profile"
                | "chrome-profile"
                | "chrome-cdp-profile"
                | "chromium-profile"
                | "chromium-cdp-profile"
                | "browser-profile"
                | "user data"
        )
    })
}

fn sensitive_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
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
            "id_rsa",
            "id_ed25519",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        || matches!(
            Path::new(&lower)
                .extension()
                .and_then(|value| value.to_str()),
            Some("pem" | "key" | "p12" | "pfx" | "crt" | "cer" | "der" | "jks" | "keystore")
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn recognizes_supported_extensions() {
        assert_eq!(supported_language(Path::new("src/main.rs")), Some("rust"));
        assert_eq!(supported_language(Path::new("app.tsx")), Some("tsx"));
        assert_eq!(supported_language(Path::new("image.png")), None);
    }

    #[test]
    fn excludes_heavy_directories_and_sensitive_files() {
        let directory = tempfile::tempdir_in("target").unwrap();
        fs::create_dir(directory.path().join("src")).unwrap();
        fs::create_dir(directory.path().join("target")).unwrap();
        fs::create_dir(directory.path().join("node_modules")).unwrap();
        fs::write(directory.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(directory.path().join("target/generated.rs"), "secret").unwrap();
        fs::write(directory.path().join("node_modules/index.js"), "secret").unwrap();
        fs::write(directory.path().join(".env"), "TOKEN=x").unwrap();
        for name in [
            ".env.local",
            "credentials.json",
            "client_secret.json",
            "private_key.pem",
            "access_token.txt",
            "id_rsa.key",
        ] {
            fs::write(directory.path().join(name), "sensible").unwrap();
        }
        let report = scan_project_with_cancel(directory.path(), &AtomicBool::new(false)).unwrap();
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].relative_path, "src/main.rs");
    }

    #[test]
    fn scanner_only_exposes_read_operations() {
        let mut file = tempfile::Builder::new()
            .suffix(".rs")
            .tempfile_in("target")
            .unwrap();
        write!(file, "fn unchanged() {{}}").unwrap();
        let before = fs::read(file.path()).unwrap();
        let _ = scan_single_file(file.path()).unwrap();
        assert_eq!(fs::read(file.path()).unwrap(), before);
    }

    #[test]
    fn scanner_rejects_a_file_outside_the_selected_root() {
        let root = tempfile::tempdir_in("target").unwrap();
        let outside = tempfile::Builder::new()
            .suffix(".rs")
            .tempfile_in("target")
            .unwrap();
        assert!(scanned_file(root.path(), outside.path()).unwrap().is_none());
    }

    #[test]
    fn excludes_browser_profiles_and_generated_logs_without_rejecting_project_json() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let browser = directory
            .path()
            .join("edge-cdp-profile/Default/Extensions/example");
        fs::create_dir_all(&browser).unwrap();
        fs::write(browser.join("service_worker_bin_config.json"), "{}").unwrap();
        fs::create_dir_all(directory.path().join("src")).unwrap();
        fs::write(directory.path().join("src/config.json"), "{\"real\":true}").unwrap();

        let report = scan_project_with_cancel(directory.path(), &AtomicBool::new(false)).unwrap();
        assert_eq!(
            report
                .files
                .iter()
                .map(|file| file.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["src/config.json"]
        );
    }

    #[test]
    fn cancellation_stops_scanning_without_returning_a_complete_file_set() {
        let directory = tempfile::tempdir_in("target").unwrap();
        fs::write(directory.path().join("main.rs"), "fn main() {}").unwrap();
        let cancelled = AtomicBool::new(true);

        let report = scan_project_with_cancel(directory.path(), &cancelled).unwrap();

        assert!(report.files.is_empty());
    }
}
