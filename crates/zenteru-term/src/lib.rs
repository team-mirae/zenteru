//! The platform-independent core of zenteru (everything but rendering and
//! windowing): escape-sequence parsing, the zenkaku-aware terminal grid,
//! pty spawning, key encoding and the plugin API server. No dependencies
//! beyond zenteru-text.

pub mod api;
pub mod base64;
pub mod cell;
pub mod input;
pub mod json;
pub mod parser;
pub mod pty;
pub mod session;
pub mod term;
