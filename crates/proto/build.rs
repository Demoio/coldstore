fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_files = &[
        "proto/common.proto",
        "proto/metadata.proto",
        "proto/scheduler.proto",
        "proto/cache.proto",
        "proto/tape.proto",
    ];

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(proto_files, &["proto/"])?;

    for file in proto_files {
        println!("cargo:rerun-if-changed={file}");
    }

    Ok(())
}
