// The original inventory contract on the legacy process-host transport, and precise SQL observation.
plugins {
    id("rocky.jvm")
}

dependencies {
    implementation(project(":rocky-core"))
}

kotlin {
    compilerOptions { allWarningsAsErrors.set(true) }
}

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

tasks.check { dependsOn(qualify) }
