set default-list

fmt:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings

test:
    cargo test --workspace --locked

[env("RUSTDOCFLAGS", "-D warnings")]
doc:
    cargo doc --workspace --no-deps --locked

ui:
    cd crates/camel-cli/reader-ui && npm ci --ignore-scripts --no-audit --no-fund && npm run build && npm test

check: fmt clippy test doc

build:
    cargo build --release --locked

dist:
    cargo build --profile dist --locked

install:
    cargo install --path crates/camel-cli --profile dist --locked

evaluate *args:
    cargo run -p camel-vision --profile dist --example evaluate --locked -- {{args}}
