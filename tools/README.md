# tools

Everything the repository needs that is not the product.

The root used to carry these beside the crates, which made it hard to see at a
glance what Aperio *is*: four crates, a dashboard, its docs and its tests. What
lives here supports building, checking, packaging and deploying that, and none
of it ships inside a binary.

| Directory | What it is |
| --- | --- |
| [`charts/`](charts) | The Helm chart for the server. `helm lint tools/charts/aperio-server`. |
| [`fuzz/`](fuzz) | `cargo-fuzz` targets for the tunnel wire protocol. A standalone workspace, so the main `cargo build` never sees it: `cargo +nightly fuzz run --fuzz-dir tools/fuzz <target>`. |
| [`packaging/`](packaging) | nfpm descriptions, systemd units and the manifest renderer behind the `.deb` and `.rpm` builds. Paths inside the nfpm files are relative to the **repository root**, because that is where the release workflow runs them from. |
| [`scripts/`](scripts) | Two helpers: a CI check that the warm cache and the release build stay in step, and a per-request profiler. |

One thing deliberately stayed at the root. `aperio-tunnel-action/` is a GitHub
Action, and an action's path *is* its public address: people write
`uses: co3moz/aperio/aperio-tunnel-action@master` in workflows this repository
cannot see. Moving it would break every one of them for the sake of a tidier
listing.
