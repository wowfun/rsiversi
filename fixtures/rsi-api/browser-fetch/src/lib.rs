use rsi_api_protocol::*;

pub const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const MALFORMED_FINITE: &[&str] = &[
    "json",
    "truncated",
    "epoch",
    "endpoint",
    "type",
    "encoding",
    "length",
    "redirect",
    "binary",
    "header",
    "head-loss",
];
pub const MALFORMED_STREAMS: &[&str] = &[
    "missing-end",
    "trailing",
    "opening",
    "domain-without-end",
    "invalid-item",
];

pub fn malformed_specs() -> Vec<OperationSpec> {
    let mut operations = vec![describe_operation(), operations_operation()];
    for name in MALFORMED_FINITE {
        for (prefix, effect) in [
            ("read", OperationEffect::Read),
            ("mutate", OperationEffect::Mutation),
        ] {
            operations.push(OperationSpec {
                id: OperationId::new("fault", format!("{prefix}-{name}"), 1).unwrap(),
                class: OperationClass::Data,
                effect,
                access: rsi_api_protocol::OperationAccess::Authenticated,
                encoding: RequestEncoding::Json,
                maximum_request_bytes: 128,
                maximum_response_bytes: 128,
            });
        }
    }
    for name in MALFORMED_STREAMS {
        operations.push(OperationSpec {
            id: OperationId::new("fault", name.to_string(), 1).unwrap(),
            class: OperationClass::Subscription,
            effect: OperationEffect::Read,
            access: rsi_api_protocol::OperationAccess::Authenticated,
            encoding: RequestEncoding::Json,
            maximum_request_bytes: 128,
            maximum_response_bytes: 128,
        });
    }
    operations
}
pub const NAMES: &[&str] = &[
    "binary",
    "events",
    "idle",
    "mutate",
    "read-gate",
    "release",
    "stats",
    "reject",
    "domain",
    "pool-barrier",
];
pub fn spec(name: &str) -> OperationSpec {
    assert!(NAMES.contains(&name));
    OperationSpec {
        id: OperationId::new("fetch", name, 1).unwrap(),
        class: match name {
            "events" | "idle" => OperationClass::Subscription,
            "release" | "stats" | "pool-barrier" => OperationClass::Control,
            _ => OperationClass::Data,
        },
        effect: match name {
            "mutate" | "release" | "reject" | "domain" => OperationEffect::Mutation,
            _ => OperationEffect::Read,
        },
        access: rsi_api_protocol::OperationAccess::Authenticated,
        encoding: if name == "binary" {
            RequestEncoding::Binary
        } else {
            RequestEncoding::Json
        },
        maximum_request_bytes: if name == "binary" { 512 * 1024 } else { 128 },
        maximum_response_bytes: match name {
            "binary" => 512 * 1024 + 128,
            "events" | "idle" => 36 * 1024 * 1024,
            _ => 128,
        },
    }
}

#[cfg(target_arch = "wasm32")]
mod malformed;
#[cfg(target_arch = "wasm32")]
mod probe;
#[cfg(target_arch = "wasm32")]
pub use malformed::run_malformed_probe;
#[cfg(target_arch = "wasm32")]
pub use probe::{run_pool_probe, run_probe, run_shared_pool_probe};
