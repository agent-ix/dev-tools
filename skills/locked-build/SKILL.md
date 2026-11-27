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

The generated `hold_ms` default is 5,400,000 ms (90 minutes), the ceiling for
any command; tune it in each host's policy. If the limit ends a command,
`locked-build status` and the `run` output name the limit and direct you to
change policy `hold_ms`.

`rss_cap` means memory enforcement killed the command for exceeding its cap. `memory_cap_unavailable` means policy required memory enforcement that the launcher could not establish, so the command was not launched.

Use `locked-build policy show` to inspect the effective policy. `locked-build policy init --force` regenerates it.

## Install and verify a host

From the dev-tools repository root, install the binary, measured policy, and
Cargo shim with `make install-tools`. The command is idempotent and puts the
shim ahead of real Cargo in the supported shell startup files. Start a new
shell after the first install. Run `locked-build check-install` to compare all
tool versions against `tools/locked-build/tool-versions.json`; its one-line
`ok`/`mismatch` output and exit status are suitable for ix-board cadence checks.

## Wrapper migration map

| Old command or state | New command |
|---|---|
| `locked-build.sh` build lane | `locked-build run --class <must-fix\|point\|block\|periodic> --owner <owner> --ticket <ticket> -- cargo ...` |
| `locked-build.sh` Kani lane | `locked-build run --class kani --owner <owner> --ticket <ticket> -- cargo kani ...` |
| `lead-reqs-cargo.sh` | `locked-build run --class must-fix --owner lead-reqs --ticket <ticket> -- cargo ...` |
| `arch-test-run.sh` | `locked-build worktree-run --repository <repo> --slot <n> --ticket <ticket> -- cargo test ...` |
| cargo-resource-controller jobs registry | `locked-build status` and `locked-build metrics` |
| per-lane `gate.sh` or raw `timeout ... cargo` | `locked-build run --class <class> --owner <owner> --ticket <ticket> -- cargo ...` |

The checked-in host capacity examples record core and RAM measurements for
draco (18 cores), luna (8), titan (32), and terra (14). Installation uses the
host's measured cores, RAM, and disk when
writing its local policy. Terra's Linux verification is run through `ssh cave`.
Windows is unsupported: the process supervisor uses Unix process and signal
APIs.
