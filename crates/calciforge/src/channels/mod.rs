//! Channel adapters for Calciforge.
//!
//! Active: Telegram, Matrix, WhatsApp, Signal, text/iMessage, and mock.
//!
//! Matrix and Telegram use their native HTTP APIs. WhatsApp and Signal embed
//! zeroclawlabs transports directly. Text/iMessage uses the zeroclawlabs Linq
//! transport for outbound sends plus a Calciforge-hosted Linq webhook receiver
//! for inbound iMessage/RCS/SMS events.

pub mod matrix;
pub mod mock;
pub mod signal;
pub mod sms;
pub mod telegram;
pub mod telemetry;
pub mod whatsapp;
