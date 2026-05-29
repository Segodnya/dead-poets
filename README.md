# dead-poets

A playful reference to Dead Poets Society — “dead” here meaning dead (unused) keys hidden in your PO catalogs. Other catchy options you could consider:

## Scope & safety

dead-poets reports keys as **Dead** only with respect to the **source roots it scans**. External
consumers are invisible to static analysis: databases/configs, other services or repositories sharing
the same catalog, and email/cron/generated templates. A key marked `Dead` may still be used outside the
scanned code.

**Never auto-delete from this report.** It is a ranked review list — verify each key (e.g. in TMS,
the source of truth) before removing it.
