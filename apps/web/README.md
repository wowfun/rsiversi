# Web document bridge

The Web package supplies the shared RSIversi browser and Desktop document.
The [runtime contract](docs/runtime.md) owns the Worker connection, frame
acknowledgement and teardown. [Presentation](docs/presentation.md) owns layout,
interaction and the graphical brand; [renderer hosting](docs/renderers.md)
owns independently published extensions and their document integration.
[Document persistence](docs/persistence.md) owns saved input and device-local
preferences, with separate storage and failure boundaries.

Build with `node apps/devtools/build.mjs web ABSOLUTE_PACKAGE [--debug]`,
then install using `rsi plugin install --local ABSOLUTE_PACKAGE` and launch with
`rsi web`. The producer builds the document/Worker/WASM package independently
from the native core. The [development guide](../../docs/development.md) explains
ordinary `--watch` recipes and Vite supervision.
Dependency installation keeps lifecycle scripts disabled. The [pnpm policy](pnpm-workspace.yaml)
explicitly denies esbuild's install script; its locked platform package supplies
the executable. This also keeps repeated installs consistent with pnpm's recorded
build decisions.

The document test command verifies every adapted DSH file against its recorded
SHA-256, including the retained license. CI runs both this command and TypeScript
checking before the product browser fixture. With `RSI_DSH_REFERENCE` pointing to
the pinned checkout, it also verifies each recorded upstream source hash; CI
performs this comparison after acquiring that exact revision.

The document Node tests require Node 22.18 or later for direct TypeScript imports.
