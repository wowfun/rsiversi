# pulldown-cmark

The workspace uses the library sources from [pulldown-cmark 0.13.6](https://github.com/pulldown-cmark/pulldown-cmark/tree/014b61d38d235f382935bb676bab872b7680afa2/pulldown-cmark). The upstream [license](LICENSE) applies.

The optional conversation syntax adapts the [DSH Markdown rules](https://github.com/deepseek-ai/deepseek-harness/tree/2e39e70496aa02986ca6a820c5d497e816b69d7e/packages/client/ui-primitives/src/markdown) under their [MIT license](LICENSE-DSH). Opt-in parser flags extend the existing delimiter scan and destination decoding at the point where the original source is available. The [GUI contract](../../crates/rsi/gui/README.md) specifies their behavior.

The default library forbids unsafe code. The optional x86-64 SIMD scanner
dispatches to SSSE3 after runtime CPU detection and reads complete 16-byte
blocks. Its mask positions refer to bytes within the same input slice.
