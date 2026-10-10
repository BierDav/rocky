# Android runtime

A Gradle build that publishes Rocky's native runtime for Android as
`rocky:rocky-runtime-android:0.1.0` to Maven local (`~/.m2`). The AAR holds
`librocky_uniffi.so` and JNA's `libjnidispatch.so` for `arm64-v8a` and `x86_64`.

It exists because the Kotlin toolchain cannot put native libraries in its own Android
libraries, and its plugins cannot contribute them. The toolchain does merge the native
libraries of a Maven AAR into an app, so the
[`rocky-runtime-android`](../rocky-runtime-android/module.yaml) module depends on this
artifact. An Android app gets Rocky's runtime by depending on that module.

```sh
sdks/kotlin/android-runtime/gradlew -p sdks/kotlin/android-runtime publishToMavenLocal
```

Run it once, and again after the Rust sources change. Every build of
`rocky-runtime-android` checks the current sources against both the published
POM and provenance embedded inside the AAR on every build. Its generated
resource records the AAR's content hash, so another workspace replacing the
shared Maven-local artifact invalidates dependent app builds. A mismatch fails
with the publication command; a cached check cannot silently accept another wire version.
Desktop builds never resolve the artifact.

Parallel workspaces can select a private Maven-local repository with the standard
`maven.repo.local` JVM property. Use the same path for publication and consumption:

```sh
sdks/kotlin/android-runtime/gradlew -p sdks/kotlin/android-runtime \
    -Dmaven.repo.local="$PWD/local/android-maven" publishToMavenLocal
KOTLIN_CLI_JAVA_OPTIONS="-Dmaven.repo.local=$PWD/local/android-maven" \
    ./kotlin build -p android -m marketAndroid
```

The provenance check honors that property as the toolchain's resolver does.

`buildSrc` compiles the [toolchain plugins' helpers](../tooling/build-logic/src), so the
runtime is built the same way as everywhere else: a stripped release library per ABI,
linked by NDK 28.2 for API 26, with rustup provisioned under `local/android-rust` when
none is on `PATH`. It needs the Android SDK with platform `android-35` and that NDK;
without `ANDROID_HOME`, the Android Studio default location is used.
