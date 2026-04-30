//! Channel-agnostic outbound message envelope.
//!
//! Channels can render this as native media when they support it, or fall back
//! to text links/paths while richer channel senders are being added.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Audio,
    Video,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundAttachment {
    pub kind: AttachmentKind,
    pub path: PathBuf,
    pub mime_type: String,
    pub caption: Option<String>,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutboundMessage {
    pub text: Option<String>,
    pub attachments: Vec<OutboundAttachment>,
}

impl OutboundMessage {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            attachments: Vec::new(),
        }
    }

    pub fn response_len(&self) -> usize {
        self.render_text_fallback().len()
    }

    pub fn render_text_fallback(&self) -> String {
        let mut rendered = self.text.clone().unwrap_or_default();

        if !self.attachments.is_empty() {
            if !rendered.trim().is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str("Attachments:");
            for attachment in &self.attachments {
                rendered.push_str("\n- ");
                rendered.push_str(&attachment.mime_type);
                rendered.push_str(": ");
                let name = attachment
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("artifact");
                rendered.push_str(name);
                rendered.push_str(" (");
                rendered.push_str(&attachment.size_bytes.to_string());
                rendered.push_str(" bytes)");
                if let Some(caption) = &attachment.caption {
                    if !caption.trim().is_empty() {
                        rendered.push_str(" - ");
                        rendered.push_str(caption.trim());
                    }
                }
            }
        }

        if rendered.trim().is_empty() {
            "Agent completed without a text response.".to_string()
        } else {
            rendered
        }
    }
}

impl AttachmentKind {
    pub fn from_mime(mime_type: &str) -> Self {
        if mime_type.starts_with("image/") {
            Self::Image
        } else if mime_type.starts_with("audio/") {
            Self::Audio
        } else if mime_type.starts_with("video/") {
            Self::Video
        } else {
            Self::File
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_does_not_expose_local_artifact_path() {
        let msg = OutboundMessage {
            text: Some("done".to_string()),
            attachments: vec![OutboundAttachment {
                kind: AttachmentKind::Image,
                path: PathBuf::from("/tmp/calciforge-artifacts/run-1/out.png"),
                mime_type: "image/png".to_string(),
                caption: Some("preview".to_string()),
                size_bytes: 42,
            }],
        };

        let rendered = msg.render_text_fallback();
        assert!(rendered.contains("out.png"));
        assert!(rendered.contains("42 bytes"));
        assert!(rendered.contains("preview"));
        assert!(
            !rendered.contains("/tmp/calciforge-artifacts"),
            "chat fallback must not leak local storage paths: {rendered}"
        );
    }
}
