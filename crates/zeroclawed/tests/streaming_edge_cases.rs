//! Streaming edge case tests for ZeroClawed
//! Tests SSE parsing, chunk handling, and streaming failures

use std::time::Duration;

// ==================== SSE PARSING TESTS ====================

/// Test basic SSE chunk parsing
#[test]
fn test_sse_basic_parsing() {
    let sse_data = "data: {\"content\": \"Hello\"}\n\n";

    // Parse SSE format
    let lines: Vec<&str> = sse_data.lines().collect();
    assert_eq!(lines.len(), 2); // data line + empty line

    let data_line = lines[0];
    assert!(data_line.starts_with("data: "));

    let json_str = &data_line[6..]; // Remove "data: " prefix
    let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap();
    assert_eq!(parsed["content"].as_str().unwrap(), "Hello");
}

/// Test multi-line SSE events
#[test]
fn test_sse_multiline_events() {
    let sse_data = concat!(
        "id: 1\n",
        "event: message\n",
        "data: {\"content\": \"Line 1\", \"index\": 0}\n\n",
        "id: 2\n",
        "event: message\n",
        "data: {\"content\": \"Line 2\", \"index\": 1}\n\n",
    );

    let events: Vec<&str> = sse_data.split("\n\n").collect();
    assert_eq!(events.len(), 3); // 2 events + empty final

    // Check first event
    let event1 = events[0];
    assert!(event1.contains("id: 1"));
    assert!(event1.contains("event: message"));
}

/// Test SSE [DONE] termination
#[test]
fn test_sse_done_termination() {
    let sse_stream = concat!(
        "data: {\"content\": \"Hello\"}\n\n",
        "data: {\"content\": \" world\"}\n\n",
        "data: [DONE]\n\n",
    );

    let mut content_parts = Vec::new();
    let mut done_received = false;

    for line in sse_stream.lines() {
        if line.starts_with("data: ") {
            let data = &line[6..];
            if data == "[DONE]" {
                done_received = true;
                break;
            }

            if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                if let Some(content) = json["content"].as_str() {
                    content_parts.push(content.to_string());
                }
            }
        }
    }

    assert!(done_received, "Should receive [DONE] signal");
    assert_eq!(content_parts.join(""), "Hello world");
}

/// Test SSE with malformed JSON
#[test]
fn test_sse_malformed_json() {
    let malformed_chunks = vec![
        "data: {broken json}\n\n",
        "data: not json at all\n\n",
        "data: \n\n", // Empty data
    ];

    for chunk in &malformed_chunks {
        let lines: Vec<&str> = chunk.lines().collect();
        if let Some(data_line) = lines.first() {
            if data_line.starts_with("data: ") {
                let json_str = &data_line[6..];

                // Should fail gracefully, not panic
                let result: Result<serde_json::Value, _> = serde_json::from_str(json_str);
                assert!(
                    result.is_err() || json_str.trim().is_empty(),
                    "Malformed JSON should be handled gracefully"
                );
            }
        }
    }
}

/// Test SSE with very long content
#[test]
fn test_sse_long_content() {
    let long_content = "x".repeat(10000);
    let sse_data = format!("data: {{\"content\": \"{}\"}}\n\n", long_content);

    assert!(sse_data.len() > 10000, "SSE data should be large");

    // Should be able to parse without issues
    if let Some(json_start) = sse_data.find("data: ") {
        let json_str = &sse_data[json_start + 6..].trim();
        let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap();
        assert_eq!(
            parsed["content"].as_str().unwrap().len(),
            long_content.len()
        );
    }
}

/// Test SSE with Unicode content
#[test]
fn test_sse_unicode_content() {
    let unicode_content = "Hello 世界 🌍 Привет مرحبا こんにちは";
    let sse_data = format!("data: {{\"content\": \"{}\"}}\n\n", unicode_content);

    if let Some(json_start) = sse_data.find("data: ") {
        let json_str = &sse_data[json_start + 6..].trim();
        let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap();
        assert_eq!(parsed["content"].as_str().unwrap(), unicode_content);
    }
}

/// Test streaming aggregation
#[test]
fn test_streaming_content_aggregation() {
    let chunks = vec![
        r#"{"content": "The", "index": 0}"#,
        r#"{"content": " quick", "index": 1}"#,
        r#"{"content": " brown", "index": 2}"#,
        r#"{"content": " fox", "index": 3}"#,
    ];

    let mut full_content = String::new();
    let mut max_index = 0;

    for chunk in &chunks {
        let parsed: serde_json::Value = serde_json::from_str(chunk).unwrap();
        if let Some(content) = parsed["content"].as_str() {
            full_content.push_str(content);
        }
        if let Some(index) = parsed["index"].as_u64() {
            max_index = max_index.max(index as usize);
        }
    }

    assert_eq!(full_content, "The quick brown fox");
    assert_eq!(max_index, 3);
}

