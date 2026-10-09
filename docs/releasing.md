# Releasing

The [Release workflow](../.github/workflows/release.yml) publishes every artifact of one
commit of `main` under one version, to the packages and releases of the forge it runs on:
GitHub, or the Gitea mirror ([CI on Gitea](development.md#gitea)). The tables below name the
GitHub locations; on Gitea, images are `gitea.quickme.at/bierdav/<image>` and the Maven
repository is `https://gitea.quickme.at/api/packages/bierdav/maven`, both published with the
`PACKAGE_TOKEN` secret.

## Triggers and channels

| Channel | Trigger | Version | GitHub release |
| --- | --- | --- | --- |
| `nightly` | Daily at 02:23 UTC, or manual | `X.Y.Z-nightly.YYYYMMDD` | Prerelease; the latest 14 kept |
| `alpha`, `beta`, `rc` | Manual | `X.Y.Z-alpha.N`, `-beta.N`, `-rc.N` | Prerelease |
| `stable` | Manual | `X.Y.Z` | Latest |

Run it manually from the Actions tab or with `gh workflow run release.yml -f channel=beta`;
only the channel is chosen. Releases are cut from the default branch only.

A nightly is published only when shipped code changed since the most recent release of any
channel: a path under `crates/`, `sdks/`, the Cargo manifests and lockfile, the toolchain file or
Kotlin project files, excluding Markdown (`scripts/release/code-changed`). Documentation,
benchmarks, examples and CI changes alone produce none.

## Version numbers

`scripts/release/version` computes the version with git-cliff ([`cliff.toml`](../cliff.toml)) from
the [commit messages](commits.md) since the last stable tag `vX.Y.Z`:

- `X.Y.Z` is the last stable version bumped by the largest change: major for a breaking change,
  minor for a feature, otherwise patch. Before 1.0, as in Cargo, a breaking change bumps the minor
  version and everything else the patch. Without a stable tag, it is 0.1.0.
- Prereleases count up per version: `0.2.0-alpha.1`, `0.2.0-alpha.2`, then `0.2.0-beta.1`. They
  advance alpha, beta, rc and never go back: an alpha after a beta of the same version is refused.
- A second nightly on one day gets a `.2` suffix.

The workflow serializes runs, so two releases never take the same number. Notes are the
grouped commits since the last stable release.

## Published artifacts

| Artifact | Where | Tags or coordinates |
| --- | --- | --- |
| Coordinator image | `ghcr.io/bierdav/rocky-db-coordinator` | `<version>`, channel (`latest` for stable), `sha-<commit>` |
| Custody peer image | `ghcr.io/bierdav/rocky-db-peer` | the same |
| JVM SDK, `darwin-aarch64` and `linux-x86-64` natives | GitHub Packages Maven | `rocky:rocky-core-jvm:<version>` |
| Android SDK ([Android](runtime/android.md)) | GitHub Packages Maven | `rocky:rocky-core-android`, `rocky:rocky-sqlite`, `rocky:rocky-room` |
| Android runtime | GitHub Packages Maven | `rocky:rocky-runtime-android` |
| Packaged JVM SDK ([packaging](runtime/sdk-packaging.md)) | GitHub release assets | `rocky-kotlin-sdk-<version>-<platform>.tar.gz` |

The Maven repository is `https://maven.pkg.github.com/BierDav/rocky-db`. GitHub Packages needs
credentials even for public packages: a token with `read:packages`.

```kotlin
repositories {
    maven {
        url = uri("https://maven.pkg.github.com/BierDav/rocky-db")
        credentials { username = "<user>"; password = "<token with read:packages>" }
    }
}
dependencies { implementation("rocky:rocky-core-jvm:0.1.0-alpha.1") }
```

## Pipeline

1. Version: decides whether to release and computes the version and notes. It refuses a commit
   whose latest completed `shopping` qualification in the baseline workflow failed.
2. The [baseline workflow](development.md#ci) without the soak tests and the shopping
   qualification; it skips a tree whose checks already passed. Both images are built for amd64
   and arm64 after it, then merged and tagged at the release version.
3. In parallel with them: the JVM SDK packaged and qualified per platform, and the Android
   runtime and SDK built at the release version (`-ProckyVersion`, `ROCKY_VERSION`).
4. Publish: merges the JVM natives into one jar (`scripts/release/jvm-maven`), uploads every Maven
   artifact (`scripts/release/maven-deploy`), then creates the tag and release last.

A failed run publishes no release or tag; rerunning it computes the same version. Images are
retagged and Maven uploads that already exist are skipped, since package versions are immutable.

## Limits

- Native libraries are the development profile; they are neither optimized, signed nor notarized.
- JVM natives cover `darwin-aarch64` and `linux-x86-64`; Windows and Linux arm64 are not built.
- Pruning removes old nightly GitHub releases and tags, not their packages; delete those in the
  package settings when needed.
- Version numbers are not written into `Cargo.toml`; the tag carries the version.
