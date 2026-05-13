//! Channel adapters for Calciforge.
//!
//! Active: Telegram, Matrix, WhatsApp, Signal, text/iMessage, and mock.
//!
//! Matrix and Telegram use their native HTTP APIs. WhatsApp and Signal embed
//! zeroclawlabs transports directly. Text/iMessage uses the zeroclawlabs Linq
//! transport for outbound sends plus a Calciforge-hosted Linq webhook receiver
//! for inbound iMessage/RCS/SMS events.

pub mod matrix;
#[cfg(feature = "channel-matrix-e2ee")]
pub mod matrix_e2ee;
pub mod mock;
pub mod runtime;
pub mod signal;
pub mod sms;
pub mod telegram;
mod telegram_progress;
pub mod telemetry;
pub mod whatsapp;
