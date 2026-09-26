# rsi-schedule

The native Host-generation controller owns at most 64 live Schedule drivers,
independently of the Goal controller and Node. It retains the exact Session
composition through a continuation lease, waits on injectable UTC timers and
Session activity, and serializes one automatic round per Session through Kernel
admission. Its Agent-side Local interface and durable semantics belong to
[rsi-agent-schedule](../../rsi-agent/schedule/README.md).

Tool mutation admission captures a Host epoch before persistence. Arming requires
a matching canonical receipt and the same still-open epoch after persistence.
Arming is serialized per Session; a short generation gate only covers admission
and publication, never Store or continuation I/O. At most 64 arming operations
are retained alongside the 64 drivers. Host stop cancels in-flight arming before
joining it. Shutdown revokes leases, discards pending automatic input or cancels
its claimed Turn, and durably settles the accepted reservation before joining.
Cleanup failure is retained in a bounded per-Session diagnostic, exposed by
`schedule_list`, and makes Host stop fail rather than report clean disarming. Observations, cold attachment and restart do not
arm work. Explicit human-root resume selects retained intent without replenishing
its lifetime allowance. UI detach does not stop an embedded Host's drivers.

Command preparation and settlement re-read current revisions on contention.
Capacity and revision contention are retried at most six times, with exponential
10–320 ms delays; persistent failures remain visible and stop the driver. Busy
reserve attempts wait for idle admission. Store errors and unknown commit outcomes
are never replayed. The driver's cancellation and cleanup deadline bound retries.
