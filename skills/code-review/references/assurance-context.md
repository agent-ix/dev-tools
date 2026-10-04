# Assurance Context for Code Review

Use this reference only when an `AssuranceProfile` applies to the reviewed change. The
profile selects context for the existing code-review judgement; it does not replace the
rubric in `SKILL.md` or make analyzer output self-interpreting.

## Establish applicability and baseline

Read the profile's `scope`, impact scenarios, relationships, selected practices, and
exceptions. Record:

- profile id and path;
- source revision, comparison base, and changed paths actually reviewed;
- the concern and project-owned impact tier that make the profile applicable;
- linked assurance artifacts and records that were available;
- selected context that was unavailable or stale.

Do not infer applicability from a directory name or apply every profile in the
repository. If scope is ambiguous, keep that ambiguity visible in the review instead of
quietly treating the profile as applicable or irrelevant.

## Evaluate, do not merely inventory

Use applicable context as follows:

- **ArchitectureDescription:** evaluate changed code against declared conformance rules,
  decisions, boundaries, and quality scenarios. If the profile calls for architecture
  evaluation but no description or rule is available, say so explicitly.
- **Change impact:** inspect changed and transitively impacted claims, decisions, code,
  tests, configurations, and evidence. A missing graph edge is a traceability limitation,
  not proof that nothing is impacted.
- **MeasurementPlan and records:** interpret a measurement only with its plan id,
  definition version, unit, producer/version/configuration, environment, sampling
  identity, and comparison base. Do not compare unlike definitions or tools, invent a
  universal threshold, or promote an observe-stage metric into a gate.
- **Evidence independence:** check only dimensions selected for the exact obligation.
  Different labels are asserted lineage, not proof; expose shared or missing actors,
  implementation/toolchains, techniques, data sources, and review paths.
- **Evidence-producer reliance:** use the decision for this producer and intended use.
  Surface unmet validation, limitations, invalidation triggers, and stale decisions; do
  not treat a tool as globally trusted.
- **Structural coverage:** review only profile-selected decision surfaces. Consume each
  gap's disposition and owner; do not turn a repository percentage into severity.
- **Exceptions:** retain the exception's scope, owner, rationale, compensating evidence,
  and expiry. An expired or scope-mismatched exception is not approval.

When Quoin assurance, audit, comparison, or impact commands are available, use their
machine-readable output rather than reconstructing stored evidence. Quire remains the
authority for authored artifact structure. A command that is unavailable must be
reported as unavailable context; do not substitute a fabricated clean result.

## Integrate findings once

Relate each observation to a concrete implementation, test, architecture, or evidence
failure before creating a finding. Merge it with the corresponding ordinary rubric
finding when both describe the same defect. Tool exit status, a high numeric value, or a
profile impact tier does not directly choose the finding severity or verdict.

Add an `## Assurance Context` section to the existing `SpecReview` when a profile
applies. It should identify the profile and baseline, summarize what was evaluated, list
unavailable or stale inputs, and retain active exceptions. Keep the existing validated
`analysis: code-review`, findings table, and verdict rules.
