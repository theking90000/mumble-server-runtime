//! Public protocol surface used by the independent Controller conformance scenarios.
#![forbid(unsafe_code)]

pub mod protocol {
    tonic::include_proto!("mumble.controller.v1");
}
