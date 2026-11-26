---
name: locked-build
description: Run or inspect shared-host build leases with locked-build. Use when coordinating Cargo builds, checking queued work, or diagnosing memory-cap outcomes.
---

# locked-build

Use the installed `locked-build` command to submit and inspect builds. Do not write a private wrapper around it.

## Run a command

```sh
locked-build run -- cargo test -p package-name
```

State and policy paths resolve automatically. The first invocation writes a measured host policy. Use `--state` or `--config` only to override the defaults.

Choose `--class must-fix`, `--class point`, `--class block`, or `--class periodic`. The default is `periodic`. Direct runs and the Cargo shim derive the owner from the user, host, and current worktree directory; `LOCKED_BUILD_OWNER` overrides it. Ticket labels derive from the Git branch; `LOCKED_BUILD_TICKET` overrides them. This gives each worktree its own Cargo target directory.

## Read request status

```sh
locked-build status
locked-build cancel REQUEST-ID
```

`queued` means the request is accepted and waiting for admission. Its command has not started. `running` means it owns a lease and its command has started. Use the request id from status to cancel queued or running work.

`rss_cap` means memory enforcement killed the command for exceeding its cap. `memory_cap_unavailable` means policy required memory enforcement that the launcher could not establish, so the command was not launched.

Use `locked-build policy show` to inspect the effective policy. `locked-build policy init --force` regenerates it.
