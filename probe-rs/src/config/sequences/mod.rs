//! Target-specific debug sequence implementations.

// Common code for certain chip families
mod esp;
mod nrf;

// RISC-V ESP32 devices
pub mod esp32c2;
pub mod esp32c3;
pub mod esp32c6;
pub mod esp32h2;

// Xtensa ESP32 devices
pub mod esp32;
pub mod esp32s2;
pub mod esp32s3;

// ARM devices
pub mod atsam;
pub mod cc13xx_cc26xx;
pub mod efm32xg2;
pub mod infineon;
pub mod nrf52;
pub mod nrf53;
pub mod nrf91;
pub mod nxp_armv7m;
pub mod nxp_armv8m;
pub mod stm32_armv6;
pub mod stm32_armv7;
pub mod stm32h7;
