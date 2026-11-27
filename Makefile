.PHONY: catalog test test-typecat doccheck loadtest dev

catalog:
	cd tools/typecat && npm ci --no-audit --no-fund >/dev/null
	node tools/typecat/typecat.mjs types --out catalog.json

test-typecat:
	cd tools/typecat && npm test

doccheck:
	node tools/doccheck.mjs

test: catalog test-typecat doccheck
	cargo test --workspace

loadtest: catalog
	cargo run --release -p i3cm-loadtest -- --embedded

dev: catalog
	cargo run -p i3cm-server -- dev
