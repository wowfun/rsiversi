//! Shared native and Worker GUI application plugins.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod application;
mod catalog;
mod details;
mod frames;
mod markdown;
mod panes;
mod projection;
mod renderer;
mod surface_id;
pub use surface_id::SurfaceId;

pub use application::{
    GuiApplication, GuiApplicationContract, GuiApplicationFactory, display_error,
};
/// Maximum source bytes admitted by an image import before platform copying.
pub const MAXIMUM_UPLOAD_BYTES: usize = panes::images::MAXIMUM_UPLOAD_BYTES;
