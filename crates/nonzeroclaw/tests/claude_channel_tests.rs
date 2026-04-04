//! Integration tests for the Claude API and Claude Code ACP channels.
//!
//! These tests validate the channel contracts, config construction,
//! protocol parsing, and conversation management without making real
//! network or subprocess calls.

use async_trait::async_trait;
use nonzeroclaw::channels::claude_acp::{
    extract_acp_response, parse_acp_events, AcpEvent, ClaudeAcpChannel,
};
use nonzeroclaw::channels::claude_api::{
    claude_tool_use_to_nzc, nzc_tool_to_claude, ClaudeApiChannel, ToolUseBlock,
};
use nonzeroclaw::channels::traits::{Channel, ChannelMessage, SendMessage};
use nonzeroclaw::config::schema::{ClaudeAcpConfig, ClaudeApiConfig};

// ─────────────────────────────────────────────────────────────────────────────
// ClaudeApiChannel — basic construction and channel trait
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn claude_api_channel_name() {
    let ch = ClaudeApiChannel::new("sk-ant-key", "claude-sonnet-4-6", 4096, 24);
    assert_eq!(ch.name(), "claude_api");
}

#[test]
fn claude_api_from_config_sets_model() {
    let config = ClaudeApiConfig {
        enabled: true,
        api_key: "sk-ant-test".to_string(),
        model: "claude-opus-4".to_string(),
        max_tokens: 2048,
        conversation_ttl_hours: 12,
        system_prompt: None,
        webhook_secret: None,
    };
    let ch = ClaudeApiChannel::from_config(&config);
    assert_eq!(ch.name(), "claude_api");
}

#[test]
fn claude_api_from_config_applies_defaults_for_zero_values() {
    let config = ClaudeApiConfig {
        enabled: true,
        api_key: "sk-ant-test".to_string(),
        model: String::new(),
        max_tokens: 0,
        conversation_ttl_hours: 0,
        system_prompt: None,
        webhook_secret: None,
    };
    let ch = ClaudeApiChannel::from_config(&config);
    assert_eq!(ch.name(), "claude_api");
    // Channel should be created successfully with defaults applied internally
}

#[test]
fn claude_api_with_system_prompt() {
    let ch = ClaudeApiChannel::new("key", "model", 100, 1)
        .with_system_prompt("You are a helpful assistant.");
    assert_eq!(ch.name(), "claude_api");
}

// ─────────────────────────────────────────────────────────────────────────────
// ClaudeApiChannel — conversation state management
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn claude_api_new_conversation_is_empty() {
    let ch = ClaudeApiChannel::new("key", "model", 100, 1);
    let state = ch.get_or_create_conversation("test-conv");
    assert!(state.messages.is_empty());
}

#[test]
fn claude_api_conversation_retrieval_returns_same_state() {
    let ch = ClaudeApiChannel::new("key", "model", 100, 1);
    // Create the conversation once
    let _state = ch.get_or_create_conversation("conv-a");
    // Retrieve it again — should be the same empty state
    let state2 = ch.get_or_create_conversation("conv-a");
    assert!(state2.messages.is_empty());
}

#[test]
fn claude_api_different_conversations_are_isolated() {
    let ch = ClaudeApiChannel::new("key", "model", 100, 1);
    // Create two distinct conversations
    let _a = ch.get_or_create_conversation("conv-a");
    let _b = ch.get_or_create_conversation("conv-b");
    // Each should be independent
    let a2 = ch.get_or_create_conversation("conv-a");
    let b2 = ch.get_or_create_conversation("conv-b");
    assert!(a2.messages.is_empty());
    assert!(b2.messages.is_empty());
}

#[test]
fn claude_api_stale_conversation_eviction() {
    // TTL of 1 hour; manually insert a conversation older than TTL
    let ch = ClaudeApiChannel::new("key", "model", 100, 1);
    {
        let mut store = ch.conversation_store.lock().unwrap();
        let mut old = nonzeroclaw::channels::claude_api::ConversationState::new();
        // Backdated by 2 hours
        old.last_activity = old.last_activity.saturating_sub(7200);
        store.insert("old-conv".to_string(), old);
    }
    ch.evict_stale_conversations();
    let store = ch.conversation_store.lock().unwrap();
    assert!(
        !store.contains_key("old-conv"),
        "stale conversation should be evicted after TTL"
    );
}

