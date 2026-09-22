use std::{
    fs,
    path::{Component, Path},
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result};

use super::types::{ScanPartialReason, ScanReport, ScannedCodeFile};

const MAX_FILES: usize = 1_000;
pub const MAX_FILE_BYTES: u64 = 512 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 20 * 1024 * 1024;
const MAX_DEPTH: usize = 20;

#[derive(Clone, Copy)]
struct ScanLimits {
    max_files: usize,
    max_file_bytes: u64,
    max_total_bytes: u64,
    max_depth: usize,
}

const DEFAULT_LIMITS: ScanLimits = ScanLimits {
    max_files: MAX_FILES,
    max_file_bytes: MAX_FILE_BYTES,
    max_total_bytes: MAX_TOTAL_BYTES,
    max_depth: MAX_DEPTH,
};

#[derive(Default)]
struct ScanControl {
    #[cfg(test)]
    fail_read_dir: Option<std::path::PathBuf>,
    #[cfg(test)]
    fail_metadata: Option<std::path::PathBuf>,
    #[cfg(test)]
    fail_canonicalize: Option<std::path::PathBuf>,
}

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
    scan_project(root, cancelled, DEFAULT_LIMITS, &ScanControl::default())
}

#[cfg(test)]
fn scan_project_with_limits(
    root: &Path,
    cancelled: &AtomicBool,
    limits: ScanLimits,
    fail_read_dir: Option<std::path::PathBuf>,
) -> Result<ScanReport> {
    scan_project_with_control(
        root,
        cancelled,
        limits,
        ScanControl {
            fail_read_dir,
            fail_metadata: None,
            fail_canonicalize: None,
        },
    )
}

#[cfg(test)]
fn scan_project_with_control(
    root: &Path,
    cancelled: &AtomicBool,
    limits: ScanLimits,
    control: ScanControl,
) -> Result<ScanReport> {
    scan_project(root, cancelled, limits, &control)
}

fn scan_project(
    root: &Path,
    cancelled: &AtomicBool,
    limits: ScanLimits,
    control: &ScanControl,
) -> Result<ScanReport> {
    let root = canonicalize_path(root, control)
        .with_context(|| format!("No se pudo abrir {}", root.display()))?;
    if !root.is_dir() {
        anyhow::bail!("La ruta seleccionada no es una carpeta");
    }
    let mut report = ScanReport::complete();
    let mut total_bytes = 0_u64;
    scan_directory(
        &root,
        &root,
        0,
        &mut total_bytes,
        &mut report,
        cancelled,
        limits,
        control,
        true,
    )?;
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
    match scanned_file(
        root,
        &absolute_path,
        DEFAULT_LIMITS,
        &ScanControl::default(),
    )? {
        ScannedFile::Indexable(file) => Ok(file),
        ScannedFile::PolicyExcluded | ScannedFile::TooLarge | ScannedFile::UnsafePath => {
            anyhow::bail!("Archivo no soportado o excluido")
        }
    }
}

