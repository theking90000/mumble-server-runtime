fn main() -> Result<(), Box<dyn std::error::Error>> {
    let core_protocol =
        "../core/contract/src/main/proto/mumble/controller/core/v1/controller.proto";
    let spaces_protocol = "../implementations/spaces/contract/src/main/proto/mumble/controller/spaces/v1/spaces.proto";
    let legacy_protocol = "../contract/src/main/proto/mumble/controller/v1/controller.proto";
    let core_include = "../core/contract/src/main/proto";
    let spaces_include = "../implementations/spaces/contract/src/main/proto";
    let legacy_include = "../contract/src/main/proto";
    let protoc = protoc_bin_vendored::protoc_bin_path()?;

    println!("cargo:rerun-if-changed={core_protocol}");
    println!("cargo:rerun-if-changed={spaces_protocol}");
    println!("cargo:rerun-if-changed={legacy_protocol}");
    let mut prost = prost_build::Config::new();
    prost.protoc_executable(protoc);
    // The generated tonic client would have both its endpoint constructor and this
    // protocol's RPC named `connect`. The Java SDK is the v1 client, so this crate
    // deliberately generates only the Rust server surface.
    tonic_prost_build::configure()
        .build_client(false)
        .compile_with_config(
            prost,
            &[core_protocol, spaces_protocol, legacy_protocol],
            &[core_include, spaces_include, legacy_include],
        )?;
    Ok(())
}
