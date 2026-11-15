set default-list

fmt:
    cargo fmt --all -- --check

# The browser glue in camel-web compiles only for wasm32, so it is linted and
# documented for that target as well.
clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo clippy -p camel-web --target wasm32-unknown-unknown --locked -- -D warnings

test:
    cargo test --workspace --locked

[env("RUSTDOCFLAGS", "-D warnings")]
doc:
    cargo doc --workspace --no-deps --locked
    cargo doc -p camel-web --target wasm32-unknown-unknown --no-deps --locked

# Build the WebAssembly core and its JS bindings into web/src/wasm.
# SIMD comes from .cargo/config.toml; text recognition needs it.
wasm:
    cargo build -p camel-web --target wasm32-unknown-unknown --profile web --locked
    wasm-bindgen --target web --out-dir web/src/wasm target/wasm32-unknown-unknown/web/camel_web.wasm
    wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals --enable-simd web/src/wasm/camel_web_bg.wasm -o web/src/wasm/camel_web_bg.wasm

# Build the static workbench into web/dist.
web: wasm
    cd web && npm ci --ignore-scripts --no-audit --no-fund && npm run build && npm test

# Serve the workbench on http://localhost:5173.
web-dev: web
    cd web && npm run dev

check: fmt clippy test doc

build:
    cargo build --release --locked

dist:
    cargo build --profile dist --locked

install:
    cargo install --path crates/camel-cli --profile dist --locked

evaluate *args:
    cargo run -p camel-vision --profile dist --example evaluate --locked -- {{args}}
