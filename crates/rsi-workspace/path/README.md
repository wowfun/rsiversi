# rsi-workspace-path

This stateless library checks UTF-8 host paths carried in protocols and durable
records. It accepts absolute POSIX paths, Windows drive paths, UNC shares and
verbatim drive/UNC paths independently of the reader's target. Paths have at
most 16 KiB, contain no NUL and no parent components. Normalized-path checking
also rejects empty or dot components and trailing separators outside roots.

The library neither accesses a filesystem nor grants authority. Native providers
must still canonicalize and verify the actual paths used for effects. Browser
decoding must not interpret another host's path using the browser target's
unsupported filesystem implementation.

Typed native paths may contain non-UTF-8 bytes. The Path helpers preserve those
values and apply their native component rules; serialization still requires
UTF-8. That local process contract is separate from decoding a foreign wire path.
