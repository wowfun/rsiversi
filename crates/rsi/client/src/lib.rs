//! Shared application control logic over domain capabilities and explicit execution.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod controller;
mod lifetime;
mod message;
mod observation;
mod read;
mod submission;
pub use controller::{SessionController, SessionControllerContract, SessionControllerFactory};
pub use lifetime::{ConnectionLifetime, ConnectionLifetimeContract};
pub use message::{MessageEvent, MessageRunError, MessageSink, drive_message};
pub use observation::ObservationSinkContract;
pub use observation::{
    ObservationFailure, ObservationKind, ObservationSink, observe_interactions, observe_session,
};
pub use read::read_with_capacity_retry;
pub use submission::submit_with_reconciliation;
