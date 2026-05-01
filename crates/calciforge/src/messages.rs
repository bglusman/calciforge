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
    pub controls: Vec<ChoiceControl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceControl {
    pub title: String,
    pub options: Vec<ChoiceOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    pub label: String,
    pub command: String,
    pub callback_data: Option<String>,
}

impl OutboundMessage {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            attachments: Vec::new(),
            controls: Vec::new(),
        }
    }

    pub fn with_control(mut self, control: ChoiceControl) -> Self {
        if !control.options.is_empty() {
            self.controls.push(control);
        }
        self
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

        if !self.controls.is_empty() {
            if !rendered.trim().is_empty() {
                rendered.push_str("\n\n");
            }
            for (control_index, control) in self.controls.iter().enumerate() {
                if control_index > 0 {
                    rendered.push('\n');
                }
                if !control.title.trim().is_empty() {
                    rendered.push_str(control.title.trim());
                    rendered.push('\n');
                }
                for option in &control.options {
                    rendered.push_str("- ");
                    rendered.push_str(option.label.trim());
                    rendered.push_str(": `");
                    rendered.push_str(option.command.trim());
                    rendered.push('`');
                    rendered.push('\n');
                }
                if rendered.ends_with('\n') {
                    rendered.pop();
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

impl ChoiceControl {
    pub fn new(title: impl Into<String>, options: Vec<ChoiceOption>) -> Self {
        Self {
            title: title.into(),
            options,
        }
    }
}

impl ChoiceOption {
    pub fn new(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            command: command.into(),
            callback_data: None,
        }
    }

    pub fn with_callback_data(mut self, callback_data: impl Into<String>) -> Self {
        self.callback_data = Some(callback_data.into());
        self
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
            controls: Vec::new(),
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

    #[test]
    fn fallback_renders_choice_commands_for_text_only_channels() {
        let msg = OutboundMessage::text("Choose an agent").with_control(ChoiceControl::new(
            "Options",
            vec![
                ChoiceOption::new("Librarian", "!agent switch librarian")
                    .with_callback_data("cf:agent:librarian"),
                ChoiceOption::new("Critic", "!agent switch critic")
                    .with_callback_data("cf:agent:critic"),
            ],
        ));

        let rendered = msg.render_text_fallback();
        assert!(rendered.contains("Choose an agent"));
        assert!(rendered.contains("Options"));
        assert!(
            rendered.contains("- Librarian: `!agent switch librarian`"),
            "fallback must expose the command users can type: {rendered}"
        );
        assert!(
            rendered.contains("- Critic: `!agent switch critic`"),
            "fallback must include every available choice: {rendered}"
        );
    }
}
