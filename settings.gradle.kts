pluginManagement {
    repositories {
        gradlePluginPortal()
        mavenCentral()
    }
}

// Tests run on a Java 8 launcher; resolve that toolchain automatically when no local JDK 8 exists.
plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "1.0.0"
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        mavenCentral()
    }
}

rootProject.name = "mumble-controller"
include("coordination:sdk:controller-core")
project(":coordination").projectDir = file("control/coordination")
project(":coordination:sdk").projectDir = file("control/coordination/sdk")
project(":coordination:sdk:controller-core").projectDir =
    file("control/coordination/sdk/java")
include("implementations:spaces:sdk:controller-spaces")
project(":implementations").projectDir = file("implementations")
project(":implementations:spaces").projectDir = file("implementations/spaces")
project(":implementations:spaces:sdk").projectDir = file("implementations/spaces/sdk")
project(":implementations:spaces:sdk:controller-spaces").projectDir =
    file("implementations/spaces/sdk/java")
include("implementations:spaces:tools:load-driver-java")
project(":implementations:spaces:tools").projectDir = file("implementations/spaces/tools")
project(":implementations:spaces:tools:load-driver-java").projectDir =
    file("implementations/spaces/tools/load-driver-java")
