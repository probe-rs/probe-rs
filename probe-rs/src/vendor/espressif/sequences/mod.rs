//! Espressif debug sequences.

// Common code for ESP32 devices
mod esp;

// RISC-V ESP32 devices
pub mod esp32c2;
pub mod esp32c3;
pub mod esp32c6;
pub mod esp32h2;

// Xtensa ESP32 devices
pub mod esp32;
pub mod esp32s2;
pub mod esp32s3;
