#![cfg(windows)]
#![allow(dead_code, unused_imports)] // Shared fixture also serves Linux service tests.
mod support;

use axum::{
    Router, body::Body, extract::State, http::StatusCode, response::Response, routing::post,
};
use std::io::{BufRead as _, Read as _, Write as _};
use std::process::{Command, Output};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Notify;

include!("service_host_cli/fixture.rs");
#[path = "service_host_cli/acp.rs"]
mod acp;
