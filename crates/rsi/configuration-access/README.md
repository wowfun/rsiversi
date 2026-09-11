# rsi-configuration-access

This ordinary Host plugin owns explicit configuration grants for registered
DeviceIds. Local callers retain configuration authority. An authenticated device
starts without a grant; Session and navigation operations remain independent.
The `rsi.configuration.grants` Storage domain contains one revisioned document,
at most 64 devices and 64 KiB, routed to the configured backend. One writer owns
its revision CAS. API revisions are canonical decimal strings.

The Local-only grant/revoke APIs verify actual registered devices and persist
before publishing a grant. Revocation first closes admission, then drains all
admitted configuration mutations and durably removes the grant before replying.
A failed revocation stays closed in this generation and reports the failure;
restart reads the durable state. Admitted writes are retained by the plugin
when their response waiter is dropped. Unknown outcomes must be reconciled with
the Local grant snapshot; callers never automatically replay mutations.

The Settings API holds a grant lease throughout its existing replace/clear
operation. Granted remote callers may edit `rsi.agent`, select an existing default
through `rsi.agent-presets` while preserving its roots, and edit
`rsi.client`. Clearing preset settings is Local-only because it can
change roots. Other namespaces remain Local-only. Namespace validation, exact
registration identity and revision CAS still belong to Settings. Endpoint and
provider forms use the same grant admission when composed by their owning plugin.

There are eight non-queued configuration mutation slots across all callers and
one non-queued grant writer. Each device gate retains only its active lease count;
closing it rejects new leases before waiting. Device-authentication revocation
also rejects further admission through previously authenticated origins; it does
not undo already admitted operations. Retirement closes all admission and
drains retained grant writes and configuration leases. No secret is stored here.

The same plugin exposes redacted credential status and separately receipted
set/unset operations for the three closed managed provider owner identities.
There is no remote resolve operation. Status is authenticated; mutations require
the same trusted-origin grant lease. Secrets are bounded to 64 KiB and moved into
the existing zeroizing credential value; mutation errors never echo store text.
Environment-owned credentials remain read-only. Each admitted credential write
is retained through completion independently of its response waiter. A successful
receipt confirms only that credential operation; provider apply and default-model
selection are separate operations. An uncertain store result is never replayed.
