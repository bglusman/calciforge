//! Calciforge-owned artifact storage helpers.
//!
//! Agent adapters can accept files from very different upstreams, but channel
//! delivery should see one constrained shape: local files under a per-run
//! Calciforge directory with known MIME, size, and path-containment checks.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use uuid::Uuid;

use crate::messages::{AttachmentKind, OutboundAttachment};

pub const DEFAULT_MAX_ARTIFACT_BYTES: u64 = 25 * 1024 * 1024;
pub const DEFAULT_MAX_ARTIFACT_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
pub const DEFAULT_MAX_ARTIFACTS: usize = 16;
pub const DEFAULT_ARTIFACT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

pub fn artifact_root(root_name: &str) -> PathBuf {
    std::env::temp_dir().join(root_name)
}

pub fn create_run_dir(root_name: &str) -> Result<PathBuf, String> {
    let root = artifact_root(root_name);
    ensure_artifact_root(&root)?;
    cleanup_old_run_dirs(&root, DEFAULT_ARTIFACT_RETENTION);
    let run_dir = root.join(Uuid::new_v4().to_string());
    create_private_dir(&run_dir)
        .map_err(|e| format!("failed to create artifact run directory: {e}"))?;
    Ok(run_dir)
}

fn ensure_artifact_root(root: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) => validate_artifact_root(root, &metadata)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match create_private_dir(root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(root).map_err(|e| {
                    format!("failed to inspect artifact root {}: {e}", root.display())
                })?;
                validate_artifact_root(root, &metadata)?;
            }
            Err(e) => return Err(format!("failed to create artifact root: {e}")),
        },
        Err(e) => {
            return Err(format!(
                "failed to inspect artifact root {}: {e}",
                root.display()
            ));
        }
    }
    set_private_dir_permissions(root)
}

fn validate_artifact_root(root: &Path, metadata: &std::fs::Metadata) -> Result<(), String> {
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "artifact root {} must not be a symlink",
            root.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "artifact root {} exists but is not a directory",
            root.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    std::fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(path)
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        format!(
            "failed to set private artifact directory permissions on {}: {e}",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

pub fn cleanup_old_run_dirs(root: &Path, retention: Duration) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified.elapsed().is_ok_and(|age| age > retention) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

pub fn collect_run_artifacts(
    artifact_dir: &Path,
    max_artifact_bytes: u64,
    max_artifacts: usize,
) -> Result<Vec<OutboundAttachment>, String> {
    let base = artifact_dir
        .canonicalize()
        .map_err(|e| format!("artifact directory is not accessible: {e}"))?;

    let mut attachments = Vec::new();
    let mut total_artifact_bytes: u64 = 0;
    let mut pending = VecDeque::from([base.clone()]);
    while let Some(dir) = pending.pop_front() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("failed to read artifact directory: {e}"))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("failed to read artifact entry: {e}"))?;
            let path = entry.path();
            let canonical = path.canonicalize().map_err(|e| {
                format!(
                    "failed to canonicalize artifact {}: {e}",
                    artifact_label(&path, &base)
                )
            })?;

            if !canonical.starts_with(&base) {
                return Err(format!(
                    "artifact path escaped run directory: {}",
                    artifact_label(&path, &base)
                ));
            }

            let metadata = std::fs::metadata(&canonical).map_err(|e| {
                format!(
                    "failed to inspect artifact {}: {e}",
                    artifact_label(&canonical, &base)
                )
            })?;
            if metadata.is_dir() {
                pending.push_back(canonical);
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if metadata.len() > max_artifact_bytes {
                return Err(format!(
                    "artifact {} exceeds {} byte limit",
                    artifact_label(&canonical, &base),
                    max_artifact_bytes
                ));
            }
            total_artifact_bytes = total_artifact_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| "artifact total byte count overflowed".to_string())?;
            if total_artifact_bytes > DEFAULT_MAX_ARTIFACT_TOTAL_BYTES {
                return Err(format!(
                    "artifacts exceed {} byte total limit",
                    DEFAULT_MAX_ARTIFACT_TOTAL_BYTES
                ));
            }
            if attachments.len() >= max_artifacts {
                return Err(format!(
                    "artifact count exceeds {} file limit",
                    max_artifacts
                ));
            }

            let mime_type = detect_mime_type(&canonical).map_err(|e| {
                format!(
                    "failed to inspect artifact {} MIME type: {e}",
                    artifact_label(&canonical, &base)
                )
            })?;
            attachments.push(OutboundAttachment {
                kind: AttachmentKind::from_mime(&mime_type),
                path: canonical,
                mime_type,
                caption: None,
                size_bytes: metadata.len(),
            });
        }
    }

    attachments.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(attachments)
}

