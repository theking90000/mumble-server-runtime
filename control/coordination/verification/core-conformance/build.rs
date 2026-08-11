fn main() -> Result<(), Box<dyn std::error::Error>> {
    let core_include = std::path::PathBuf::from("../../protocol/src/main/proto");
    let core_protocol = core_include.join("mumble/controller/core/v1/controller.proto");
    let spaces_include =
        std::path::PathBuf::from("../../../../implementations/spaces/protocol/src/main/proto");
    let spaces_protocol = spaces_include.join("mumble/controller/spaces/v1/spaces.proto");
    let protoc = protoc_bin_vendored::protoc_bin_path()?;

    println!("cargo:rerun-if-changed={}", core_protocol.display());
    println!("cargo:rerun-if-changed={}", spaces_protocol.display());
    let mut prost = prost_build::Config::new();
    prost.protoc_executable(protoc);
    tonic_prost_build::configure()
        .build_server(false)
        // The RPC itself is named `connect`, so the generated endpoint shortcut
        // would collide with it. The verifier constructs a Channel explicitly.
        .build_transport(false)
        .compile_with_config(
            prost,
            &[core_protocol, spaces_protocol],
            &[core_include, spaces_include],
        )?;
    Ok(())
}
