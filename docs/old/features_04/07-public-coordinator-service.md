# 7. Public coordinator registration and operation

Status: implemented software, 2026-10-06; no public deployment.
Owner: coordinator service/client, starter configuration and provider runbooks. Depends on phases 2, 3, 5 and 6. Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [shared coordination](../runtime/shared-coordination.md), phase 6's accepted
retention contract and [Directory registration](../applications/directory/registration.md).
The current root-certified small native mesh is a foundation, not a public
multi-tenant service. Do not simply remove its quota checks or expose fixture keys.

Deliver a provider-operated metadata service and client setup allowing an app to
register its own isolated domains without deploying app code or a Directory app.
The provider's coordination authority is independent of each app publisher.

## Ordered slices

1. Record authenticated registration/control/trust bootstrap: the client pins the
   provider identity, proves ownership and installs a domain under its authority.
   Define tenant/account quota ownership without assuming external login grants
   app data rights. Use signed capabilities or equivalent bounded control tokens;
   identities survive restarts without cloning author logs.
2. Ensure registration carries only owner/control identities, bounded eligible
   executors/invokers, definition commitments, generations and generic limits.
   No table schema, handler executable, arguments, rows or reader keys are uploaded.
   Enrollment and coordinator eligibility remain distinct grants.
3. Isolate tenants in state, authentication, head/lookup/control paths, caches,
   rate limits and capacity. Quotas cover domains, sessions, bytes, concurrent
   work and registration churn; noisy tenants cannot exhaust all service lanes.
   Apply phase 6 retirement to business and administrative requests.
4. Use iroh as the authenticated confidential public client transport
   ([resolved choices](resolved-choices.md)). Current signed plaintext TCP is not an adequate default for public
   credential/control metadata. Preserve certified quorum and voter identity
   verification; transport encryption does not replace consensus authorization.
5. Provide provider-style configuration for a public address/trust bundle, plus
   identical self-hosted setup. No endpoint is advertised as available until
   deployed and tested. Supply local multi-process service setup for reproducible
   development with independent secrets, not built-in production fixture keys.
6. Add bounded metrics/logging and operating procedures: provisioning, restart,
   membership/key changes, coherent backup/restore, capacity, outages and permanent
   quorum loss. Reads should not cause unbounded consensus/storage amplification;
   any read optimization must prove its consistency contract. Snapshot/backup
   storage and exported diagnostics must follow phase 6 bounds and avoid secrets.

Do not let the coordinator become a schema registry, application host, payload
custodian or general peer directory. Always-on execution/custody can be separately
configured. Cross-provider migration is deferred: reject unsupported changes of
an existing domain's authority instead of treating them as URL edits.

## Completion gate

- Two independent app owners register isolated domains on one multi-process
  coordinator and confirm commands without uploading schemas/handlers/payloads.
  Cross-tenant acquire/control/lookup/admission and quota-bypass attempts fail.
- An owner changes eligible executors with fencing while preserving the head;
  registration does not confer application row/decryption rights.
- One tenant saturates its quota while another continues within the declared
  fairness/latency budget. Phase 6 storage bounds hold under registration churn.
- Leader/voter loss, quorum outage and controlled restart preserve state. Mergeable
  writes continue while strong tickets report pending/unknown honestly.
- Backups restore only under safe fenced procedures; loss/reset cannot fabricate
  success or regress authoritative heads. Logs/metrics contain no app payloads.
- The starter accepts public-style or self-hosted config and has a clear trust,
  availability, recovery-window and payload-custody explanation.

Run coordinator multi-process/security/fairness tests, affected adapter tests and
`./scripts/check`. Write runbooks; do not deploy/publish a real public service
without a separate deployment request and concrete provider configuration.

## Delivered evidence and limits

[Decision 0027](../decisions/0027-public-coordination-service.md) records the
registration, transport, isolation and restore formats. The
[service guide](../runtime/public-coordination.md) covers trust bundles, tenant
registration and starter configuration; the
[operations runbook](../runtime/coordinator-operations.md) covers independent
keys, local provisioning, membership, backup, fenced restore and permanent loss.

[Qualification](../runtime/public-coordination-evidence.md) covers two owners on
three voter processes, cross-tenant denials, executor fencing, bounded churn,
request-lane fairness, outages, restarts, restore and real runtime/SDK commands
over iroh. Phase 6's persistent storage plateau and crash gates remain in force.

No endpoint is deployed or advertised. Internet/relay load, production capacity,
hardware power cuts and Byzantine voters are unqualified. Foreign-language
service administration and executing Kotlin handler composition remain phase 8;
the starter already accepts the same public/self-hosted trust bundle through
its JSON/UniFFI boundary. Cross-provider migration is refused and deferred.
