//! Channel adapters for Calciforge.
//!
//! Currently active: Telegram.
//! Scaffolded (needs bot account): Matrix.
//! Scaffolded (needs ZeroClaw WA session): WhatsApp.
//! Scaffolded (needs OpenClaw Signal session): Signal.
//!
//! Matrix was removed in v0.4.x (Zig) due to a tight-loop bug. The Rust v2 doesn't
//! have that problem — the adapter below is ready to wire up once the bot account exists.
//! See MATRIX-SETUP-NEEDED.md in the repo root for what's required.
//!
//! WhatsApp runs as a webhook receiver sidecar to ZeroClaw's wa-rs session.
//! Calciforge listens for incoming webhook POSTs (forwarded from ZeroClaw) and sends
//! replies back via ZeroClaw's /tools/invoke API.  The QR pairing happens in ZeroClaw;
//! Calciforge only handles identity routing and agent dispatch.
//!
//! Signal follows the same webhook receiver pattern as WhatsApp, but uses
//! OpenClaw's native Signal support. Calciforge receives webhooks from OpenClaw
//! and sends replies via the /tools/invoke API.

pub mod matrix;
pub mod mock;
pub mod signal;
pub mod telegram;
pub mod telemetry;
pub mod whatsapp;
