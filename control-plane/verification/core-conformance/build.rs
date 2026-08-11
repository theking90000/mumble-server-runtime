fn main() -> Result<(), Box<dyn std::error::Error>> {
    let core_protocol =
        "../../core/contract/src/main/proto/mumble/controller/core/v1/controller.proto";
    let spaces_protocol = "../../implementations/spaces/contract/src/main/proto/mumble/controller/spaces/v1/spaces.proto";
    let core_include = "../../core/contract/src/main/proto";
    let spaces_include = "../../implementations/spaces/contract/src/main/proto";
    let protoc = protoc_bin_vendored::protoc_bin_path()?;

    println!("cargo:rerun-if-changed={core_protocol}");
    println!("cargo:rerun-if-changed={spaces_protocol}");
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
