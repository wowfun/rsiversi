# rsi-files-tools

`FilesToolsFactory` is an ordinary Agent contribution. It registers `file_read`
and `directory_list`, plus `present`, through the staged Tool registrar and consumes the independent
Files reader. It does not call the human Files API.
The existing Tool catalog gate, resolved Turn policy and approval owner remain
in force. A read-only operation does not imply approval exemption.

Every invocation obtains `ToolExecution::workspace_read()` from its exact pinned
Sandbox. The resulting scope fixes mode, cwd, workspace and Sandbox generation;
model input cannot select a root, policy mode, provider or generation. All three
existing modes permit these workspace-only reads. Arguments accept either a UTF-8 `path` or exact `path_hex`, never both.
Relative paths start at that
scope's cwd, cannot traverse parents and never follow symlinks. `.` selects the
cwd for directory listing. The Files reader opens only the scope's workspace
root and descends through held directory handles.

A Tool invocation opens one fresh bounded file/directory snapshot, reads one page,
then releases its private caller namespace on success, error or cancellation.
No retained token is exposed to model context. A later Tool invocation observes
a fresh snapshot; it does not promise a directory remained unchanged between
independent calls. Human UI continuations use the separate retained-token API.
Reader version checks still guard every page against mutation during that call.

Byte offsets and page limits use the Files protocol bounds. File results retain
exact hex bytes and safe UTF-8 display text; when decoding/sanitization changes
bytes, model-facing text includes the exact hex too. Directory entries retain
exact cwd-relative hex paths for later Tool calls, including unrepresentable
filenames. These contents remain untrusted data, never project instructions.
No process enforcement stamp is fabricated for a read.

`present` validates all requested regular files before returning one complete
`PresentedFiles` value (version 1). It accepts one to eight cwd-relative paths,
each with an optional description of at most 256 UTF-8 bytes. Descriptions reject
control characters and explicit Unicode bidirectional controls; ordinary joiners
and multilingual text remain available. The value records
exact path bytes and captured length, with a 160 KiB encoded ceiling. Each metadata
token is released before opening the next file, so one invocation retains at most
one token while inspecting its declaration. It declares
current files; it neither snapshots contents nor grants later human access.

`file_read` and `directory_list` opt into Local program calls. Program dispatch
retains the same sandbox read scope and sealed Tool admission as model calls.
