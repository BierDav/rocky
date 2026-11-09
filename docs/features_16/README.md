# features_16: public coordinator and relay

Status: implemented and deployed (2026-11-09).

The `rocky-voter` artifact includes an optional `relay-server` feature and
`relay HTTP_BIND_ADDRESS` command. TLS terminates at the deployment's proxy;
networking remains optional and server dependencies are native-only. The relay
limits incoming traffic to 1 MiB/s per connection and shuts down on termination.

`check BUNDLE.json RELAY_URL TENANT_KEY_FILE` verifies the current bundle,
registers a temporary domain, reads tenant usage and closes the domain using a
client without IP transports. It exercises public admission and consensus through
the relay, using a locally retained tenant key. Provider private keys are never
published. `Dockerfile.coordinator` packages the relay, voter and check command;
the native CI check also tests this feature.

Deployment belongs to `selfhosted-platform/score/rocky-coordinator`: three
independent voters on the Oracle nodes, a singleton relay and two setup-page
replicas. `https://rocky.quickme.at/bundle.json` publishes format 2, and the relay
URL is `https://rocky-relay.quickme.at`. The public image supports Linux amd64 and
arm64. The operator explicitly requested replacing format 1 and deleting its
stores; no compatibility or migration is provided.

Measured evidence:

- The packaged relay test exchanges authenticated QUIC data between endpoints
  without IP transports. Relay-feature clippy, the build without networking,
  and the wasm build with the relay feature selected pass.
- The Kubernetes chart passes Helm lint and server-side validation. The setup
  page, trust bundle, discovery configuration and relay respond over HTTPS.
- An external macOS relay-only client registered a domain, read its committed
  usage (1,472 bytes) and closed it. An anonymously pulled amd64 container also
  completed registration, usage and closure (1,471 bytes).
- All three signed admin statuses reported voter set `[1, 2, 3]`, leader 1 and
  the same applied index after the first public check. Each voter keeps its own
  claim; Argo sync waves roll them sequentially. After the final rollout all
  three retained applied index 12 and reported leader 3.
- The old DaemonSet and credentials were pruned. Cleanup jobs on all four former
  voter nodes verified `/var/lib/rocky-coordinator` was empty. The platform is
  deployed from revision `7459839069188fddbacbd2ac1ee65546fb4bd559`, with image
  manifest `sha256:5a94d24e20785fc646b5a8642d8a0aa7c1b5d53d0ca40632c3dc656a4dc0adc6`.
- `scripts/check` passed 264 native unit tests, 1,177 of 1,178 integration tests,
  all 908 wasm tests in 71 runs, and browser/native relay synchronization. The
  SDK test `a_partition_granted_to_an_identity_reaches_a_node_it_links_later`
  failed once with a revocation size-limit error and passed its isolated rerun;
  the full check therefore exited nonzero. `scripts/lint` also emitted the
  existing invalid arithmetic-expression diagnostic in `chaos.yml`; the changed
  baseline workflow passes actionlint separately.

These checks demonstrate the deployed registration and relay path. They do not
qualify arbitrary application adapters or promise public capacity or availability.
