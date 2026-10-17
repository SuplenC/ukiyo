//! ukiyo-core: the platform-neutral half of ukiyo.
//!
//! - [`scene`]: the `scene.toml` format.
//! - [`expr`] and [`smooth`]: the binding language and its smoothing.
//! - [`signals`]: the signal bus that sources write into.
//! - [`config`] and [`settings`]: the layered user configuration and its schema.
//! - [`engine`]: a scene, its bindings and clock, producing frames of uniforms.
//! - [`render`]: the wgpu renderer for those frames.
//! - [`ipc`]: the daemon's control protocol.
//! - [`catalog`]: where scenes are installed.

pub mod catalog;
pub mod config;
pub mod engine;
pub mod expr;
pub mod ipc;
pub mod render;
pub mod scene;
pub mod settings;
pub mod signals;
pub mod smooth;

pub use engine::{Engine, Frame};
pub use scene::Scene;
