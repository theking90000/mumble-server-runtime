//! Public protocol surface used by the independent Controller conformance scenarios.
#![forbid(unsafe_code)]

pub mod protocol {
    tonic::include_proto!("mumble.controller.core.v1");
}

pub mod spaces_protocol {
    tonic::include_proto!("mumble.controller.spaces.v1");
}
