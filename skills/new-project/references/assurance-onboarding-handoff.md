# Generic assurance onboarding handoff

This is the consumer-owned boundary between `new-project` and an optional,
installed assurance provider. It defines what the scaffold workflow passes and
records; it does not define assurance policy or provider behavior.

## Preconditions and decision

Construct a handoff only after scaffold verification has finished. Preserve the
ordered verification checks exactly as executed.

- If verification is `failed` or `incomplete`, do not perform plugin discovery.
  Record `deferred` with reason code `scaffold_verification_failed`.
- Otherwise ask the creator to choose `invoke`, `deferred`, `skipped`, or
  `not_applicable`.
- Require a non-empty human-readable reason for every choice except `invoke`.
  Preserve the selected outcome and reason without invoking a provider.

The scaffold remains successful and usable for every handoff outcome.

## Caller-owned payload

Build one `agent-ix.assurance-onboarding-handoff/v1` object:

```json
{
  "contract": "agent-ix.assurance-onboarding-handoff/v1",
  "repository": {
    "organization": "<organization>",
    "name": "<repository-name>",
    "path": "<absolute-local-path>",
    "remote_url": "<remote-url-or-null>"
  },
  "template_type": "<selected-template>",
  "verification": {
    "outcome": "passed",
    "checks": [
      {
        "command": "<exact-command>",
        "exit_code": 0,
        "summary": "<concise-observed-result>"
      }
    ]
  },
  "requested_context": {},
  "decision": {
    "outcome": "invoke",
    "reason": ""
  }
}
```

Treat `requested_context` as opaque caller input. Do not require or add a
provider policy, profile, plan, schema, workflow, or engine-specific field.

## Installed-skill discovery

For an eligible `invoke` decision, use the host's installed-plugin APIs:

1. Find the installed plugin whose manifest identity is exactly
   `engineering-assurance`.
2. Require an immutable plugin version and a non-empty declared skill source.
3. Let the host resolve that source relative to the installed plugin root. Do
   not use an absolute, workstation-specific, or guessed installation path.
4. From the resolved source, select exactly one skill whose frontmatter name is
   `assurance-onboarding` and confirm the host exposes it as callable.
5. Invoke that installed skill through the host and provide the complete v1
   payload unchanged as its invocation context.

Do not require the provider manifest to repeat the caller-owned handoff version.
Compatibility is the versioned plugin identity plus its declared skill source,
the exact skill name, and a callable host entry point.

Handle discovery failures explicitly:

| Condition | Outcome | Reason code | Required detail |
| --- | --- | --- | --- |
| Host has no plugin system or plugin is absent | `deferred` | `plugin_unavailable` | Obtain the provider's current installation help from the host catalog or plugin manager. |
| Installed plugin has no uniquely named skill | `deferred` | `capability_missing` | Record the observed plugin identity and version. |
| Manifest, immutable version, skill source, skill metadata, or callable entry point is malformed | `deferred` | `entrypoint_incompatible` | Record the observed incompatibility. |

Never fall back to a copied skill, provider command, or provider filesystem
layout. Installation guidance must remain module-published; report what the host
catalog returns instead of reproducing instructions here.

## Result record

Record one `agent-ix.assurance-onboarding-handoff-result/v1` object containing:

- `contract`: the exact result contract identifier;
- `input`: the complete handoff payload unchanged;
- `outcome`: exactly `completed`, `failed`, `deferred`, `skipped`, or
  `not_applicable`;
- `provider`: installed plugin identity, immutable version, and skill name when
  invocation was attempted;
- `provider_result`: the returned value or failure diagnostic, retained as
  opaque provider-owned data;
- `reason_code` and `reason` whenever the outcome is not `completed`; and
- `installation_help` only when returned by the host catalog for an unavailable
  plugin.

A normal provider return is `completed`. A host or provider invocation error is
`failed` with `provider_failed`; preserve its diagnostic without translating
provider states. Do not create, modify, interpret, or delete assurance artifacts
at this generic boundary, and do not roll back the verified scaffold.
