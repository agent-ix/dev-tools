# SWM-242 round-three fix dispositions

Coder dispositions for PR #25 round three at the fix head. The reviewer SR-003 and SR-004 files are copied unchanged from the round-three review scratchpad.

| Finding | Disposition | Evidence |
|---|---|---|
| FND-022 | fixed | Admission-blocked time is stored in one host-level accumulator rather than tied to the selected request. Store transactions recalculate the current queued head's reason after cancel, interrupt, recovery, and terminal transitions. Tests cover cancellation with another queued request and changing from memory to target-disk reason. |
| FND-023 | fixed | Stale heartbeat alone no longer expires an interval. Expiry checks all queued worker identities; a verified live worker or unavailable process evidence preserves the active interval. Runtime test holds the state lock beyond the poll grace while live workers have stale heartbeats and confirms no counter regression. |
| FND-024 | fixed | Load-time expiry checkpoints to the last valid heartbeat, closes the active interval, and compares against the pre-load sidecar snapshot so the update is persisted immediately. Runtime coverage verifies last-waiter expiry, sidecar persistence, and nondecreasing value after reopen. |
| FND-025 | fixed | Persisted active reasons now deserialize through the closed `BlockReason` enum; the reverse string-to-enum `from_label` mapping was removed. |
| FND-026 | fixed | README and reference state that `metrics --output` has no default and show the full Terra WSL output file path. |
| FND-027 | fixed | Terra WSL documentation uses `/mnt/c/Users/<user>/.ix/metrics/ix_build_metrics.prom`, with no personal user path. |