fn artifact_label(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .ok()
        .and_then(|relative| {
            let value = relative.display().to_string();
            (!value.is_empty()).then_some(value)
        })
        .or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "artifact".to_string())
}

pub fn write_inline_attachment(
    run_dir: &Path,
    index: usize,
    name: Option<&str>,
    mime_type: Option<&str>,
    caption: Option<String>,
    data_base64: &str,
    max_bytes: usize,
) -> Result<OutboundAttachment, String> {
    let mime_type = mime_type
        .filter(|value| is_safe_mime_type(value))
        .unwrap_or("application/octet-stream")
        .to_string();
    let data_base64 = strip_data_url_prefix(data_base64);
    let max_encoded_len = max_bytes.div_ceil(3) * 4 + 4;
    if data_base64.len() > max_encoded_len {
        return Err(format!(
            "callback attachment base64 payload exceeds encoded limit of {max_encoded_len} bytes"
        ));
    }

    let data = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(data_base64.as_bytes())
            .map_err(|e| format!("callback attachment base64 error: {e}"))?
    };

    if data.is_empty() {
        return Err("callback attachment is empty".to_string());
    }
    if data.len() > max_bytes {
        return Err(format!(
            "callback attachment is {} bytes, limit is {}",
            data.len(),
            max_bytes
        ));
    }

    let filename = sanitize_attachment_name(name, &mime_type, index);
    let (path, mut file) = create_unique_attachment_file(run_dir, &filename, index)
        .map_err(|e| format!("failed to write callback attachment: {e}"))?;
    file.write_all(&data)
        .map_err(|e| format!("failed to write callback attachment: {e}"))?;

    Ok(OutboundAttachment {
        kind: AttachmentKind::from_mime(&mime_type),
        path,
        mime_type,
        caption: caption.filter(|caption| !caption.trim().is_empty()),
        size_bytes: data.len() as u64,
    })
}

pub fn detect_mime_type(path: &Path) -> Result<String, String> {
    let mut header = [0_u8; 16];
    let bytes_read = std::fs::File::open(path)
        .and_then(|mut file| file.read(&mut header))
        .map_err(|e| e.to_string())?;
    let bytes = &header[..bytes_read];
    if !bytes.is_empty() {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Ok("image/png".to_string());
        }
        if bytes.starts_with(b"\xff\xd8\xff") {
            return Ok("image/jpeg".to_string());
        }
        if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            return Ok("image/gif".to_string());
        }
        if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            return Ok("image/webp".to_string());
        }
    }

    Ok(match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("pdf") => "application/pdf",
        Some("txt") | Some("md") => "text/plain",
        _ => "application/octet-stream",
    }
    .to_string())
}

pub fn strip_data_url_prefix(value: &str) -> &str {
    value
        .split_once(',')
        .filter(|(prefix, _)| prefix.trim_start().starts_with("data:"))
        .map(|(_, data)| data)
        .unwrap_or(value)
        .trim()
}

pub fn is_safe_mime_type(value: &str) -> bool {
    let Some((top, sub)) = value.split_once('/') else {
        return false;
    };
    !top.is_empty()
        && !sub.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '+' | '-' | '.'))
}

pub fn sanitize_attachment_name(name: Option<&str>, mime_type: &str, index: usize) -> String {
    let raw = name.unwrap_or("").rsplit(['/', '\\']).next().unwrap_or("");
    let mut sanitized = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();

    while sanitized.starts_with('.') {
        sanitized.remove(0);
    }
    if sanitized.is_empty() {
        sanitized = format!("attachment-{}", index + 1);
    }
    if sanitized.len() > 128 {
        sanitized.truncate(128);
        sanitized = sanitized.trim_end_matches('.').to_string();
    }
    if !sanitized.contains('.') {
        sanitized.push_str(default_extension_for_mime(mime_type));
    }
    sanitized
}

fn create_unique_attachment_file(
    run_dir: &Path,
    filename: &str,
    index: usize,
) -> std::io::Result<(PathBuf, std::fs::File)> {
    for attempt in 0..1024 {
        let path = run_dir.join(disambiguated_attachment_name(filename, index, attempt));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no unique callback attachment filename available",
    ))
}

