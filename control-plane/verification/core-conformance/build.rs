fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protocol = "../../contract/src/main/proto/mumble/controller/v1/controller.proto";
    let include = "../../contract/src/main/proto";
    let protoc = protoc_bin_vendored::protoc_bin_path()?;

    println!("cargo:rerun-if-changed={protocol}");
    let mut prost = prost_build::Config::new();
    prost.protoc_executable(protoc);
    tonic_prost_build::configure()
        .build_server(false)
        // The RPC itself is named `connect`, so the generated endpoint shortcut
        // would collide with it. The verifier constructs a Channel explicitly.
        .build_transport(false)
        .compile_with_config(prost, &[protocol], &[include])?;
    Ok(())
}
