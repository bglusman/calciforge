//! Server-sent events (SSE) streaming support
//!
//! Handles streaming responses from providers and forwards them
//! to clients as OpenAI-compatible SSE chunks.

#![allow(dead_code)]

use futures_util::Stream;
use std::pin::Pin;

/// A stream of SSE chunks from a provider
pub type SseStream = Pin<Box<dyn Stream<Item = Result<axum::body::Bytes, std::convert::Infallible>> + Send>>;

/// Transform a provider's SSE stream into OpenAI-compatible format
pub fn transform_stream(
    _upstream: impl Stream<Item = Result<String, reqwest::Error>> + Send + 'static,
) -> SseStream {
    // TODO: Implement transformation of provider-specific SSE format
    // to OpenAI-compatible format
    Box::pin(futures_util::stream::empty())
}
