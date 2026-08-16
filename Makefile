# Builds the release wasm32 module manifest/cpldicy.toml points at.
# Needs the wasm32-unknown-unknown rustup target:
#   rustup target add wasm32-unknown-unknown

all:
	cargo build --release --locked --target wasm32-unknown-unknown

test:
	cargo test

conformance: all
	cd tests/copperline && ./run.sh

thermal-probe: all
	cd tests/copperline && ./run-thermal.sh

fetch-oracle:
	./vendor/fetch-oracle.sh

oracle: all fetch-oracle
	cd tests/copperline && ./run-oracle.sh

clean:
	cargo clean

.PHONY: all test conformance thermal-probe fetch-oracle oracle clean
