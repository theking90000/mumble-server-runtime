fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protocol = "../contract/src/main/proto/mumble/controller/v1/controller.proto";
    let include = "../contract/src/main/proto";
    let protoc = protoc_bin_vendored::protoc_bin_path()?;

    println!("cargo:rerun-if-changed={protocol}");
    let mut prost = prost_build::Config::new();
    prost.protoc_executable(protoc);
    // The generated tonic client would have both its endpoint constructor and this
    // protocol's RPC named `connect`. The Java SDK is the v1 client, so this crate
    // deliberately generates only the Rust server surface.
    tonic_prost_build::configure()
        .build_client(false)
        .compile_with_config(prost, &[protocol], &[include])?;
    Ok(())
}
