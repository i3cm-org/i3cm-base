# Vendored libraries

Copied unchanged from the npm packages named below; no build step (see `docs/developer/ui.md`).

| Directory | Package and version | Files taken | Licence |
|---|---|---|---|
| `fluentui-web-components-3.2.1` | `@fluentui/web-components@3.2.1` | `dist/web-components-all.min.js` (includes `@microsoft/fast-element`) | MIT |
| `fluentui-tokens-1.0.0-alpha.24` | `@fluentui/tokens@1.0.0-alpha.24` | `lib/**/*.js` (ES modules) | MIT |
| `marked-18.2.0` | `marked@18.2.0` | `lib/marked.esm.js` | MIT |
| `dompurify-3.4.17` | `dompurify@3.4.17` | `dist/purify.es.mjs` | Apache-2.0 or MPL-2.0 |

No fonts or icons from Fluent are shipped; the UI uses the system font stack of the theme tokens.
