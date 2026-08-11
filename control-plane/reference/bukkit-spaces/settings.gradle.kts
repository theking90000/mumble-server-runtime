pluginManagement {
    repositories {
        gradlePluginPortal()
        mavenCentral()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        mavenCentral()
        // The Spigot API is not on Maven Central. This mirror carries both
        // `spigot-api` and the `bungeecord-chat` module it depends on for the
        // chat components used to send the clickable join link.
        maven {
            name = "papermc"
            url = uri("https://repo.papermc.io/repository/maven-public/")
            content {
                includeGroup("org.spigotmc")
                includeGroup("net.md-5")
            }
        }
    }
}

rootProject.name = "voice-example-plugin"

// This example is a consumer of the SDK, not a module of it: it stays out of the
// `mumble-controller` build so that `./gradlew check` there never reaches for the
// Spigot repository. The composite build below resolves the SDK dependency from
// the sources next door instead of a published artifact.
includeBuild("../..") {
    dependencySubstitution {
        substitute(module("be.theking90000.mumble:controller-spaces"))
            .using(project(":implementations:spaces:clients:controller-spaces"))
    }
}
