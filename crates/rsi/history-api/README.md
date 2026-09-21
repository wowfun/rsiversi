# rsi-history-api

This wasm-safe contract exposes lexical history separately from navigation's
metadata search. Each request names a registered workspace and a native or
external conversation; the owner independently authorizes search, original reads
and reference capture against that workspace. A search hit is only a candidate.
Read and freeze reread the original through its source owner and reject a changed
Header, observed epoch, text digest, content coordinate or byte length.

Advance performs one finite indexing batch. Coverage reports the indexed cursor,
observed source horizon, omitted originals/fields, and whether more work remains.
Search never claims an unindexed conversation is complete. Rebuild discards only
this conversation's derived entries. Cursors bind the query, source and cache
generation; a rebuilt cache rejects them. Results contain at most 64 hits and
256 KiB, queries at most 256 bytes, and original reads at most 64 KiB of UTF-8.
Freeze binds the selected original to an independently authorized actual target
Session Header; references remain data, with no source execution authority.
