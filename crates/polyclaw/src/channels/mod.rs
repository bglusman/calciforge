//! Channel adapters for PolyClaw v2.
//!
//! Currently active: Telegram.
//! Scaffolded (needs bot account): Matrix.
//! Scaffolded (needs NZC WA session): WhatsApp.
//! Scaffolded (needs OpenClaw Signal session): Signal.
//!
//! Matrix was removed in v0.4.x (Zig) due to a tight-loop bug. The Rust v2 doesn't
//! have that problem — the adapter below is ready to wire up once the bot account exists.
//! See MATRIX-SETUP-NEEDED.md in the repo root for what's required.
//!
//! WhatsApp runs as a webhook receiver sidecar to NonZeroClaw's wa-rs session.
//! PolyClaw listens for incoming webhook POSTs (forwarded from NZC) and sends
//! replies back via NZC's /tools/invoke API.  The QR pairing happens in NZC;
//! PolyClaw only handles identity routing and agent dispatch.
//!
//! Signal follows the same webhook receiver pattern as WhatsApp, but uses
//! OpenClaw's native Signal support. PolyClaw receives webhooks from OpenClaw
//! and sends replies via the /tools/invoke API.

pub mod signal;
pub mod telegram;
pub mod matrix;
pub mod whatsapp;
