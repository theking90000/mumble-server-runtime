fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protocol = "../contract/src/main/proto/mumble/controller/spaces/v1/spaces.proto";
    let include = "../contract/src/main/proto";
    let protoc = protoc_bin_vendored::protoc_bin_path()?;

    println!("cargo:rerun-if-changed={protocol}");
    let mut prost = prost_build::Config::new();
    prost.protoc_executable(protoc);
    prost.compile_protos(&[protocol], &[include])?;
    Ok(())
}
