fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Set PROTOC to use bundled protobuf compiler
    std::env::set_var("PROTOC", protobuf_src::protoc());

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/adsb.proto"], &["proto/"])?;
    Ok(())
}
