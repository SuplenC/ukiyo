//! Signal sources. Each is optional and degrades to "not written" when its
//! input isn't there. The parsing and maths are platform-neutral and tested
//! here; the daemon wires them to its event loop.

pub mod audio;
pub mod cpu;
pub mod hyprland;
pub mod power;
pub mod typing;
