# rsi-api-local-transport

This native mechanics library admits same-user Windows named pipes before any
API bytes are sent or interpreted. It owns no product paths, API operations,
configuration or daemon lifecycle.

Names use the bounded local `\\.\pipe\rsi-` namespace. The first server instance
claims its name exclusively, applies a protected TokenUser-only DACL at creation,
and rejects remote clients. An accepting listener creates its next instance
before releasing the previous one, retaining name ownership throughout serving.
If next-instance creation fails, the current instance disconnects its client and
retains the name until a later accept can replace it. A failed instance's cached
I/O completions are never reused for a different client.
Every accepted client is checked against its actual process TokenUser.

Clients compare the actual server PID, creation time and SID against the supplied
expected process identity before sending data. A caller-supplied SID must also
match the current user's SID. Reconnection repeats these checks. Identity comes
from retained process handles through Process Native, not pipe payloads.
HTTP and lifecycle codecs accept only the resulting authenticated stream types.

These checks separate users and fence process generations. They do not isolate
mutually untrusted, unconfined processes running as the same user. Unsafe calls
are confined to the Windows native module, with handle and buffer lifetimes
documented at each call. Tests use private pipes and require no network service.
