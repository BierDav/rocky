import org.jetbrains.kotlin.gradle.dsl.KotlinMultiplatformExtension
import org.jetbrains.kotlin.gradle.plugin.KotlinPlatformType

// Room's compiler for every target, exporting each database's schema under schemas/.
plugins {
    id("com.google.devtools.ksp")
    id("androidx.room")
}

room {
    schemaDirectory(layout.projectDirectory.dir("schemas").asFile.path)
}

val compiler = versionCatalogs.named("libs").findLibrary("room-compiler").get()

pluginManager.withPlugin("org.jetbrains.kotlin.multiplatform") {
    extensions.getByType<KotlinMultiplatformExtension>().targets.configureEach {
        if (platformType != KotlinPlatformType.common) {
            dependencies.add("ksp" + name.replaceFirstChar(Char::uppercase), compiler)
        }
    }
}
pluginManager.withPlugin("org.jetbrains.kotlin.jvm") {
    dependencies.add("ksp", compiler)
}
