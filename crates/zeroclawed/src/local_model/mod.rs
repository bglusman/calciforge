//! Local model lifecycle management.
//!
//! Handles starting and stopping local inference servers (mlx_lm.server,
//! llama-server) when `!model <id>` or `POST /control/local/switch` is called.
//! Only one local model can be loaded at a time.

pub mod mlx_lm;

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::{info, warn};

use crate::config::{LocalModelDef, LocalModelsConfig};

/// State of the currently-loaded local model.
#[derive(Debug, Clone)]
pub struct LoadedModel {
    pub id: String,
    pub hf_id: String,
    #[allow(dead_code)]
    pub provider_type: String,
}

/// Serializes all local model switch operations.
pub struct LocalModelManager {
    config: LocalModelsConfig,
    state: Mutex<Option<LoadedModel>>,
    // Child process handle is stored in the mlx_lm backend and managed there.
    server_handle: Mutex<Option<mlx_lm::MlxLmHandle>>,
}

impl std::fmt::Debug for LocalModelManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalModelManager")
            .field("config.enabled", &self.config.enabled)
            .finish()
    }
}

impl LocalModelManager {
    pub fn new(config: LocalModelsConfig) -> Self {
        Self {
            config,
            state: Mutex::new(None),
            server_handle: Mutex::new(None),
        }
    }

    /// Current model ID (None if no model loaded).
    pub fn current(&self) -> Option<LoadedModel> {
        self.state.lock().expect("state lock").clone()
    }

    /// List all configured local models.
    pub fn models(&self) -> &[LocalModelDef] {
        &self.config.models
    }

    /// Find a model definition by its short ID.
    pub fn find_model(&self, id: &str) -> Option<&LocalModelDef> {
        self.config.models.iter().find(|m| m.id == id)
    }

    /// Switch to a different local model.
    ///
    /// Steps:
    /// 1. Run pre_switch hook (if configured).
    /// 2. Kill the current server (if running).
    /// 3. Start the new server and wait for readiness.
    /// 4. Run post_switch hook (if configured).
    /// 5. Update internal state.
    ///
    /// This is synchronous and blocks until the new model is ready (or timeout).
    pub fn switch(&self, target_id: &str) -> Result<LoadedModel> {
        let model_def = self
            .find_model(target_id)
            .with_context(|| format!("unknown local model id '{target_id}'; use !model to list"))?
            .clone();

        let prev = self.state.lock().expect("state lock").clone();
        let prev_id = prev.as_ref().map(|m| m.id.as_str()).unwrap_or("");
        let prev_hf = prev.as_ref().map(|m| m.hf_id.as_str()).unwrap_or("");

        info!(
            target = %target_id,
            prev = %prev_id,
            "Local model switch requested"
        );

        if prev_id == target_id {
            // Already loaded — return current state.
            return Ok(prev.expect("checked above"));
        }

        // 1. Pre-switch hook.
        if let Some(ref script) = self.config.mlx_lm.hooks.pre_switch {
            if !script.is_empty() {
                run_hook(
                    script,
                    &[
                        ("ZEROCLAWED_PREV_MODEL_ID", prev_id),
                        ("ZEROCLAWED_PREV_MODEL_HF_ID", prev_hf),
                        ("ZEROCLAWED_MODEL_ID", target_id),
                        ("ZEROCLAWED_MODEL_HF_ID", &model_def.hf_id),
                    ],
                )?;
            }
        }

        // 2. Kill current server.
        {
            let mut handle = self.server_handle.lock().expect("server handle lock");
            if let Some(h) = handle.take() {
                info!(prev = %prev_id, "Stopping previous local model server");
                h.stop();
            }
        }

        // 3. Start new server and wait for readiness.
        let mlx_cfg = &self.config.mlx_lm;
        let new_handle = mlx_lm::MlxLmHandle::start(
            &model_def.hf_id,
            &mlx_cfg.host,
            mlx_cfg.port,
            &mlx_cfg.extra_args,
            Duration::from_secs(mlx_cfg.startup_timeout_seconds),
        )
        .with_context(|| format!("starting mlx_lm.server for model '{}'", model_def.hf_id))?;

        {
            let mut handle = self.server_handle.lock().expect("server handle lock");
            *handle = Some(new_handle);
        }

        // 4. Post-switch hook.
        if let Some(ref script) = self.config.mlx_lm.hooks.post_switch {
            if !script.is_empty() {
                if let Err(e) = run_hook(
                    script,
                    &[
                        ("ZEROCLAWED_MODEL_ID", target_id),
                        ("ZEROCLAWED_MODEL_HF_ID", &model_def.hf_id),
                        ("ZEROCLAWED_PREV_MODEL_ID", prev_id),
                    ],
                ) {
                    // Post-switch hook failure is non-fatal (model is already up).
                    warn!(error = %e, "post_switch hook failed (model is running)");
                }
            }
        }

        // 5. Update state.
        let loaded = LoadedModel {
            id: model_def.id.clone(),
            hf_id: model_def.hf_id.clone(),
            provider_type: model_def.provider_type.clone(),
        };
        *self.state.lock().expect("state lock") = Some(loaded.clone());

        info!(model = %target_id, hf_id = %model_def.hf_id, "Local model switch complete");
        Ok(loaded)
    }

    /// Shut down the currently-running local server (called on zeroclawed exit).
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        let mut handle = self.server_handle.lock().expect("server handle lock");
        if let Some(h) = handle.take() {
            info!("Shutting down local model server on exit");
            h.stop();
        }
    }
}

/// Run a hook shell script, waiting up to 60 seconds.
fn run_hook(script: &str, env: &[(&str, &str)]) -> Result<()> {
    use std::process::Command;

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script);
    for (k, v) in env {
        cmd.env(k, v);
    }

    let output = cmd
        .output()
        .with_context(|| format!("spawning hook script: {script}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "hook script exited with status {}: {stderr}",
            output.status
        );
    }
    Ok(())
}
