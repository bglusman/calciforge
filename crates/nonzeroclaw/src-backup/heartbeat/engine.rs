use crate::config::HeartbeatConfig;
use crate::observability::{Observer, ObserverEvent};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use tokio::time::{self, Duration};
use tracing::{info, warn};

// ── Structured task types ────────────────────────────────────────

/// Priority level for a heartbeat task.
///
/// Backport of upstream zeroclaw commit c86a067.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskPriority {
    Low,
    Medium,
    High,
}

impl fmt::Display for TaskPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

/// Status of a heartbeat task.
///
/// Backport of upstream zeroclaw commit c86a067.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Active,
    Paused,
    Completed,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Paused => write!(f, "paused"),
            Self::Completed => write!(f, "completed"),
        }
    }
}

/// A structured heartbeat task with priority and status metadata.
///
/// Backport of upstream zeroclaw commit c86a067.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatTask {
    pub text: String,
    pub priority: TaskPriority,
    pub status: TaskStatus,
}

impl HeartbeatTask {
    pub fn is_runnable(&self) -> bool {
        self.status == TaskStatus::Active
    }
}

impl fmt::Display for HeartbeatTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.priority, self.text)
    }
}

// ── Engine ───────────────────────────────────────────────────────

/// Heartbeat engine — reads HEARTBEAT.md and executes tasks periodically
pub struct HeartbeatEngine {
    config: HeartbeatConfig,
    workspace_dir: std::path::PathBuf,
    observer: Arc<dyn Observer>,
}

impl HeartbeatEngine {
    pub fn new(
        config: HeartbeatConfig,
        workspace_dir: std::path::PathBuf,
        observer: Arc<dyn Observer>,
    ) -> Self {
        Self {
            config,
            workspace_dir,
            observer,
        }
    }

    /// Start the heartbeat loop (runs until cancelled)
    pub async fn run(&self) -> Result<()> {
        if !self.config.enabled {
            info!("Heartbeat disabled");
            return Ok(());
        }

        let interval_mins = self.config.interval_minutes.max(5);
        info!("💓 Heartbeat started: every {} minutes", interval_mins);

        let mut interval = time::interval(Duration::from_secs(u64::from(interval_mins) * 60));

        loop {
            interval.tick().await;
            self.observer.record_event(&ObserverEvent::HeartbeatTick);

            match self.tick().await {
                Ok(tasks) => {
                    if tasks > 0 {
                        info!("💓 Heartbeat: processed {} tasks", tasks);
                    }
                }
                Err(e) => {
                    warn!("💓 Heartbeat error: {}", e);
                    self.observer.record_event(&ObserverEvent::Error {
                        component: "heartbeat".into(),
                        message: e.to_string(),
                    });
                }
            }
        }
    }

    /// Single heartbeat tick — read HEARTBEAT.md and return task count
    async fn tick(&self) -> Result<usize> {
        Ok(self.collect_tasks().await?.len())
    }

    /// Read HEARTBEAT.md and return all parsed structured tasks.
    pub async fn collect_tasks(&self) -> Result<Vec<HeartbeatTask>> {
        let heartbeat_path = self.workspace_dir.join("HEARTBEAT.md");
        if !heartbeat_path.exists() {
            return Ok(Vec::new());
        }
        let content = tokio::fs::read_to_string(&heartbeat_path).await?;
        Ok(Self::parse_tasks(&content))
    }

    /// Collect only runnable (active) tasks, sorted by priority (high first).
    ///
    /// Backport of upstream zeroclaw commit c86a067.
    pub async fn collect_runnable_tasks(&self) -> Result<Vec<HeartbeatTask>> {
        let mut tasks: Vec<HeartbeatTask> = self
            .collect_tasks()
            .await?
            .into_iter()
            .filter(HeartbeatTask::is_runnable)
            .collect();
        // Sort by priority descending (High > Medium > Low)
        tasks.sort_by(|a, b| b.priority.cmp(&a.priority));
        Ok(tasks)
    }

