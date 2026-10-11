// Rocky's native runtime for Android: librocky_uniffi.so and JNA's libjnidispatch.so for every
// packaged ABI. rocky-core depends on it, so Android apps receive it transitively.
plugins {
    id("rocky.android-library")
}

val runtime = tasks.register<BuildAndroidRuntime>("buildRuntime") {
    repository.set(repositoryRoot)
    sources.from(rustSources())
    cache.set(rootProject.layout.buildDirectory.dir("android-tools"))
    libraries.set(layout.buildDirectory.dir("runtime"))
}

androidComponents {
    onVariants { variant ->
        variant.sources.jniLibs?.addGeneratedSourceDirectory(runtime, BuildAndroidRuntime::libraries)
    }
}
