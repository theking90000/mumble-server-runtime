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
include("core:clients:java")
include("sdk-java")