    /// Parse tasks from HEARTBEAT.md with structured metadata support.
    ///
    /// Supports both legacy flat format and new structured format:
    ///
    /// Legacy:
    ///   `- Check email`  →  medium priority, active status
    ///
    /// Structured:
    ///   `- [high] Check email`           →  high priority, active
    ///   `- [low|paused] Review old PRs`  →  low priority, paused
    ///   `- [completed] Old task`         →  medium priority, completed
    ///
    /// Backport of upstream zeroclaw commit c86a067.
    fn parse_tasks(content: &str) -> Vec<HeartbeatTask> {
        content
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                let text = trimmed.strip_prefix("- ")?;
                if text.is_empty() {
                    return None;
                }
                Some(Self::parse_task_line(text))
            })
            .collect()
    }

    /// Parse a single task line into a structured `HeartbeatTask`.
    ///
    /// Format: `[priority|status] task text` or just `task text`.
    fn parse_task_line(text: &str) -> HeartbeatTask {
        if let Some(rest) = text.strip_prefix('[') {
            if let Some((meta, task_text)) = rest.split_once(']') {
                let task_text = task_text.trim();
                if !task_text.is_empty() {
                    let (priority, status) = Self::parse_meta(meta);
                    return HeartbeatTask {
                        text: task_text.to_string(),
                        priority,
                        status,
                    };
                }
            }
        }
        // No metadata — default to medium/active
        HeartbeatTask {
            text: text.to_string(),
            priority: TaskPriority::Medium,
            status: TaskStatus::Active,
        }
    }

    /// Parse metadata tags like `high`, `low|paused`, `completed`.
    fn parse_meta(meta: &str) -> (TaskPriority, TaskStatus) {
        let mut priority = TaskPriority::Medium;
        let mut status = TaskStatus::Active;

        for part in meta.split('|') {
            match part.trim().to_ascii_lowercase().as_str() {
                "high" => priority = TaskPriority::High,
                "medium" | "med" => priority = TaskPriority::Medium,
                "low" => priority = TaskPriority::Low,
                "active" => status = TaskStatus::Active,
                "paused" | "pause" => status = TaskStatus::Paused,
                "completed" | "complete" | "done" => status = TaskStatus::Completed,
                _ => {}
            }
        }

        (priority, status)
    }

    /// Build the Phase 1 LLM decision prompt for two-phase heartbeat.
    ///
    /// Phase 1 asks the LLM (at temperature 0.0) whether any tasks need to run
    /// right now, saving API cost on quiet periods.
    ///
    /// Backport of upstream zeroclaw commit c86a067.
    pub fn build_decision_prompt(tasks: &[HeartbeatTask]) -> String {
        let mut prompt = String::from(
            "You are a heartbeat scheduler. Review the following periodic tasks and decide \
             whether any should be executed right now.\n\n\
             Consider:\n\
             - Task priority (high tasks are more urgent)\n\
             - Whether the task is time-sensitive or can wait\n\
             - Whether running the task now would provide value\n\n\
             Tasks:\n",
        );

        for (i, task) in tasks.iter().enumerate() {
            use std::fmt::Write;
            let _ = writeln!(prompt, "{}. [{}] {}", i + 1, task.priority, task.text);
        }

        prompt.push_str(
            "\nRespond with ONLY one of:\n\
             - `run: 1,2,3` (comma-separated task numbers to execute)\n\
             - `skip` (nothing needs to run right now)\n\n\
             Be conservative — skip if tasks are routine and not time-sensitive.",
        );

        prompt
    }

    /// Parse the Phase 1 LLM decision response.
    ///
    /// Returns indices of tasks to run (0-based), or empty vec if skipped.
    ///
    /// Backport of upstream zeroclaw commit c86a067.
    pub fn parse_decision_response(response: &str, task_count: usize) -> Vec<usize> {
        let trimmed = response.trim().to_ascii_lowercase();

        if trimmed == "skip" || trimmed.starts_with("skip") {
            return Vec::new();
        }

        // Look for "run: 1,2,3" pattern
        let numbers_part = if let Some(after_run) = trimmed.strip_prefix("run:") {
            after_run.trim()
        } else if let Some(after_run) = trimmed.strip_prefix("run ") {
            after_run.trim()
        } else {
            // Try to parse as bare numbers
            trimmed.as_str()
        };

        numbers_part
            .split(',')
            .filter_map(|s| {
                let n: usize = s.trim().parse().ok()?;
                // 1-based → 0-based, validate range
                if n >= 1 && n <= task_count {
                    Some(n - 1)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Create a default HEARTBEAT.md if it doesn't exist
    pub async fn ensure_heartbeat_file(workspace_dir: &Path) -> Result<()> {
        let path = workspace_dir.join("HEARTBEAT.md");
        if !path.exists() {
            let default = "# Periodic Tasks\n\n\
                           # Add tasks below (one per line, starting with `- `)\n\
                           # The agent will check this file on each heartbeat tick.\n\
                           #\n\
                           # Structured format (backport from zeroclaw c86a067):\n\
                           #   - [high] Check critical alerts\n\
                           #   - [medium] Check email\n\
                           #   - [low|paused] Review old PRs\n\
                           #   - [completed] Old finished task\n\
                           #\n\
                           # Examples:\n\
                           # - Check my email for important messages\n\
                           # - Review my calendar for upcoming events\n\
                           # - Check the weather forecast\n";
            tokio::fs::write(&path, default).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tasks_basic() {
        let content = "# Tasks\n\n- Check email\n- Review calendar\nNot a task\n- Third task";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].text, "Check email");
        assert_eq!(tasks[0].priority, TaskPriority::Medium);
        assert_eq!(tasks[0].status, TaskStatus::Active);
        assert_eq!(tasks[1].text, "Review calendar");
        assert_eq!(tasks[2].text, "Third task");
    }

    #[test]
    fn parse_tasks_empty_content() {
        assert!(HeartbeatEngine::parse_tasks("").is_empty());
    }

    #[test]
    fn parse_tasks_only_comments() {
        let tasks = HeartbeatEngine::parse_tasks("# No tasks here\n\nJust comments\n# Another");
        assert!(tasks.is_empty());
    }

    #[test]
    fn parse_tasks_with_leading_whitespace() {
        let content = "  - Indented task\n\t- Tab indented";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].text, "Indented task");
        assert_eq!(tasks[1].text, "Tab indented");
    }

    #[test]
    fn parse_tasks_dash_without_space_ignored() {
        let content = "- Real task\n-\n- Another";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].text, "Real task");
        assert_eq!(tasks[1].text, "Another");
    }

    #[test]
    fn parse_tasks_trailing_space_bullet_trimmed_to_dash() {
        let content = "- ";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 0);
    }

    #[test]
    fn parse_tasks_bullet_with_content_after_spaces() {
        let content = "- hello  ";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "hello");
    }

    #[test]
    fn parse_tasks_unicode() {
        let content = "- Check email 📧\n- Review calendar 📅\n- 日本語タスク";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 3);
        assert!(tasks[0].text.contains("📧"));
        assert!(tasks[2].text.contains("日本語"));
    }

    #[test]
    fn parse_tasks_mixed_markdown() {
        let content = "# Periodic Tasks\n\n## Quick\n- Task A\n\n## Long\n- Task B\n\n* Not a dash bullet\n1. Not numbered";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].text, "Task A");
        assert_eq!(tasks[1].text, "Task B");
    }

    #[test]
    fn parse_tasks_single_task() {
        let tasks = HeartbeatEngine::parse_tasks("- Only one");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "Only one");
    }

    #[test]
    fn parse_tasks_many_tasks() {
        let content: String = (0..100).fold(String::new(), |mut s, i| {
            use std::fmt::Write;
            let _ = writeln!(s, "- Task {i}");
            s
        });
        let tasks = HeartbeatEngine::parse_tasks(&content);
        assert_eq!(tasks.len(), 100);
        assert_eq!(tasks[99].text, "Task 99");
    }

    // ── Structured task format tests ─────────────────────────────

    #[test]
    fn parse_task_line_high_priority() {
        let task = HeartbeatEngine::parse_task_line("[high] Check alerts");
        assert_eq!(task.text, "Check alerts");
        assert_eq!(task.priority, TaskPriority::High);
        assert_eq!(task.status, TaskStatus::Active);
    }

    #[test]
    fn parse_task_line_low_priority_paused() {
        let task = HeartbeatEngine::parse_task_line("[low|paused] Review old PRs");
        assert_eq!(task.text, "Review old PRs");
        assert_eq!(task.priority, TaskPriority::Low);
        assert_eq!(task.status, TaskStatus::Paused);
    }

    #[test]
    fn parse_task_line_completed() {
        let task = HeartbeatEngine::parse_task_line("[completed] Old task");
        assert_eq!(task.text, "Old task");
        assert_eq!(task.priority, TaskPriority::Medium);
        assert_eq!(task.status, TaskStatus::Completed);
    }

    #[test]
    fn parse_task_line_legacy_no_metadata() {
        let task = HeartbeatEngine::parse_task_line("Check email");
        assert_eq!(task.text, "Check email");
        assert_eq!(task.priority, TaskPriority::Medium);
        assert_eq!(task.status, TaskStatus::Active);
    }

    #[test]
    fn parse_tasks_structured_format() {
        let content = "- [high] Critical alert\n- [low|paused] Low priority\n- [completed] Done task\n- Regular task";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 4);
        assert_eq!(tasks[0].priority, TaskPriority::High);
        assert_eq!(tasks[0].status, TaskStatus::Active);
        assert_eq!(tasks[1].priority, TaskPriority::Low);
        assert_eq!(tasks[1].status, TaskStatus::Paused);
        assert_eq!(tasks[2].status, TaskStatus::Completed);
        assert_eq!(tasks[3].priority, TaskPriority::Medium);
        assert_eq!(tasks[3].status, TaskStatus::Active);
    }

    #[test]
    fn is_runnable_only_active() {
        let active = HeartbeatTask {
            text: "t".into(),
            priority: TaskPriority::Medium,
            status: TaskStatus::Active,
        };
        let paused = HeartbeatTask {
            text: "t".into(),
            priority: TaskPriority::Medium,
            status: TaskStatus::Paused,
        };
        let completed = HeartbeatTask {
            text: "t".into(),
            priority: TaskPriority::Medium,
            status: TaskStatus::Completed,
        };
        assert!(active.is_runnable());
        assert!(!paused.is_runnable());
        assert!(!completed.is_runnable());
    }

    #[test]
    fn parse_decision_response_skip() {
        assert!(HeartbeatEngine::parse_decision_response("skip", 3).is_empty());
        assert!(HeartbeatEngine::parse_decision_response("skip - no tasks needed", 3).is_empty());
        assert!(HeartbeatEngine::parse_decision_response("SKIP", 3).is_empty());
    }

    #[test]
    fn parse_decision_response_run() {
        let indices = HeartbeatEngine::parse_decision_response("run: 1,3", 3);
        assert_eq!(indices, vec![0, 2]);
    }

    #[test]
    fn parse_decision_response_run_single() {
        let indices = HeartbeatEngine::parse_decision_response("run: 2", 3);
        assert_eq!(indices, vec![1]);
    }

    #[test]
    fn parse_decision_response_out_of_range_filtered() {
        let indices = HeartbeatEngine::parse_decision_response("run: 1,5", 3);
        assert_eq!(indices, vec![0]); // 5 is out of range for 3 tasks
    }

    #[test]
    fn build_decision_prompt_contains_tasks() {
        let tasks = vec![
            HeartbeatTask {
                text: "Check email".into(),
                priority: TaskPriority::High,
                status: TaskStatus::Active,
            },
            HeartbeatTask {
                text: "Review PR".into(),
                priority: TaskPriority::Low,
                status: TaskStatus::Active,
            },
        ];
        let prompt = HeartbeatEngine::build_decision_prompt(&tasks);
        assert!(prompt.contains("Check email"));
        assert!(prompt.contains("Review PR"));
        assert!(prompt.contains("high"));
        assert!(prompt.contains("low"));
        assert!(prompt.contains("run:"));
        assert!(prompt.contains("skip"));
    }

    #[tokio::test]
    async fn ensure_heartbeat_file_creates_file() {
        let dir = std::env::temp_dir().join("zeroclaw_test_heartbeat");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        HeartbeatEngine::ensure_heartbeat_file(&dir).await.unwrap();

        let path = dir.join("HEARTBEAT.md");
        assert!(path.exists());
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(content.contains("Periodic Tasks"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn ensure_heartbeat_file_does_not_overwrite() {
        let dir = std::env::temp_dir().join("zeroclaw_test_heartbeat_no_overwrite");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let path = dir.join("HEARTBEAT.md");
        tokio::fs::write(&path, "- My custom task").await.unwrap();

        HeartbeatEngine::ensure_heartbeat_file(&dir).await.unwrap();

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content, "- My custom task");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn tick_returns_zero_when_no_file() {
        let dir = std::env::temp_dir().join("zeroclaw_test_tick_no_file");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let observer: Arc<dyn Observer> = Arc::new(crate::observability::NoopObserver);
        let engine = HeartbeatEngine::new(
            HeartbeatConfig {
                enabled: true,
                interval_minutes: 30,
            },
            dir.clone(),
            observer,
        );
        let count = engine.tick().await.unwrap();
        assert_eq!(count, 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn tick_counts_tasks_from_file() {
        let dir = std::env::temp_dir().join("zeroclaw_test_tick_count");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        tokio::fs::write(dir.join("HEARTBEAT.md"), "- A\n- B\n- C")
            .await
            .unwrap();

        let observer: Arc<dyn Observer> = Arc::new(crate::observability::NoopObserver);
        let engine = HeartbeatEngine::new(
            HeartbeatConfig {
                enabled: true,
                interval_minutes: 30,
            },
            dir.clone(),
            observer,
        );
        let count = engine.tick().await.unwrap();
        assert_eq!(count, 3);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_returns_immediately_when_disabled() {
        let observer: Arc<dyn Observer> = Arc::new(crate::observability::NoopObserver);
        let engine = HeartbeatEngine::new(
            HeartbeatConfig {
                enabled: false,
                interval_minutes: 30,
            },
            std::env::temp_dir(),
            observer,
        );
        // Should return Ok immediately, not loop forever
        let result = engine.run().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn collect_runnable_tasks_filters_and_sorts() {
        let dir = std::env::temp_dir().join("zeroclaw_test_runnable");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        tokio::fs::write(
            dir.join("HEARTBEAT.md"),
            "- [low] Low task\n- [high] High task\n- [completed] Done\n- [paused] Paused",
        )
        .await
        .unwrap();

        let observer: Arc<dyn Observer> = Arc::new(crate::observability::NoopObserver);
        let engine = HeartbeatEngine::new(
            HeartbeatConfig {
                enabled: true,
                interval_minutes: 30,
            },
            dir.clone(),
            observer,
        );

        let runnable = engine.collect_runnable_tasks().await.unwrap();
        // Only active tasks (low + high), sorted high-first
        assert_eq!(runnable.len(), 2);
        assert_eq!(runnable[0].priority, TaskPriority::High);
        assert_eq!(runnable[1].priority, TaskPriority::Low);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