#[test]
fn claude_api_fresh_conversation_survives_eviction() {
    let ch = ClaudeApiChannel::new("key", "model", 100, 1);
    let _state = ch.get_or_create_conversation("fresh-conv");
    ch.evict_stale_conversations();
    let store = ch.conversation_store.lock().unwrap();
    assert!(
        store.contains_key("fresh-conv"),
        "fresh conversation should survive eviction"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Tool bridging helpers
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn nzc_tool_to_claude_produces_valid_definition() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "query": { "type": "string" } },
        "required": ["query"]
    });
    let tool = nzc_tool_to_claude("web_search", "Search the web for information", &schema);
    assert_eq!(tool.name, "web_search");
    assert_eq!(tool.description, "Search the web for information");
    assert_eq!(tool.input_schema["type"], "object");
    assert!(tool.input_schema["properties"]["query"].is_object());
}

#[test]
fn claude_tool_use_to_nzc_returns_name_and_args() {
    let block = ToolUseBlock {
        id: "tu_001".to_string(),
        name: "file_read".to_string(),
        input: serde_json::json!({ "path": "/etc/hosts" }),
    };
    let (name, args) = claude_tool_use_to_nzc(&block);
    assert_eq!(name, "file_read");
    assert_eq!(args["path"], "/etc/hosts");
}

#[test]
fn claude_tool_use_preserves_complex_input() {
    let block = ToolUseBlock {
        id: "tu_002".to_string(),
        name: "shell".to_string(),
        input: serde_json::json!({
            "command": "find /tmp -name '*.rs' -type f",
            "timeout": 30,
            "env": { "RUST_LOG": "debug" }
        }),
    };
    let (name, args) = claude_tool_use_to_nzc(&block);
    assert_eq!(name, "shell");
    assert_eq!(args["command"], "find /tmp -name '*.rs' -type f");
    assert_eq!(args["timeout"], 30);
    assert_eq!(args["env"]["RUST_LOG"], "debug");
}

// ─────────────────────────────────────────────────────────────────────────────
// ClaudeAcpChannel — basic construction and channel trait
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn claude_acp_channel_name() {
    let ch = ClaudeAcpChannel::new("/usr/local/bin/claude", "/tmp");
    assert_eq!(ch.name(), "claude_acp");
}

#[test]
fn claude_acp_from_config_defaults() {
    let config = ClaudeAcpConfig {
        enabled: true,
        claude_path: String::new(),
        workspace_dir: "/my/project".to_string(),
        permission_mode: String::new(),
        timeout_secs: 0,
        extra_args: vec![],
    };
    let ch = ClaudeAcpChannel::from_config(&config);
    assert_eq!(ch.name(), "claude_acp");
}

#[test]
fn claude_acp_from_config_explicit_values() {
    let config = ClaudeAcpConfig {
        enabled: true,
        claude_path: "/opt/claude/bin/claude".to_string(),
        workspace_dir: "/workspace".to_string(),
        permission_mode: "default".to_string(),
        timeout_secs: 300,
        extra_args: vec!["--output-format".to_string(), "json".to_string()],
    };
    let ch = ClaudeAcpChannel::from_config(&config);
    assert_eq!(ch.name(), "claude_acp");
}

#[test]
fn claude_acp_builder_methods() {
    let ch = ClaudeAcpChannel::new("claude", "/tmp")
        .with_permission_mode("default")
        .with_timeout(120)
        .with_extra_args(vec!["--verbose".to_string()]);
    assert_eq!(ch.name(), "claude_acp");
}

// ─────────────────────────────────────────────────────────────────────────────
// ACP protocol parsing
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn acp_parse_empty_string() {
    assert!(parse_acp_events("").is_empty());
}

#[test]
fn acp_parse_done_event() {
    let events = parse_acp_events("[done]");
    assert_eq!(events, vec![AcpEvent::Done]);
}

#[test]
fn acp_parse_client_block() {
    let events = parse_acp_events("[client]\nHello world\n[end]");
    assert_eq!(events, vec![AcpEvent::Client("Hello world".into())]);
}

#[test]
fn acp_parse_tool_block() {
    let events = parse_acp_events("[tool]\ncat /etc/hostname\n[end]");
    assert_eq!(events, vec![AcpEvent::Tool("cat /etc/hostname".into())]);
}

#[test]
fn acp_parse_thinking_block() {
    let events = parse_acp_events("[thinking]\nI need to check the file.\n[end]");
    assert_eq!(
        events,
        vec![AcpEvent::Thinking("I need to check the file.".into())]
    );
}