fn scan_directory(
    root: &Path,
    directory: &Path,
    depth: usize,
    total_bytes: &mut u64,
    report: &mut ScanReport,
    cancelled: &AtomicBool,
    limits: ScanLimits,
    control: &ScanControl,
    is_root: bool,
) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        report.mark_partial(ScanPartialReason::Cancelled);
        return Ok(());
    }
    if depth > limits.max_depth {
        report.skipped += 1;
        report.mark_partial(ScanPartialReason::DepthLimit);
        return Ok(());
    }
    if report.files.len() >= limits.max_files {
        report.skipped += 1;
        report.mark_partial(ScanPartialReason::FileLimit);
        return Ok(());
    }
    if *total_bytes >= limits.max_total_bytes {
        report.skipped += 1;
        report.mark_partial(ScanPartialReason::TotalBytesLimit);
        return Ok(());
    }
    let entries = read_directory(directory, control);
    let entries = match entries {
        Ok(entries) => entries,
        Err(error) if is_root => {
            return Err(error).with_context(|| format!("No se pudo leer {}", directory.display()));
        }
        Err(error) => {
            report
                .errors
                .push(format!("{}: {error}", directory.display()));
            report.mark_partial(ScanPartialReason::TraversalError);
            return Ok(());
        }
    };
    for entry in entries {
        if cancelled.load(Ordering::Relaxed) {
            report.mark_partial(ScanPartialReason::Cancelled);
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.errors.push(error.to_string());
                report.mark_partial(ScanPartialReason::TraversalError);
                continue;
            }
        };
        let path = entry.path();
        let metadata = match symlink_metadata(&path, control) {
            Ok(metadata) => metadata,
            Err(error) => {
                record_error(report, &path, "metadata", &error);
                report.mark_partial(ScanPartialReason::TraversalError);
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            report.skipped += 1;
            continue;
        }
        if metadata.is_dir() {
            let file_name = entry.file_name();
            let name = match file_name.to_str() {
                Some(name) => name,
                None => {
                    record_error(
                        report,
                        &path,
                        "ruta",
                        "La ruta no puede representarse de forma segura como UTF-8",
                    );
                    report.mark_partial(ScanPartialReason::TraversalError);
                    continue;
                }
            };
            let canonical = match canonicalize_path(&path, control) {
                Ok(path) if path.starts_with(root) => path,
                Ok(_) => {
                    report.skipped += 1;
                    continue;
                }
                Err(error) => {
                    record_error(report, &path, "canonicalizacion", &error);
                    report.mark_partial(ScanPartialReason::TraversalError);
                    continue;
                }
            };
            if ignored_directory(name) || generated_browser_directory(root, &path) {
                report.skipped += 1;
            } else if let Err(error) = scan_directory(
                root,
                &canonical,
                depth + 1,
                total_bytes,
                report,
                cancelled,
                limits,
                control,
                false,
            ) {
                report.errors.push(error.to_string());
                report.mark_partial(ScanPartialReason::TraversalError);
            }
            continue;
        }
        if report.files.len() >= limits.max_files {
            report.skipped += 1;
            report.mark_partial(ScanPartialReason::FileLimit);
            continue;
        }
        if *total_bytes >= limits.max_total_bytes {
            report.skipped += 1;
            report.mark_partial(ScanPartialReason::TotalBytesLimit);
            continue;
        }
        match scanned_file(root, &path, limits, control) {
            Ok(ScannedFile::Indexable(file))
                if *total_bytes + file.size_bytes <= limits.max_total_bytes =>
            {
                *total_bytes += file.size_bytes;
                report.files.push(file);
                if report.files.len() >= limits.max_files {
                    report.mark_partial(ScanPartialReason::FileLimit);
                }
                if *total_bytes >= limits.max_total_bytes {
                    report.mark_partial(ScanPartialReason::TotalBytesLimit);
                }
            }
            Ok(ScannedFile::Indexable(_)) => {
                report.skipped += 1;
                report.mark_partial(ScanPartialReason::TotalBytesLimit);
            }
            Ok(ScannedFile::TooLarge) => {
                report.skipped += 1;
                report.mark_partial(ScanPartialReason::FileSizeLimit);
            }
            Ok(ScannedFile::PolicyExcluded) => report.skipped += 1,
            Ok(ScannedFile::UnsafePath) => {
                record_error(
                    report,
                    &path,
                    "ruta",
                    "La ruta no puede representarse de forma segura como UTF-8",
                );
                report.mark_partial(ScanPartialReason::TraversalError);
            }
            Err(error) => {
                record_error(report, &path, "escaneo", &error);
                report.mark_partial(ScanPartialReason::TraversalError);
            }
        }
    }
    Ok(())
}

fn record_error<E: std::fmt::Display + ?Sized>(
    report: &mut ScanReport,
    path: &Path,
    stage: &str,
    error: &E,
) {
    report
        .errors
        .push(format!("{} ({stage}): {error}", path.display()));
}

fn read_directory(directory: &Path, control: &ScanControl) -> std::io::Result<fs::ReadDir> {
    #[cfg(not(test))]
    let _ = control;
    #[cfg(test)]
    if control.fail_read_dir.as_deref() == Some(directory) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "error de acceso inyectado",
        ));
    }
    fs::read_dir(directory)
}

fn symlink_metadata(path: &Path, control: &ScanControl) -> std::io::Result<fs::Metadata> {
    #[cfg(not(test))]
    let _ = control;
    #[cfg(test)]
    if control.fail_metadata.as_deref() == Some(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "error de metadata inyectado",
        ));
    }
    fs::symlink_metadata(path)
}

fn canonicalize_path(path: &Path, control: &ScanControl) -> std::io::Result<std::path::PathBuf> {
    #[cfg(not(test))]
    let _ = control;
    #[cfg(test)]
    if control.fail_canonicalize.as_deref() == Some(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "error de canonicalizacion inyectado",
        ));
    }
    path.canonicalize()
}

enum ScannedFile {
    Indexable(ScannedCodeFile),
    PolicyExcluded,
    TooLarge,
    UnsafePath,
}

