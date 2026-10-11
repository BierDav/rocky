// The original inventory contract on the legacy process-host transport, and typed-command type errors.
plugins {
    id("rocky.jvm")
}

dependencies {
    implementation(project(":rocky-core"))
}

kotlin {
    compilerOptions { allWarningsAsErrors.set(true) }
}

val compiler by configurations.creating
dependencies { compiler(libs.kotlin.compiler.embeddable) }

val host = tasks.register<Exec>("buildProcessHost") {
    workingDir(repositoryRoot)
    commandLine("cargo", "build", "-p", "rocky-runtime", "--example", "sdk_host", "--locked")
}

val qualify = tasks.register<JavaExec>("qualify") {
    description = "Runs the inventory contract against the process host."
    dependsOn(host)
    classpath = sourceSets.main.get().runtimeClasspath
    mainClass.set("rocky.QualificationKt")
    val work = layout.buildDirectory.dir("qualification")
    doFirst { work.get().asFile.deleteRecursively() }
    args(work.get().asFile.path, repositoryRoot.asFile.path)
}

val rejected = tasks.register<RejectedFixtures>("rejectedFixtures") {
    description = "Checks that each fixture under rejected/ is a type error."
    fixtures.from(fileTree("rejected") { include("*.kt") })
    support.from(fileTree("rejected/support"))
    classpath.from(sourceSets.main.get().compileClasspath)
    compiler.from(configurations["compiler"])
    work.set(layout.buildDirectory.dir("rejected"))
}

tasks.check { dependsOn(qualify, rejected) }
