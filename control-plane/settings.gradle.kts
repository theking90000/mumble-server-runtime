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
include("core:clients:controller-core")
project(":core").projectDir = file("../control/coordination")
project(":core:clients").projectDir = file("../control/coordination/sdk")
project(":core:clients:controller-core").projectDir = file("../control/coordination/sdk/java")
include("implementations:spaces:clients:controller-spaces")
project(":implementations:spaces:clients:controller-spaces").projectDir =
    file("implementations/spaces/clients/java")
