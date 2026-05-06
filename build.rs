fn main() {
    use std::path::PathBuf;

    let proto_file = "proto/dbobjects.proto";
    println!("cargo:rerun-if-changed={proto_file}");
    let rpc_proto_file = "proto/kaspa-rpc/messages.proto";
    let rpc_proto_dir = "proto/kaspa-rpc";
    let p2p_proto_file = "proto/kaspa-p2p/messages.proto";
    let p2p_proto_dir = "proto/kaspa-p2p";
    println!("cargo:rerun-if-changed={rpc_proto_file}");
    println!("cargo:rerun-if-changed={rpc_proto_dir}/rpc.proto");
    println!("cargo:rerun-if-changed={p2p_proto_file}");
    println!("cargo:rerun-if-changed={p2p_proto_dir}/p2p.proto");

    let protoc = protoc_bin_vendored::protoc_bin_path().expect("failed to find vendored protoc");
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    prost_build::Config::new()
        .compile_protos(&[proto_file], &["proto"])
        .expect("failed to compile protobuf files");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set"));
    let rpc_out_dir = out_dir.join("rpcwire");
    let p2p_out_dir = out_dir.join("p2pwire");
    std::fs::create_dir_all(&rpc_out_dir).expect("failed creating rpc proto out dir");
    std::fs::create_dir_all(&p2p_out_dir).expect("failed creating p2p proto out dir");

    tonic_build::configure()
        .build_server(false)
        .out_dir(&rpc_out_dir)
        .compile_protos(&[rpc_proto_file], &[rpc_proto_dir])
        .expect("failed to compile Kaspa RPC protobuf files");

    tonic_build::configure()
        .build_server(false)
        .out_dir(&p2p_out_dir)
        .compile_protos(&[p2p_proto_file], &[p2p_proto_dir])
        .expect("failed to compile Kaspa P2P protobuf files");
}
