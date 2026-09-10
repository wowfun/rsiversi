# rsi-files-tools

`FilesToolsFactory` is an ordinary Agent contribution. It registers `file_read`
and `directory_list` through the staged Tool registrar and consumes the independent
Files reader. It does not call the human Files API or consult WorkspaceTrust.
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