fn scanned_file(
    root: &Path,
    path: &Path,
    limits: ScanLimits,
    control: &ScanControl,
) -> Result<ScannedFile> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if sensitive_file(name) {
        return Ok(ScannedFile::PolicyExcluded);
    }
    let Some(language) = supported_language(path) else {
        return Ok(ScannedFile::PolicyExcluded);
    };
    let metadata = symlink_metadata(path, control)?;
    if metadata.file_type().is_symlink() {
        return Ok(ScannedFile::PolicyExcluded);
    }
    if !metadata.is_file() {
        return Ok(ScannedFile::PolicyExcluded);
    }
    if metadata.len() > limits.max_file_bytes {
        return Ok(ScannedFile::TooLarge);
    }
    let canonical = canonicalize_path(path, control)?;
    if !canonical.starts_with(root) {
        return Ok(ScannedFile::PolicyExcluded);
    }
    let relative_path = match relative_path_for_storage(root, &canonical) {
        Ok(path) => path,
        Err(_) => return Ok(ScannedFile::UnsafePath),
    };
    let extension = canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Ok(ScannedFile::Indexable(ScannedCodeFile {
        absolute_path: canonical,
        relative_path,
        extension,
        language: language.into(),
        size_bytes: metadata.len(),
    }))
}

pub fn relative_path_for_storage(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("{} queda fuera del proyecto", path.display()))?;
    let relative = relative
        .to_str()
        .context("La ruta no puede representarse de forma segura como UTF-8")?;
    if relative.is_empty()
        || Path::new(relative)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        anyhow::bail!("La ruta relativa no es segura para persistir");
    }
    Ok(relative.replace('\\', "/"))
}

pub fn path_for_storage(path: &Path) -> Result<String> {
    Ok(path
        .to_str()
        .context("La ruta del proyecto no puede representarse de forma segura como UTF-8")?
        .to_owned())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MissingPathVerification {
    pub confirmed_missing: Vec<String>,
    pub errors: Vec<String>,
}

pub fn verify_missing_code_files(
    root: &Path,
    report: &ScanReport,
    indexed_paths: impl IntoIterator<Item = String>,
) -> MissingPathVerification {
    let mut verification = MissingPathVerification::default();
    if !report.is_complete() {
        verification
            .errors
            .push("El scan no esta completo; no se puede verificar pruning".into());
        return verification;
    }
    let root = match root.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            verification
                .errors
                .push(format!("{} (canonicalizacion): {error}", root.display()));
            return verification;
        }
    };
    for relative_path in indexed_paths {
        if report
            .files
            .iter()
            .any(|file| file.relative_path == relative_path)
        {
            continue;
        }
        let candidate = match safe_relative_join(&root, &relative_path) {
            Ok(path) => path,
            Err(error) => {
                verification.errors.push(format!(
                    "{} (verificacion de pruning): {error}",
                    relative_path
                ));
                continue;
            }
        };
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                verification.confirmed_missing.push(relative_path);
            }
            Ok(metadata) if metadata.file_type().is_symlink() => verification.errors.push(format!(
                "{} (verificacion de pruning): enlace no verificable",
                candidate.display()
            )),
            Ok(_) => match candidate.canonicalize() {
                Ok(canonical) if canonical.starts_with(&root) => verification.errors.push(format!(
                    "{} (verificacion de pruning): el archivo reaparecio o cambio durante el scan",
                    candidate.display()
                )),
                Ok(_) => verification.errors.push(format!(
                    "{} (verificacion de pruning): la ruta sale del proyecto",
                    candidate.display()
                )),
                Err(error) => verification.errors.push(format!(
                    "{} (verificacion de pruning): {error}",
                    candidate.display()
                )),
            },
            Err(error) => verification.errors.push(format!(
                "{} (verificacion de pruning): {error}",
                candidate.display()
            )),
        }
    }
    verification
}

