//! Generic HTTP forwarding for voice passthrough endpoints.
//!
//! Both STT and TTS work the same way: receive an incoming request, optionally
//! run a shell hook on the body, then proxy the (possibly transformed) request
//! verbatim to the configured upstream URL.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::body::Bytes;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use tracing::{debug, info, warn};

use super::VoiceEndpointConfig;

const HOOK_TIMEOUT_SECS: u64 = 30;

static VOICE_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn voice_client() -> &'static reqwest::Client {
    VOICE_CLIENT.get_or_init(reqwest::Client::new)
}

/// Forward a raw request body (with its Content-Type) to the upstream STT URL.
///
/// Returns the upstream response body on success.
pub async fn forward_stt(
    config: &VoiceEndpointConfig,
    body: Bytes,
    content_type: &str,
) -> Result<(Bytes, String)> {
    let url = format!(
        "{}/v1/audio/transcriptions",
        config.url.trim_end_matches('/')
    );
    forward_raw(config, &url, body, content_type).await
}

/// Forward a raw request body to the upstream TTS URL.
///
/// Returns `(audio_bytes, content_type)`.
pub async fn forward_tts(
    config: &VoiceEndpointConfig,
    body: Bytes,
    content_type: &str,
) -> Result<(Bytes, String)> {
    let url = format!("{}/v1/audio/speech", config.url.trim_end_matches('/'));
    forward_raw(config, &url, body, content_type).await
}

/// Run an optional shell hook: pipe `input` to stdin, collect stdout.
///
/// If the hook path is None, returns the input unchanged.
/// On hook failure or timeout the original input is returned and a warning is logged —
/// the pipeline degrades gracefully rather than erroring.
pub async fn run_hook(hook_path: Option<&str>, input: Bytes) -> Bytes {
    let Some(path) = hook_path else {
        return input;
    };

    debug!(hook = %path, bytes = input.len(), "running voice hook");

    let task = tokio::task::spawn_blocking({
        let path = path.to_string();
        let input = input.clone();
        move || -> Result<Vec<u8>> {
            use std::io::Write;
            use std::process::{Command, Stdio};

            let mut child = Command::new(&path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("failed to spawn hook {path}"))?;

            child
                .stdin
                .take()
                .unwrap()
                .write_all(&input)
                .context("writing hook stdin")?;

            let out = child.wait_with_output().context("waiting for hook")?;

            if !out.stderr.is_empty() {
                let msg = String::from_utf8_lossy(&out.stderr);
                warn!(hook = %path, stderr = %msg.trim(), "hook stderr");
            }

            if out.status.success() {
                Ok(out.stdout)
            } else {
                anyhow::bail!("hook exited with status {}", out.status);
            }
        }
    });

    match tokio::time::timeout(Duration::from_secs(HOOK_TIMEOUT_SECS), task).await {
        Ok(Ok(Ok(out))) => {
            info!(hook = %path, output_bytes = out.len(), "voice hook succeeded");
            Bytes::from(out)
        }
        Ok(Ok(Err(e))) => {
            warn!(hook = %path, error = %e, "voice hook failed, passing through original");
            input
        }
        Ok(Err(e)) => {
            warn!(hook = %path, error = %e, "voice hook panicked, passing through original");
            input
        }
        Err(_) => {
            warn!(hook = %path, timeout_secs = HOOK_TIMEOUT_SECS, "voice hook timed out, passing through original");
            input
        }
    }
}

// ── internals ────────────────────────────────────────────────────────────────

async fn forward_raw(
    config: &VoiceEndpointConfig,
    url: &str,
    body: Bytes,
    content_type: &str,
) -> Result<(Bytes, String)> {
    debug!(url = %url, body_bytes = body.len(), "forwarding voice request");

    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(content_type).context("invalid content-type")?,
    );
    if let Some(ref key) = config.api_key {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {key}")).context("invalid api key")?,
        );
    }

    let resp = voice_client()
        .post(url)
        .timeout(Duration::from_secs(config.timeout_seconds))
        .headers(headers)
        .body(body)
        .send()
        .await
        .with_context(|| format!("POST {url} failed"))?;

    let status = resp.status();
    let resp_content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let resp_body = resp
        .bytes()
        .await
        .context("reading upstream response body")?;

    if !status.is_success() {
        let text = String::from_utf8_lossy(&resp_body);
        anyhow::bail!("upstream returned {status}: {}", text.trim());
    }

    debug!(
        url = %url,
        status = %status,
        response_bytes = resp_body.len(),
        response_content_type = %resp_content_type,
        "voice forward complete"
    );

    Ok((resp_body, resp_content_type))
}
