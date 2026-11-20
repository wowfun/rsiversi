# rsi-schedule

The native Host-generation controller waits on one UTC timer for all scheduled
Sessions. A future reminder holds its Session identity and next UTC deadline.
Only due work takes an execution slot and pins a continuation lease. At most 64
Sessions can execute concurrently, with one automatic round per Session through
Kernel admission. Its Agent-side Local interface and durable semantics belong to
[rsi-agent-schedule](../../rsi-agent/schedule/README.md).

A due reminder for an archived Session stays in the calendar with its original
deadline. Restoring the Session wakes the existing dispatcher. Other Sessions
continue to run. An already claimed Turn can finish and settle its original
reservation.
When a Host Profile omits the navigation owner, enabled reminders continue to
run, including reminders for Sessions archived in a previous Profile. Editing a
running standard Host's navigation setting takes effect on Host restart.

Tool mutation admission captures a Host epoch before persistence. Arming requires
a matching canonical receipt and the same still-open epoch after persistence.
Arming is serialized per Session; a short generation gate only covers admission
and publication, never Store or continuation I/O. At most 64 arming operations
are retained alongside the 64 drivers. Host stop cancels in-flight arming before
joining it. Shutdown revokes leases, discards pending automatic input or cancels
its claimed Turn, and durably settles the accepted reservation before joining.
Cleanup failure is retained in a bounded per-Session diagnostic, exposed by
`schedule_list`, and makes Host stop fail rather than report clean disarming.
The Host discovers edited Schedule domains at startup and restores their next
enabled occurrence or outstanding reservation. Observation and UI attachment
are read-only. Restoring a reminder keeps its original UTC anchor, identity and
lifetime allowance. UI detach does not stop an embedded Host's drivers.

The first delivery decision uses the exact Schedule snapshot admitted by the
continuation lease. After a reserve, settlement or wait for an idle Session the
driver reads the committed state again. Each delivery attempt prepares the Session once and uses the Store's current
control watermark as the command predecessor, including non-domain controls.
Settlement names the round and message from the original reservation.
Command preparation and settlement re-read current revisions on contention.
Capacity and revision contention are retried at most six times, with exponential
10–320 ms delays; persistent failures remain visible and stop the driver. Busy
reserve attempts wait for idle admission. Store errors and unknown commit outcomes
are never replayed. The driver's cancellation and cleanup deadline bound retries.
