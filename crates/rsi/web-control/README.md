# rsi-web-control

`Watch` owns one loopback listener for the requesting connection, with a validated
explicit frontend Origin. The frontend must report its bound address before this
request and finish proxy configuration before releasing a launch ticket to a UI.
Connection loss retires this listener independently of persistent Web.

The installed Desktop launcher may retain that watch listener and deliver one
bounded `DesktopLaunch` frame over private standard input. The frame contains a
canonical loopback URL, one-use ticket, and mandatory `desktop_bridge` descriptor
retained with that listener's verified assets. Desktop accepts it only in explicit
managed frontend mode; the remaining pipe is the launcher's lifetime lease.
No launch URL or ticket is placed in process arguments.

The local Web control protocol selects Host-owned listener plugins. It carries
no provider environment, browser command, or display environment. The caller
opens its own browser after receiving a one-use launch ticket.

Each request carries the exact Host epoch and generated interface descriptor.
Frames have a big-endian u32 length and bounded JSON payload, at most 16 KiB.
Unknown fields, invalid lengths and trailing payload data are rejected.
The native service transport authenticates the operating-system peer before
decoding these inputs. This is same-user local authority, not a sandbox boundary.

Ordinary Web launch retains one persistent browser listener; repeated launches
mint independent tickets without rotating its device credential. A different
explicit port or source conflicts with the live listener. Stop affects this
browser listener only. Serve and Desktop listeners belong to their individual
control connections and retire when those connections close.

`Serve` carries optional asset authority alongside its HTTP policy. Absence means
API-only; a supplied package export or absolute directory is admitted by the same
asset owner as Web/Desktop before binding. Serve uses device authentication and
its explicit TLS/Origin policy, without local-browser launch tickets.
