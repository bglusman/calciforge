//! Secret metadata shared by Calciforge control APIs, paste input, MCP
//! discovery, and the security proxy.
//!
//! Secret values stay in fnox or the configured vault. This sidecar
//! stores only non-secret policy such as destination host constraints.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const METADATA_FILE_NAME: &str = "secret-metadata.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMetadata {
    pub name: String,
    #[serde(default)]
    pub allowed_destinations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SecretMetadataStore {
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretMetadata>,
}

#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error("secret name {0:?} contains invalid characters")]
    InvalidName(String),
    #[error("invalid destination pattern {0:?}")]
    InvalidDestination(String),
    #[error("failed to read secret metadata at {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to parse secret metadata at {path}: {source}")]
    Parse {
        path: String,
        source: serde_json::Error,
    },
    #[error("failed to write secret metadata at {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
}

pub fn default_metadata_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CALCIFORGE_SECRET_METADATA_FILE").map(PathBuf::from) {
        return Some(path);
    }
    if let Some(path) =
        std::env::var_os("CALCIFORGE_SECRET_DESTINATION_ALLOWLIST_FILE").map(PathBuf::from)
    {
        return Some(path);
    }
    if let Some(path) = std::env::var_os("CALCIFORGE_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|base| base.join(METADATA_FILE_NAME))
    {
        return Some(path);
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|base| base.join("calciforge").join(METADATA_FILE_NAME))
    {
        return Some(path);
    }
    std::env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join(".config")
            .join("calciforge")
            .join(METADATA_FILE_NAME)
    })
}

pub fn load_default_metadata() -> Result<SecretMetadataStore, MetadataError> {
    let Some(path) = default_metadata_path() else {
        return Ok(SecretMetadataStore::default());
    };
    load_metadata(path)
}

pub fn load_metadata(path: impl Into<PathBuf>) -> Result<SecretMetadataStore, MetadataError> {
    let path = path.into();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SecretMetadataStore::default());
        }
        Err(source) => {
            return Err(MetadataError::Read {
                path: path.display().to_string(),
                source,
            });
        }
    };
    serde_json::from_str(&text).map_err(|source| MetadataError::Parse {
        path: path.display().to_string(),
        source,
    })
}

pub fn set_allowed_destinations(
    name: &str,
    allowed_destinations: &[String],
) -> Result<(), MetadataError> {
    if !crate::is_valid_secret_name(name) {
        return Err(MetadataError::InvalidName(name.to_string()));
    }
    let allowed_destinations = normalize_destinations(allowed_destinations)?;
    let Some(path) = default_metadata_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| MetadataError::Write {
            path: path.display().to_string(),
            source,
        })?;
    }
    let lock_path = path.with_extension(format!(
        "{}.lock",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("lock")
    ));
    let _lock = MetadataWriteLock::acquire(lock_path)?;

    let mut store = load_metadata(&path)?;
    store.secrets.insert(
        name.to_string(),
        SecretMetadata {
            name: name.to_string(),
            allowed_destinations,
        },
    );
    let text = serde_json::to_string_pretty(&store).map_err(|source| MetadataError::Parse {
        path: path.display().to_string(),
        source,
    })?;
    write_atomic(&path, &format!("{text}\n"))
}

struct MetadataWriteLock {
    path: PathBuf,
    _file: File,
}

impl MetadataWriteLock {
    fn acquire(path: PathBuf) -> Result<Self, MetadataError> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(file) => return Ok(Self { path, _file: file }),
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        return Err(MetadataError::Write {
                            path: path.display().to_string(),
                            source,
                        });
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(source) => {
                    return Err(MetadataError::Write {
                        path: path.display().to_string(),
                        source,
                    });
                }
            }
        }
    }
}

impl Drop for MetadataWriteLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn write_atomic(path: &PathBuf, text: &str) -> Result<(), MetadataError> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let unique = format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("secret-metadata"),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    );
    let tmp_path = parent.join(unique);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp_path)
        .map_err(|source| MetadataError::Write {
            path: tmp_path.display().to_string(),
            source,
        })?;
    file.write_all(text.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|source| MetadataError::Write {
            path: tmp_path.display().to_string(),
            source,
        })?;
    fs::rename(&tmp_path, path).map_err(|source| {
        let _ = fs::remove_file(&tmp_path);
        MetadataError::Write {
            path: path.display().to_string(),
            source,
        }
    })
}

pub fn metadata_for_names(names: &[String]) -> Result<Vec<SecretMetadata>, MetadataError> {
    let store = load_default_metadata()?;
    Ok(metadata_for_names_from_store(names, &store))
}

pub fn metadata_for_names_from_store(
    names: &[String],
    store: &SecretMetadataStore,
) -> Vec<SecretMetadata> {
    names
        .iter()
        .map(|name| {
            store.secrets.get(name).cloned().unwrap_or(SecretMetadata {
                name: name.clone(),
                allowed_destinations: Vec::new(),
            })
        })
        .collect()
}

pub fn parse_destinations(input: &str) -> Result<Vec<String>, MetadataError> {
    let raw = input
        .split([',', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    normalize_destinations(&raw)
}

fn normalize_destinations(values: &[String]) -> Result<Vec<String>, MetadataError> {
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim().trim_end_matches('/').to_ascii_lowercase();
        if value.is_empty() {
            continue;
        }
        if !is_valid_destination_pattern(&value) {
            return Err(MetadataError::InvalidDestination(value));
        }
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    Ok(normalized)
}

fn is_valid_destination_pattern(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '*' | '_' | ':'))
        && !value.contains("://")
        && !value.contains('/')
        && !value.starts_with('.')
        && !value.ends_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: tests that mutate process-global environment hold
            // ENV_MUTEX for the full guard lifetime, so no other test in
            // this module can concurrently read or write this key.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: the guard is only used while ENV_MUTEX is held by the
            // test, preserving the same serialization invariant as `set`.
            unsafe {
                if let Some(previous) = &self.previous {
                    std::env::set_var(self.key, previous);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn parse_destinations_normalizes_and_deduplicates() {
        let parsed = parse_destinations("API.EXAMPLE.com, *.Example.com\napi.example.com").unwrap();
        assert_eq!(parsed, vec!["api.example.com", "*.example.com"]);
    }

    #[test]
    fn parse_destinations_rejects_urls() {
        assert!(matches!(
            parse_destinations("https://api.example.com"),
            Err(MetadataError::InvalidDestination(_))
        ));
    }

    #[test]
    fn set_allowed_destinations_can_clear_existing_entry() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|err| err.into_inner());
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("secret-metadata.json");
        let _env = EnvVarGuard::set("CALCIFORGE_SECRET_METADATA_FILE", &path);

        set_allowed_destinations("API_KEY", &["api.example.com".to_string()]).unwrap();
        set_allowed_destinations("API_KEY", &[]).unwrap();

        let store = load_metadata(&path).unwrap();
        assert!(
            store
                .secrets
                .get("API_KEY")
                .unwrap()
                .allowed_destinations
                .is_empty()
        );
    }
}
