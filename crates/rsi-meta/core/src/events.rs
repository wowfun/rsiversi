mod handler;
mod model;
mod target;

pub use handler::EventHandler;
pub use model::{DispatchMode, EventOptions, EventOutcome, EventReceipt};
pub use target::{EventTarget, ListenerView};