fn safe_relative_join(root: &Path, relative_path: &str) -> Result<std::path::PathBuf> {
    let relative = Path::new(relative_path);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        anyhow::bail!("ruta relativa insegura");
    }
    Ok(root.join(relative))
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
        assert!(matches!(
            scanned_file(
                root.path(),
                outside.path(),
                DEFAULT_LIMITS,
                &ScanControl::default(),
            )
            .unwrap(),
            ScannedFile::PolicyExcluded
        ));
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
        assert!(!report.is_complete());
        assert!(matches!(
            report.completeness,
            super::super::types::ScanCompleteness::Partial(ref reasons)
                if reasons.contains(&ScanPartialReason::Cancelled)
        ));
    }

    #[test]
    fn intentional_exclusions_do_not_make_a_scan_partial() {
        let directory = tempfile::tempdir_in("target").unwrap();
        fs::create_dir(directory.path().join("target")).unwrap();
        fs::write(
            directory.path().join("target/generated.rs"),
            "fn generated() {}",
        )
        .unwrap();
        fs::write(directory.path().join("secret.env"), "TOKEN=x").unwrap();
        fs::write(directory.path().join("image.png"), "not code").unwrap();

        let report = scan_project_with_cancel(directory.path(), &AtomicBool::new(false)).unwrap();

        assert!(report.is_complete());
        assert!(report.files.is_empty());
    }

    #[test]
    fn limits_and_access_errors_return_partial_reports() {
        let directory = tempfile::tempdir_in("target").unwrap();
        fs::write(directory.path().join("a.rs"), "fn a() {}").unwrap();
        fs::write(directory.path().join("b.rs"), "fn b() {}").unwrap();
        let one_file = ScanLimits {
            max_files: 1,
            ..DEFAULT_LIMITS
        };
        let report =
            scan_project_with_limits(directory.path(), &AtomicBool::new(false), one_file, None)
                .unwrap();
        assert!(!report.is_complete());

        let bytes = ScanLimits {
            max_total_bytes: 1,
            ..DEFAULT_LIMITS
        };
        let report =
            scan_project_with_limits(directory.path(), &AtomicBool::new(false), bytes, None)
                .unwrap();
        assert!(!report.is_complete());

        let nested = directory.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("deep.rs"), "fn deep() {}").unwrap();
        let depth = ScanLimits {
            max_depth: 0,
            ..DEFAULT_LIMITS
        };
        let report =
            scan_project_with_limits(directory.path(), &AtomicBool::new(false), depth, None)
                .unwrap();
        assert!(!report.is_complete());

        let oversized = ScanLimits {
            max_file_bytes: 1,
            ..DEFAULT_LIMITS
        };
        let report =
            scan_project_with_limits(directory.path(), &AtomicBool::new(false), oversized, None)
                .unwrap();
        assert!(!report.is_complete());

        let report = scan_project_with_limits(
            directory.path(),
            &AtomicBool::new(false),
            DEFAULT_LIMITS,
            Some(nested.canonicalize().unwrap()),
        )
        .unwrap();
        assert!(!report.is_complete());
        assert!(report.errors.iter().any(|error| error.contains("acceso")));
    }

    #[test]
    fn preserves_unicode_and_space_paths_without_lossy_storage() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let source = directory.path().join("carpeta con espacios/niño.rs");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "fn niño() {}").unwrap();

        let report = scan_project_with_cancel(directory.path(), &AtomicBool::new(false)).unwrap();

        assert!(report.is_complete());
        assert_eq!(
            report.files[0].relative_path,
            "carpeta con espacios/niño.rs"
        );
    }

    #[test]
    fn metadata_errors_are_partial_with_path_context() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let source = directory.path().join("main.rs");
        fs::write(&source, "fn main() {}").unwrap();

        let report = scan_project_with_control(
            directory.path(),
            &AtomicBool::new(false),
            DEFAULT_LIMITS,
            ScanControl {
                fail_read_dir: None,
                fail_metadata: Some(source.canonicalize().unwrap()),
                fail_canonicalize: None,
            },
        )
        .unwrap();

        assert!(!report.is_complete());
        assert!(report.errors.iter().any(|error| error.contains("metadata")));
        assert!(report.errors.iter().any(|error| error.contains("main.rs")));
    }

    #[test]
    fn verification_only_confirms_paths_that_remain_absent() {
        let directory = tempfile::tempdir_in("target").unwrap();
        fs::write(directory.path().join("kept.rs"), "fn kept() {}").unwrap();
        let report = scan_project_with_cancel(directory.path(), &AtomicBool::new(false)).unwrap();
        fs::write(directory.path().join("reappeared.rs"), "fn reappeared() {}").unwrap();

        let verification = verify_missing_code_files(
            directory.path(),
            &report,
            ["missing.rs".into(), "reappeared.rs".into()],
        );

        assert_eq!(verification.confirmed_missing, ["missing.rs"]);
        assert_eq!(verification.errors.len(), 1);
        assert!(verification.errors[0].contains("reaparecio"));
    }

    #[test]
    fn rejects_relative_paths_that_escape_the_root() {
        let root = tempfile::tempdir_in("target").unwrap();
        assert!(safe_relative_join(root.path(), "../outside.rs").is_err());
        assert!(safe_relative_join(root.path(), "/outside.rs").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn external_symlink_is_not_followed() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir_in("target").unwrap();
        let outside = tempfile::tempdir_in("target").unwrap();
        fs::write(outside.path().join("outside.rs"), "fn outside() {}").unwrap();
        symlink(outside.path(), root.path().join("linked")).unwrap();

        let report = scan_project_with_cancel(root.path(), &AtomicBool::new(false)).unwrap();

        assert!(report.is_complete());
        assert!(report.files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_not_persisted_lossily() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join(OsString::from_vec(vec![
            b'b', b'a', b'd', 0xff, b'.', b'r', b's',
        ]));
        fs::write(path, "fn invalid_name() {}").unwrap();

        let report = scan_project_with_cancel(directory.path(), &AtomicBool::new(false)).unwrap();

        assert!(!report.is_complete());
        assert!(report.files.is_empty());
        assert!(report.errors.iter().any(|error| error.contains("UTF-8")));
    }
}
