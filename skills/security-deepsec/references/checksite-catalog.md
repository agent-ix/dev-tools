# Checksite Catalog

Use this catalog to decide what DeepSec should examine and where manual checksites are required. Record each reviewed item in `CHECKSITES.json` with status: `candidate`, `finding`, `false-positive`, `no-finding`, `needs-more-context`, or `out-of-scope`.

## DeepSec Built-In Areas To Look For

Prefer actual matcher slugs from DeepSec output. Common relevant slugs include:

- React/browser: `dangerous-html`, `xss`, `postmessage-origin`, `js-react-unsafe-json-in-html`
- Redirects: `open-redirect`, `unsafe-redirect`, `untrusted-redirect-following`, `url-regex-validation`
- Auth/session: `auth-bypass`, `missing-auth`, `jwt-handling`, `oauth-flow`, `session-cookie-config`, `test-header-bypass`, `dev-auth-bypass`
- Secrets and logs: `secret-env-var`, `secrets-exposure`, `secrets-plaintext-exposure`, `secret-in-log`, `sensitive-data-in-traces`, `env-exposure`, `process-env-access`
- Client/server APIs: `js-nextjs-route-handlers`, `js-nextjs-server-action`, `js-nextjs-server-actions`, `js-nextjs-server-action-no-auth`, `js-nextjs-page-data-fetch`, `js-nextjs-page-without-auth-fetch`, `js-nextjs-middleware`, `js-nextjs-middleware-only-auth`, `js-express-route`, `js-fastify-route`, `js-hono-route`, `js-remix-route`, `js-sveltekit-route`, `js-graphql-resolver`, `trpc-public-procedure`
- Cache and tenant boundaries: `cache-key-scope`, `cache-key-poisoning`, `cross-tenant-id`
- Injection/data handling: `js-sql-raw`, `js-nosql-injection`, `object-injection`, `spread-operator-injection`, `zod-passthrough-mass-assignment`
- Transport/browser policy: `cors-wildcard`, `response-header-leak`, `error-message-leak`
- Supply/deploy support: `github-workflow-security`, `dockerfile-from-mutable-tag`, `dockerfile-run-as-root`, `k8s-secret-reference`

## Manual Checksite Categories

Create manual checksites for applicable areas even when DeepSec has no candidate. These are generic examples, not a project-specific checklist; adapt them to the target repo's language, framework, product domain, and actual security boundaries.

### Entry Points And Authorization

- Public HTTP routes, server actions, RPC handlers, GraphQL resolvers, webhooks, workers, queue consumers, scheduled jobs, CLI commands, and file importers.
- Route/middleware ordering, missing auth checks, public opt-outs, dev/test bypasses, internal headers, and unauthenticated debug endpoints.
- Tenant/org/project/user scoping in requests, database filters, cache keys, generated clients, SDKs, and permission checks.
- UI-only or client-only guards that are not backed by server-side authorization.

### Browser, Frontend, And SDK Surfaces

Use these when the target repo contains browser UI, mobile/web clients, generated clients, SDKs, or identity/session flows.

- Login form rendering, validation, submission, error handling, and account enumeration behavior.
- Signup/registration flows, invite acceptance, account activation, and tenant/org selection.
- Forgot password and password reset flows, especially reset token URL handling, referrer leakage, logging, telemetry, and post-reset session state.
- Browser token storage: localStorage, sessionStorage, cookies, in-memory stores, IndexedDB, URL fragments, query strings, and persisted Redux/Zustand/query caches.
- Cookie assumptions: SameSite, Secure, HttpOnly, domain/path scope, CSRF token flow, cross-site redirects, and whether frontend assumptions match backend config.
- Redirect/callback parameters: `next`, `redirect`, `redirect_uri`, `returnTo`, `callbackUrl`, `continue`, `state`, and router push/replace targets.
- OAuth/OIDC/SAML/social login: state/nonce generation and validation, callback routing, provider selection, error forwarding, and token/code leakage.
- MFA/passkey/WebAuthn: challenge lifetime, origin/rpId assumptions, recovery code display/storage, fallback paths, and enrollment vs login boundaries.
- Auth hooks/providers/context: stale auth state, logout clearing, refresh races, multi-tab sync, route guard bypass, SSR/client boundary mismatch.
- API clients and SDKs: auth header attachment, tenant/org header attachment, retry behavior on 401/403, refresh retry loops, base URL override, and generated client auth gaps.
- Permission clients and caches: cache keys must include tenant/org/resource type/resource ID/action/principal where those dimensions affect authorization.
- Telemetry/analytics/error reporting: credentials, tokens, reset links, auth headers, emails, tenant IDs, and sensitive PII must not be captured.
- CORS and origin assumptions in client config, including dev/prod environment splits.
- Route protection: UI guard coverage is not enough; verify data fetches and API calls are server-authorized.
- SSR/Next.js: server-only env leaks, `NEXT_PUBLIC_*` misuse, server actions as public POSTs, middleware-only auth, cached fetches crossing users/tenants, JSON-in-script escaping.
- XSS surfaces: `dangerouslySetInnerHTML`, markdown/HTML renderers, translations containing HTML, templated error messages, postMessage handlers, and linkification.
- Dependency risk in auth-adjacent packages: auth SDKs, markdown/html sanitizers, jwt/token libraries, OAuth clients, analytics, and generated clients.

### Data, Injection, And Storage

- Raw SQL, query builder escape hatches, NoSQL filters, ORM mass assignment, object spreading into trusted structures, and schema passthrough.
- File paths, uploads, archive extraction, symlinks, generated files, temp directories, and path traversal.
- SSRF and outbound fetches, URL validation, redirect following, metadata service access, and webhook callbacks.
- Serialization/deserialization, template rendering, markdown/HTML rendering, unsafe eval/runtime execution, shell commands, and sandbox escapes.
- Cache keys and invalidation boundaries for user, tenant, org, role, resource type, locale, feature flag, and authorization state.

### Secrets, Config, And Observability

- Secrets in source, env vars, fallback defaults, logs, traces, analytics, crash reports, test fixtures, generated clients, and build artifacts.
- Environment variable exposure to browsers, mobile clients, static bundles, or public config endpoints.
- Cookie/session security, CORS policy, response headers, CSP, error detail leakage, and production vs development config splits.
- CI/CD workflows, container images, mutable tags, root containers, package scripts, deployment manifests, cloud IAM, public ingress, and secret references.

## Cross-Checks For Findings

For every promoted finding, check the corresponding enforcement boundary:

- Is the invariant enforced at the trusted boundary, or only in a caller, UI, SDK, or convention?
- Does the client/SDK/service send the identity, tenant, org, CSRF, idempotency, or authorization context the receiver expects?
- Can a direct caller, job payload, replay, alternate client, or modified request bypass the intended constraint?
- Are errors, logs, metrics, and traces compatible with non-disclosure and non-enumeration requirements?
- Do specs and tests describe the same invariant the code enforces?

## Custom Matcher Triggers

Consider adding or proposing custom DeepSec matchers when:

- A repo has many entry points DeepSec does not detect.
- The stack uses internal auth wrappers, route factories, generated clients, SDK conventions, queue frameworks, custom RPC, or deployment conventions.
- A true positive reveals a sibling pattern likely to recur.
- DeepSec candidates cluster under broad `other-*` or generic slugs.

Prefer precise or normal matchers with tight file globs. Use noisy matchers only for entry-point coverage where candidate count is intentionally bounded by directory structure.