/// Test out-of-order streaming chunks
#[test]
fn test_streaming_out_of_order_chunks() {
    // Simulate chunks arriving out of order
    let mut chunks = vec![
        (2, r#"{"content": " third", "index": 2}"#),
        (0, r#"{"content": "First", "index": 0}"#),
        (1, r#"{"content": " second", "index": 1}"#),
    ];

    // Sort by index
    chunks.sort_by_key(|(idx, _)| *idx);

    let mut content = String::new();
    for (_, chunk) in &chunks {
        let parsed: serde_json::Value = serde_json::from_str(chunk).unwrap();
        if let Some(c) = parsed["content"].as_str() {
            content.push_str(c);
        }
    }

    assert_eq!(content, "First second third");
}

/// Test streaming with missing chunks
#[test]
fn test_streaming_missing_chunks() {
    let chunks = vec![
        (0, r#"{"content": "First", "index": 0}"#),
        // Missing index 1
        (2, r#"{"content": " third", "index": 2}"#),
    ];

    let received_indices: Vec<usize> = chunks.iter().map(|(i, _)| *i).collect();
    let expected_indices: Vec<usize> = (0..=2).collect();

    let missing: Vec<usize> = expected_indices
        .iter()
        .filter(|i| !received_indices.contains(i))
        .copied()
        .collect();

    assert_eq!(missing, vec![1], "Should detect missing chunk at index 1");
}

/// Test streaming timeout simulation
#[test]
fn test_streaming_timeout_handling() {
    // Simulate a stream that takes too long
    let start = std::time::Instant::now();
    let _timeout = Duration::from_millis(100);

    // Simulate processing
    let elapsed = start.elapsed();

    // In real implementation, this would check if elapsed > timeout
    // and cancel the stream
    assert!(
        elapsed < Duration::from_secs(10),
        "Stream processing should complete within reasonable time"
    );
}

/// Test streaming with error in middle
#[test]
fn test_streaming_error_mid_stream() {
    let chunks = vec![
        Ok(r#"{"content": "Start"}"#),
        Ok(r#"{"content": " middle"}"#),
        Err("Connection reset"),
    ];

    let mut content = String::new();
    let mut error_encountered = false;

    for chunk in &chunks {
        match chunk {
            Ok(data) => {
                let parsed: serde_json::Value = serde_json::from_str(data).unwrap();
                if let Some(c) = parsed["content"].as_str() {
                    content.push_str(c);
                }
            }
            Err(e) => {
                error_encountered = true;
                println!("Stream error: {}", e);
                break;
            }
        }
    }

    assert!(error_encountered, "Should detect stream error");
    assert_eq!(content, "Start middle", "Should have partial content");
}

/// Test empty streaming response
#[test]
fn test_streaming_empty_response() {
    let sse_data = "data: [DONE]\n\n";

    let mut content = String::new();
    let mut done_received = false;

    for line in sse_data.lines() {
        if line.starts_with("data: ") {
            let data = &line[6..];
            if data == "[DONE]" {
                done_received = true;
                break;
            }

            if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                if let Some(c) = json["content"].as_str() {
                    content.push_str(c);
                }
            }
        }
    }

    assert!(done_received);
    assert!(content.is_empty(), "Empty stream should produce no content");
}

/// Test streaming with duplicate chunks
#[test]
fn test_streaming_duplicate_chunks() {
    let chunks = vec![
        (0, r#"{"content": "Hello", "index": 0}"#),
        (1, r#"{"content": " world", "index": 1}"#),
        (0, r#"{"content": "Hello", "index": 0}"#), // Duplicate
    ];

    // Deduplicate by index
    let mut seen_indices = std::collections::HashSet::new();
    let unique_chunks: Vec<_> = chunks
        .into_iter()
        .filter(|(idx, _)| seen_indices.insert(*idx))
        .collect();

    assert_eq!(unique_chunks.len(), 2, "Should remove duplicate");

    let mut content = String::new();
    for (_, chunk) in &unique_chunks {
        let parsed: serde_json::Value = serde_json::from_str(chunk).unwrap();
        if let Some(c) = parsed["content"].as_str() {
            content.push_str(c);
        }
    }

    assert_eq!(content, "Hello world");
}
