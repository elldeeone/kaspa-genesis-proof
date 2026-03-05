fn main() {
    let proto_file = "proto/dbobjects.proto";
    println!("cargo:rerun-if-changed={proto_file}");

    let protoc = protoc_bin_vendored::protoc_bin_path().expect("failed to find vendored protoc");
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    prost_build::Config::new()
        .compile_protos(&[proto_file], &["proto"])
        .expect("failed to compile protobuf files");
}
