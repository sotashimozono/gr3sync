//! A stand-in for the camera, for tests and for the container image.
//!
//! **Read [`gatt`]'s module docs before trusting a green test against this.**
//! The emulator is built from the same reverse-engineered specification as
//! gr3sync itself, so by default it agrees with gr3sync's assumptions whether
//! or not the real camera does. It verifies the transport chain and guards
//! against regressions; it cannot tell you the spec is right.
//!
//! Behind the `emulator` feature so none of it ships in the release binary.

pub mod gatt;
pub mod http;

pub use gatt::{GattTable, Provenance};
pub use http::{Card, HttpCamera};
