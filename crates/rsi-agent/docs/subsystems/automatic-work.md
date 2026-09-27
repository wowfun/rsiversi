# Automatic work

[Schedule](../../schedule/README.md) and Goal share atomic idle admission through
independent continuation domains. The Kernel arbitrates before either domain
reserves a round; durable intent alone never recreates a live timer or Goal owner.

The [Goal](../../goal/README.md) and [Schedule](../../schedule/README.md)
packages own domain allocation and settlement. Their product controllers retain
separate live continuation authority. Admission waits for the owning subtree to
be idle; stored intent does not arm a controller. Ordinary waking input takes
precedence over pending continuation input. The [Kernel contract](../../kernel/README.md)
owns the exact admission and revocation rules.
