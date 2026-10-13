# Desktop cross-builds

`examples/shoppinglist-app/package-desktop` packages Rocky and Skiko for macOS
(arm64 and x86_64), Linux (arm64 and x86_64) and Windows (x86_64) into one
executable JAR. It uses Cargo, GNU cross-compilers and Apple's SDK; no containers
or Zig are needed. Windows arm64 is outside this matrix.

## Build on macOS

Install Xcode's command-line tools and the GNU toolchains:

```sh
brew install mingw-w64
brew tap messense/macos-cross-toolchains
brew install messense/macos-cross-toolchains/x86_64-unknown-linux-gnu \
  messense/macos-cross-toolchains/aarch64-unknown-linux-gnu
```

The [Linux toolchains](https://github.com/messense/homebrew-macos-cross-toolchains)
include GCC, binutils and a glibc sysroot. The helper accepts their
`<rust-target>-gcc` names or the distro-style `<architecture>-linux-gnu-gcc`.
Windows uses `x86_64-w64-mingw32-gcc`. Rust targets are installed with rustup.
Toolchains are searched in the repository's `local/bin`, then `PATH`.

Publish the SDK from the same checkout before packaging, as for ordinary
shopping app development:

```sh
sdks/kotlin/gradlew -p sdks/kotlin publishToMavenLocal
examples/shoppinglist-app/package-desktop
java -jar examples/shoppinglist-app/build/dist/shopping-desktop.jar
```

The package script builds the five Rocky libraries with the shopping app's
default iroh feature, puts them at JNA's platform resource paths in
`rocky-core-jvm`, and downloads Skiko's five prebuilt runtime JARs from Maven
Central. Their version comes from the resolved `skiko-awt` API JAR. Downloads
are checked against Central's SHA-256 and cached in the Kotlin CLI shared cache.
Every nested JAR is listed in the executable JAR's classpath index.

The final verification resolves the repaired Compose dependencies, checks all
ten native resources through the packaged classloader and loads both native
runtimes on the build host. It also exercises the manifest's `java -jar` launcher
using a temporary copy with the verification entry point. Cross-compilation
and resource inspection do not
establish successful execution on the other operating systems. Linux still
needs Skiko's system graphics libraries; Windows needs a current Windows
installation with the Universal CRT. All targets need a compatible JDK.

Rocky's explicit WAL flush opens a writable handle. Windows requires write
access for `FlushFileBuffers`; using a read-only handle fails with access denied
(OS error 5), including during peer admission and sync. Other peers can then
report EOF because that failure closes the exchange.

## Build libraries separately

`sdks/native/build-desktop [output-directory] [rust-target ...]` builds selected
targets. Without targets it builds the complete matrix on macOS. Cargo reuses
its target cache; the output is a directory of JNA resource prefixes with debug
symbols stripped from the copied libraries.

For example, Linux can build its libraries and Windows with installed GNU
cross-toolchains:

```sh
sdks/native/build-desktop local/desktop-natives \
  x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-pc-windows-gnu
```

Combine these with macOS libraries built from the same checkout, then set
`ROCKY_DESKTOP_NATIVES` to that complete directory when running
`package-desktop`. This skips compilation and requires all five libraries.
Use matching SDK bindings from that checkout; unrelated native binaries are
not interchangeable. The existing `rocky.jvmNatives` Gradle property can also
include this directory when publishing the SDK itself.
