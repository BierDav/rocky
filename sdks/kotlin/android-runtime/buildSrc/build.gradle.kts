plugins {
    `kotlin-dsl`
}

// The Kotlin toolchain plugins' helpers, so both builds produce the runtime the same way.
sourceSets.main {
    kotlin.srcDir("../../tooling/build-logic/src")
    // The embedded compiler is only for the toolchain plugins.
    kotlin.exclude("Compiler.kt")
}
