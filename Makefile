.PHONY: catalog test test-typecat loadtest dev

catalog:
	cd tools/typecat && npm ci --no-audit --no-fund >/dev/null
	node tools/typecat/typecat.mjs types --out catalog.json

test-typecat:
	cd tools/typecat && npm test

test: catalog test-typecat
	cargo test --workspace

loadtest: catalog
	cargo run --release -p base-loadtest -- --embedded

dev: catalog
	cargo run -p base-server -- dev
