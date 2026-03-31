//! Stub bindings for the Total Phase Aardvark I2C/SPI/GPIO USB adapter SDK.
//!
//! The real Aardvark SDK requires the proprietary Total Phase C library.
//! This stub provides the same public API surface so the crate compiles
//! without the SDK installed. All methods return `Err("aardvark SDK not available")`.
//!
//! To use real hardware: replace this crate with a binding to the actual
//! `aardvark.so` / `aardvark.dll` library from Total Phase.

use std::fmt;

/// Error type for Aardvark operations.
#[derive(Debug)]
pub struct AardvarkError(pub String);

impl fmt::Display for AardvarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AardvarkError: {}", self.0)
    }
}

impl std::error::Error for AardvarkError {}

/// A handle to an open Aardvark USB adapter.
pub struct AardvarkHandle {
    port: i32,
}

impl AardvarkHandle {
    /// Return list of connected Aardvark port indices.
    /// Stub: always returns empty (no adapters found).
    pub fn find_devices() -> Vec<i32> {
        vec![]
    }

    /// Open the adapter at the given port index.
    /// Stub: always fails.
    pub fn open_port(port: i32) -> Result<Self, AardvarkError> {
        Err(AardvarkError(format!(
            "Aardvark SDK not available; cannot open port {port}"
        )))
    }

    /// Enable I2C mode with the given bitrate in kHz.
    pub fn i2c_enable(&self, _bitrate_khz: u32) -> Result<(), AardvarkError> {
        Err(AardvarkError("Aardvark SDK not available".into()))
    }

    /// Scan I2C bus and return list of found device addresses.
    pub fn i2c_scan(&self) -> Vec<u8> {
        vec![]
    }

    /// Read `len` bytes from I2C address `addr`.
    pub fn i2c_read(&self, _addr: u8, _len: usize) -> Result<Vec<u8>, AardvarkError> {
        Err(AardvarkError("Aardvark SDK not available".into()))
    }

    /// Write register byte then read `len` bytes from I2C address.
    pub fn i2c_write_read(
        &self,
        _addr: u8,
        _write: &[u8],
        _len: usize,
    ) -> Result<Vec<u8>, AardvarkError> {
        Err(AardvarkError("Aardvark SDK not available".into()))
    }

    /// Write bytes to I2C address `addr`.
    pub fn i2c_write(&self, _addr: u8, _bytes: &[u8]) -> Result<(), AardvarkError> {
        Err(AardvarkError("Aardvark SDK not available".into()))
    }

    /// Enable SPI mode with the given bitrate in kHz.
    pub fn spi_enable(&self, _bitrate_khz: u32) -> Result<(), AardvarkError> {
        Err(AardvarkError("Aardvark SDK not available".into()))
    }

    /// Perform a full-duplex SPI transfer.
    pub fn spi_transfer(&self, _data: &[u8]) -> Result<Vec<u8>, AardvarkError> {
        Err(AardvarkError("Aardvark SDK not available".into()))
    }

    /// Set GPIO direction and value.
    pub fn gpio_set(&self, _direction: u8, _value: u8) -> Result<(), AardvarkError> {
        Err(AardvarkError("Aardvark SDK not available".into()))
    }

    /// Read current GPIO value.
    pub fn gpio_get(&self) -> Result<u8, AardvarkError> {
        Err(AardvarkError("Aardvark SDK not available".into()))
    }
}

impl From<i32> for AardvarkHandle {
    fn from(port: i32) -> Self {
        Self { port }
    }
}

impl From<AardvarkHandle> for i32 {
    fn from(h: AardvarkHandle) -> Self {
        h.port
    }
}
