"""Stage iOS-only publications and inventory every distributed file."""

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

ROOT = Path(sys.argv[2]).resolve()
WORK = ROOT / "local/ios-build"
PACKAGE = ROOT / "local/packaged-ios"
VERSION = os.environ.get("ROCKY_VERSION", "0.1.0")
if not re.fullmatch(r"[0-9A-Za-z][0-9A-Za-z.+-]*", VERSION):
    raise ValueError("invalid ROCKY_VERSION")


def copy_tree(source, destination):
    shutil.copytree(source, destination, dirs_exist_ok=True)


def prepare():
    project = WORK / "project"
    shutil.rmtree(project, ignore_errors=True)
    shutil.rmtree(PACKAGE, ignore_errors=True)
    project.mkdir(parents=True)
    PACKAGE.mkdir(parents=True)
    (project / "project.yaml").write_text("modules: [rocky-core, rocky-room, rocky-sqlite]\n")
    for module in ("rocky-core", "rocky-sqlite", "rocky-room"):
        source = ROOT / "sdks/kotlin" / module
        dest = project / module
        copy_tree(source / "src", dest / "src")
        # The platform binding is an implementation below the common API.
        if module == "rocky-core":
            copy_tree(source / "src@ios", dest / "src@ios")
            generated = WORK / "bindings"
            shutil.copy2(generated / "RockyNative.kt", dest / "src@ios/RockyNative.kt")
            for file in generated.glob("*.kt"):
                if file.name != "RockyNative.kt":
                    shutil.copy2(file, dest / "src" / file.name)
            for platform, target in (("iosArm64", "aarch64-apple-ios"), ("iosSimulatorArm64", "aarch64-apple-ios-sim")):
                interop = dest / f"cinterop@{platform}"
                (interop / "include").mkdir(parents=True)
                shutil.copy2(generated / "rocky_uniffiFFI.h", interop / "include")
                (interop / "rocky.def").write_text(
                    "headers = rocky_uniffiFFI.h malloc/malloc.h\n"
                    "headerFilter = rocky_uniffiFFI.h malloc/malloc.h\n"
                    "package = rocky_uniffi.cinterop\n"
                    "staticLibraries = librocky_uniffi.a\n"
                    f"libraryPaths = {ROOT}/sdks/native/target/{target}/debug\n"
                    "linkerOpts = -framework CoreFoundation -framework Security -framework SystemConfiguration -framework Network -liconv\n"
                )
        dependencies = {
            "rocky-core": "  - org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2: exported\n  - org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0: exported\n  - org.jetbrains.kotlinx:atomicfu:0.31.0\n",
            "rocky-sqlite": "  - ../rocky-core: exported\n  - androidx.sqlite:sqlite:2.6.2: exported\n",
            "rocky-room": "  - ../rocky-core: exported\n  - ../rocky-sqlite: exported\n  - androidx.room:room-runtime:2.8.4: exported\n",
        }[module]
        (dest / "module.yaml").write_text(
            "product:\n  type: kmp/lib\n  platforms: [iosArm64, iosSimulatorArm64]\n"
            "repositories:\n  - url: mavenLocal\n    publish: true\n"
            f"dependencies:\n{dependencies}"
            "settings:\n  kotlin:\n    version: 2.4.20\n    freeCompilerArgs: [-Xexpect-actual-classes]\n"
            f"  publishing:\n    enabled: true\n    group: rocky\n    artifactId: {module}-ios\n    version: {VERSION}\n"
        )


def finish():
    # Maven local bookkeeping is not an artifact and must not reach the registry.
    for file in (PACKAGE / "maven").rglob("*"):
        if file.is_file() and (file.name == "_remote.repositories" or file.name.startswith("maven-metadata")):
            file.unlink()
    subprocess.run([
        "xcodebuild", "-create-xcframework",
        "-library", str(ROOT / "sdks/native/target/aarch64-apple-ios/debug/librocky_uniffi.a"),
        "-headers", str(WORK / "project/rocky-core/cinterop@iosArm64/include"),
        "-library", str(ROOT / "sdks/native/target/aarch64-apple-ios-sim/debug/librocky_uniffi.a"),
        "-headers", str(WORK / "project/rocky-core/cinterop@iosSimulatorArm64/include"),
        "-output", str(PACKAGE / "RockyRuntime.xcframework"),
    ], check=True)
    (PACKAGE / "BUILD.json").write_text(json.dumps({
        "version": VERSION, "minimumIos": "13.0", "kotlin": "2.4.20",
        "xcode": subprocess.check_output(["xcodebuild", "-version"], text=True).strip(),
        "profile": "debug", "runtimeEmbeddedInCinterop": True,
    }, indent=2) + "\n")
    inventory = {
        str(file.relative_to(PACKAGE)): hashlib.file_digest(file.open("rb"), "sha256").hexdigest()
        for file in sorted(PACKAGE.rglob("*")) if file.is_file()
    }
    (PACKAGE / "SHA256.json").write_text(json.dumps(inventory, indent=2) + "\n")
    print(PACKAGE)


{"prepare": prepare, "finish": finish}[sys.argv[1]]()