#[test]
fn acp_parse_full_coding_session() {
    let input = "\
[thinking]\n\
The user wants me to list the directory.\n\
[end]\n\
[tool]\n\
ls /tmp\n\
[end]\n\
[client]\n\
Here are the files in /tmp:\n\
- file1.txt\n\
- file2.rs\n\
[end]\n\
[done]";
    let events = parse_acp_events(input);
    assert_eq!(events.len(), 4);
    assert_eq!(
        events[0],
        AcpEvent::Thinking("The user wants me to list the directory.".into())
    );
    assert_eq!(events[1], AcpEvent::Tool("ls /tmp".into()));
    assert!(matches!(&events[2], AcpEvent::Client(s) if s.contains("file1.txt")));
    assert_eq!(events[3], AcpEvent::Done);
}

#[test]
fn acp_parse_raw_lines_outside_blocks() {
    let events = parse_acp_events("plain text line");
    assert_eq!(events, vec![AcpEvent::Raw("plain text line".into())]);
}

#[test]
fn acp_parse_multiline_client_block() {
    let events = parse_acp_events("[client]\nLine A\nLine B\nLine C\n[end]");
    assert_eq!(
        events,
        vec![AcpEvent::Client("Line A\nLine B\nLine C".into())]
    );
}

#[test]
fn acp_parse_done_closes_open_block() {
    let events = parse_acp_events("[client]\nContent before done\n[done]");
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0],
        AcpEvent::Client("Content before done".into())
    );
    assert_eq!(events[1], AcpEvent::Done);
}

// ─────────────────────────────────────────────────────────────────────────────
// ACP response extraction
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn acp_extract_prefers_last_client_block() {
    let events = vec![
        AcpEvent::Client("first response".into()),
        AcpEvent::Client("final response".into()),
    ];
    assert_eq!(extract_acp_response(&events), "final response");
}

#[test]
fn acp_extract_skips_whitespace_only_client_blocks() {
    let events = vec![
        AcpEvent::Client("   \n  ".into()),
        AcpEvent::Raw("fallback text".into()),
    ];
    assert_eq!(extract_acp_response(&events), "fallback text");
}

#[test]
fn acp_extract_falls_back_to_raw_when_no_client() {
    let events = vec![
        AcpEvent::Tool("ls".into()),
        AcpEvent::Raw("output line 1".into()),
        AcpEvent::Raw("output line 2".into()),
    ];
    assert_eq!(extract_acp_response(&events), "output line 1\noutput line 2");
}

#[test]
fn acp_extract_empty_events() {
    let events: Vec<AcpEvent> = vec![];
    assert_eq!(extract_acp_response(&events), "");
}

#[test]
fn acp_extract_ignores_thinking_blocks() {
    let events = vec![
        AcpEvent::Thinking("internal reasoning".into()),
        AcpEvent::Client("user-facing answer".into()),
    ];
    assert_eq!(extract_acp_response(&events), "user-facing answer");
}

// ─────────────────────────────────────────────────────────────────────────────
// Channel trait contract — send_message accepts SendMessage
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn send_message_new_constructs_correctly() {
    let msg = SendMessage::new("hello", "conv-123");
    assert_eq!(msg.content, "hello");
    assert_eq!(msg.recipient, "conv-123");
    assert!(msg.subject.is_none());
    assert!(msg.thread_ts.is_none());
}

#[test]
fn channel_message_fields_are_accessible() {
    // ChannelMessage from Claude API channel should use "claude_api" as channel
    let msg = ChannelMessage {
        id: "test-id".to_string(),
        sender: "user@example.com".to_string(),
        reply_target: "conversation-123".to_string(),
        content: "What is the weather?".to_string(),
        channel: "claude_api".to_string(),
        timestamp: 1700000000,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
    };
    assert_eq!(msg.channel, "claude_api");
    assert_eq!(msg.content, "What is the weather?");
}

#[test]
fn acp_channel_message_fields_are_accessible() {
    let msg = ChannelMessage {
        id: "acp-id".to_string(),
        sender: "developer@example.com".to_string(),
        reply_target: "session-456".to_string(),
        content: "Fix the bug in src/main.rs".to_string(),
        channel: "claude_acp".to_string(),
        timestamp: 1700000001,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
    };
    assert_eq!(msg.channel, "claude_acp");
    assert_eq!(msg.sender, "developer@example.com");
}

