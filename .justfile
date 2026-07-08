debug:
    cargo test --features test -- --nocapture
test:
    cargo test --features test 
build:
    cargo build --release

clean:
    cargo clean
    git submodule deinit -f secp256k1_spscan-sys/depend/secp256k1

submodule:
    git submodule sync secp256k1_spscan-sys/depend/secp256k1
    git submodule update --init secp256k1_spscan-sys/depend/secp256k1
