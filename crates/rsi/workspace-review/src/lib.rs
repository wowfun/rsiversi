//! Source-authorized execution interval review.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
mod git;
mod owner;
mod plugin;
mod scratch_root;
pub use owner::WorkspaceReview;
pub use plugin::{WorkspaceReviewApiFactory, WorkspaceReviewContract, WorkspaceReviewFactory};

#[cfg(test)]
mod tests;
