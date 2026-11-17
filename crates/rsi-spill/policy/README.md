# rsi-spill-policy

Retention never expands a legal result's text bytes. If all text fits, there is
no omission marker; a replacement that would not shorten the original is declined
before writing another cache object.

The optional Agent around-contribution owns text-only retention for the
[Spill family](../README.md). Its `max_inline_tokens` parameter has no implicit
default: omission registers no contribution. The standard product supplies
12,500. Canonical values, images and mixed content remain unchanged.
When content is empty, Context renders the compact JSON canonical value as text;
the policy applies the same budget to that fallback without changing the value.
Full text concatenates text blocks in their original order without inserting bytes.

Text cost is four framing tokens plus ceiling(Unicode code points / four).
The policy preserves ordered head and tail around an omission marker and reserves
space for the complete retrieval notice before choosing either slice. If the
notice alone cannot fit, the original legal result is retained. Saving is
best-effort and precedes replacement; `output_read` is excluded from retention.
Existing references are preserved but cannot authorize dropping more text: the
policy saves the complete rendered text before further shortening. The policy
budgets the retrieval notices but leaves them in typed `spills`; Context renders
them after pruning, separately from retained output text.
