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
`rocky-runtime-android` compares the digest of the Rust sources recorded in the
published POM with the current sources, and fails with this command when they differ.
Desktop builds never resolve the artifact.

`buildSrc` compiles the [toolchain plugins' helpers](../tooling/build-logic/src), so the
runtime is built the same way as everywhere else: a stripped release library per ABI,
linked by NDK 28.2 for API 26, with rustup provisioned under `local/android-rust` when
none is on `PATH`. It needs the Android SDK with platform `android-35` and that NDK;
without `ANDROID_HOME`, the Android Studio default location is used.