fn disambiguated_attachment_name(filename: &str, index: usize, attempt: usize) -> String {
    match attempt {
        0 => filename.to_string(),
        1 => format!("attachment-{}-{filename}", index + 1),
        _ => format!("attachment-{}-{attempt}-{filename}", index + 1),
    }
}

pub fn default_extension_for_mime(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "audio/mpeg" => ".mp3",
        "audio/wav" => ".wav",
        "video/mp4" => ".mp4",
        "text/plain" => ".txt",
        "application/pdf" => ".pdf",
        _ => ".bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_old_run_dirs_removes_only_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let run = temp.path().join("old-run");
        let file = temp.path().join("keep.txt");
        std::fs::create_dir(&run).expect("create run");
        std::fs::write(&file, "not a run dir").expect("write file");
        std::thread::sleep(Duration::from_millis(5));

        cleanup_old_run_dirs(temp.path(), Duration::ZERO);

        assert!(!run.exists(), "expired run directories should be removed");
        assert!(
            file.exists(),
            "non-directory files under the root should remain"
        );
    }

    #[test]
    fn inline_attachment_sanitizes_name_and_preserves_caption() {
        let temp = tempfile::tempdir().expect("tempdir");
        let attachment = write_inline_attachment(
            temp.path(),
            0,
            Some("../diagram"),
            Some("image/png"),
            Some("Generated diagram".to_string()),
            "iVBORw0KGgo=",
            DEFAULT_MAX_ARTIFACT_BYTES as usize,
        )
        .expect("inline attachment should be written");

        assert_eq!(
            attachment.path.file_name().and_then(|name| name.to_str()),
            Some("diagram.png")
        );
        assert_eq!(attachment.mime_type, "image/png");
        assert_eq!(attachment.caption.as_deref(), Some("Generated diagram"));
        assert!(std::fs::metadata(attachment.path).unwrap().is_file());
    }

    #[test]
    fn inline_attachment_disambiguates_existing_names_until_create_succeeds() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("diagram.png"), "existing").expect("write first");
        std::fs::write(temp.path().join("attachment-2-diagram.png"), "existing")
            .expect("write collision");

        let attachment = write_inline_attachment(
            temp.path(),
            1,
            Some("diagram.png"),
            Some("image/png"),
            None,
            "iVBORw0KGgo=",
            DEFAULT_MAX_ARTIFACT_BYTES as usize,
        )
        .expect("inline attachment should find an unused filename");

        assert_eq!(
            attachment.path.file_name().and_then(|name| name.to_str()),
            Some("attachment-2-2-diagram.png")
        );
    }

    #[test]
    fn collect_artifact_errors_do_not_expose_absolute_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("oversized.txt");
        std::fs::write(&artifact, "too large").expect("write artifact");

        let err =
            collect_run_artifacts(temp.path(), 4, DEFAULT_MAX_ARTIFACTS).expect_err("oversized");

        assert!(err.contains("artifact oversized.txt exceeds"));
        assert!(
            !err.contains(&temp.path().display().to_string()),
            "error leaked temp path: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn detect_mime_type_reports_read_errors() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("unreadable.png");
        std::fs::write(&artifact, b"\x89PNG\r\n\x1a\n").expect("write artifact");
        std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o000))
            .expect("chmod artifact");

        let err = detect_mime_type(&artifact).expect_err("unreadable artifact should fail");
        assert!(!err.is_empty());

        std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o600))
            .expect("restore perms");
    }

    #[cfg(unix)]
    #[test]
    fn create_run_dir_uses_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root_name = format!("calciforge-test-artifacts-{}", Uuid::new_v4());
        let root = artifact_root(&root_name);
        let run_dir = create_run_dir(&root_name).expect("run dir should be created");

        let root_mode = std::fs::metadata(&root).unwrap().permissions().mode() & 0o777;
        let run_mode = std::fs::metadata(&run_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(root_mode, 0o700);
        assert_eq!(run_mode, 0o700);

        std::fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn create_run_dir_rejects_symlinked_root() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root_name = format!("calciforge-test-artifacts-{}", Uuid::new_v4());
        let root = artifact_root(&root_name);
        symlink(temp.path(), &root).expect("create symlinked root");

        let err = create_run_dir(&root_name).expect_err("symlinked root must be rejected");
        assert!(
            err.contains("must not be a symlink"),
            "unexpected error: {err}"
        );

        std::fs::remove_file(root).ok();
    }
}