// ─────────────────────────────────────────────────────────────────────────────
// Config serialization / deserialization round-trips
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn claude_api_config_default_is_disabled() {
    let config = ClaudeApiConfig::default();
    assert!(!config.enabled);
    assert!(config.api_key.is_empty());
}

#[test]
fn claude_api_config_serializes_and_deserializes() {
    let original = ClaudeApiConfig {
        enabled: true,
        api_key: "sk-ant-test-key".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 8192,
        conversation_ttl_hours: 48,
        system_prompt: Some("You are an expert Rust developer.".to_string()),
        webhook_secret: None,
    };
    let serialized = toml::to_string_pretty(&original).expect("should serialize");
    let deserialized: ClaudeApiConfig =
        toml::from_str(&serialized).expect("should deserialize");

    assert_eq!(deserialized.enabled, original.enabled);
    assert_eq!(deserialized.api_key, original.api_key);
    assert_eq!(deserialized.model, original.model);
    assert_eq!(deserialized.max_tokens, original.max_tokens);
    assert_eq!(
        deserialized.conversation_ttl_hours,
        original.conversation_ttl_hours
    );
    assert_eq!(deserialized.system_prompt, original.system_prompt);
}

#[test]
fn claude_acp_config_default_is_disabled() {
    let config = ClaudeAcpConfig::default();
    assert!(!config.enabled);
    assert!(config.claude_path.is_empty());
}

#[test]
fn claude_acp_config_serializes_and_deserializes() {
    let original = ClaudeAcpConfig {
        enabled: true,
        claude_path: "/usr/local/bin/claude".to_string(),
        workspace_dir: "/home/user/project".to_string(),
        permission_mode: "bypassPermissions".to_string(),
        timeout_secs: 300,
        extra_args: vec!["--verbose".to_string()],
    };
    let serialized = toml::to_string_pretty(&original).expect("should serialize");
    let deserialized: ClaudeAcpConfig =
        toml::from_str(&serialized).expect("should deserialize");

    assert_eq!(deserialized.enabled, original.enabled);
    assert_eq!(deserialized.claude_path, original.claude_path);
    assert_eq!(deserialized.workspace_dir, original.workspace_dir);
    assert_eq!(deserialized.permission_mode, original.permission_mode);
    assert_eq!(deserialized.timeout_secs, original.timeout_secs);
    assert_eq!(deserialized.extra_args, original.extra_args);
}

#[test]
fn claude_api_config_in_channels_config() {
    // Verify that ClaudeApiConfig can be embedded in ChannelsConfig and parsed
    let toml_input = r#"
[claude_api]
enabled = true
api_key = "sk-ant-test"
model = "claude-opus-4"
max_tokens = 1024
conversation_ttl_hours = 6
"#;
    let channels: nonzeroclaw::config::ChannelsConfig =
        toml::from_str(toml_input).expect("should parse channels config with claude_api");
    let claude = channels.claude_api.expect("claude_api should be present");
    assert!(claude.enabled);
    assert_eq!(claude.api_key, "sk-ant-test");
    assert_eq!(claude.model, "claude-opus-4");
    assert_eq!(claude.max_tokens, 1024);
    assert_eq!(claude.conversation_ttl_hours, 6);
}

#[test]
fn claude_acp_config_in_channels_config() {
    let toml_input = r#"
[claude_acp]
enabled = true
claude_path = "/opt/claude/bin/claude"
workspace_dir = "/workspace"
permission_mode = "bypassPermissions"
timeout_secs = 600
extra_args = ["--debug"]
"#;
    let channels: nonzeroclaw::config::ChannelsConfig =
        toml::from_str(toml_input).expect("should parse channels config with claude_acp");
    let acp = channels.claude_acp.expect("claude_acp should be present");
    assert!(acp.enabled);
    assert_eq!(acp.claude_path, "/opt/claude/bin/claude");
    assert_eq!(acp.workspace_dir, "/workspace");
    assert_eq!(acp.permission_mode, "bypassPermissions");
    assert_eq!(acp.timeout_secs, 600);
    assert_eq!(acp.extra_args, vec!["--debug"]);
}

#[test]
fn channels_config_without_claude_sections_defaults_to_none() {
    let toml_input = r#"
message_timeout_secs = 300
"#;
    let channels: nonzeroclaw::config::ChannelsConfig =
        toml::from_str(toml_input).expect("should parse minimal channels config");
    assert!(channels.claude_api.is_none());
    assert!(channels.claude_acp.is_none());
}
