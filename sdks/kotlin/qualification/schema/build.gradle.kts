import org.jetbrains.kotlin.gradle.dsl.JvmTarget

// Hand and Room-imported definitions must install equivalently in the native runtime.
// Multiplatform with only the JVM: Room's database constructors are `expect` objects.
plugins {
    id("org.jetbrains.kotlin.multiplatform")
    id("rocky.room")
}

kotlin {
    jvm {
        compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
    }
    sourceSets {
        commonMain.dependencies {
            implementation(project(":rocky-room"))
        }
        jvmTest.dependencies {
            implementation(kotlin("test"))
            // The reference engine Room runs on in these tests.
            implementation(libs.sqlite.bundled)
        }
    }
}

tasks.withType<Test>().configureEach {
    useJUnitPlatform()
}

val definitions = layout.buildDirectory.dir("definitions")

val export = tasks.register<JavaExec>("exportDefinitions") {
    description = "Writes the hand and Room-imported definitions."
    val main = kotlin.jvm().compilations["main"]
    classpath = files(main.output.allOutputs, main.runtimeDependencyFiles)
    mainClass.set("rocky.SchemaQualificationKt")
    doFirst { definitions.get().asFile.deleteRecursively() }
    args(definitions.get().asFile.path, repositoryRoot.asFile.path)
    outputs.dir(definitions)
}

val qualify = tasks.register<Exec>("qualify") {
    description = "Installs the exported definitions in the native runtime."
    dependsOn(export)
    workingDir(repositoryRoot)
    commandLine("cargo", "run", "-p", "rocky-sdk", "--example", "qualify_schema_builder", "--locked", "--", definitions.get().asFile.path)
}

tasks.named("check") { dependsOn(qualify) }
