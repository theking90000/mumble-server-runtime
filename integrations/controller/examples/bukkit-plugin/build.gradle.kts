plugins {
    java
    id("com.gradleup.shadow") version "9.6.1"
}

group = "be.theking90000.mumble.bukkit"
version = "0.1.0-SNAPSHOT"

base {
    archivesName.set("voice-example-plugin")
}

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(17))
    }
}

dependencies {
    // Substituted onto `:sdk-java` of the neighbouring build by settings.gradle.kts,
    // so this example always compiles against the SDK sources in this repository.
    implementation("be.theking90000.mumble:controller:0.1.0-SNAPSHOT")

    // The server provides the Bukkit API at runtime; it must never enter the jar.
    compileOnly("org.spigotmc:spigot-api:1.8.8-R0.1-SNAPSHOT")
}

tasks.withType<JavaCompile>().configureEach {
    // Spigot 1.8 runs on Java 8.
    options.release.set(8)
    options.encoding = "UTF-8"
    options.compilerArgs.addAll(listOf("-Xlint:all", "-Werror"))
}

tasks.processResources {
    // `plugin.yml` carries the plugin version, so keep it in step with the build.
    val pluginVersion = project.version.toString()
    inputs.property("pluginVersion", pluginVersion)
    filesMatching("plugin.yml") {
        expand("version" to pluginVersion)
    }
}

tasks.shadowJar {
    archiveClassifier.set("")

    // gRPC and Protobuf are shared, unrelocatable-by-default libraries. Another
    // plugin shipping different versions would otherwise win the classloader race
    // and break one of the two.
    relocate("io.grpc", "be.theking90000.mumble.bukkit.libs.grpc")
    relocate("com.google.protobuf", "be.theking90000.mumble.bukkit.libs.protobuf")
    relocate("com.google.common", "be.theking90000.mumble.bukkit.libs.guava")
    relocate("com.google.thirdparty", "be.theking90000.mumble.bukkit.libs.guava.thirdparty")
    relocate("io.perfmark", "be.theking90000.mumble.bukkit.libs.perfmark")
    // Not in the book's list, but the Spigot server jar ships its own Gson and
    // wins the delegation, so gRPC's service-config parsing would run against
    // whichever version the server happens to bundle.
    relocate("com.google.gson", "be.theking90000.mumble.bukkit.libs.gson")

    // gRPC finds its transport, name resolvers and load balancers through
    // `META-INF/services`. Without this the descriptors keep their original names
    // and point at classes the relocation moved, and the channel fails to build.
    // Several dependencies contribute entries for the same descriptor. The default
    // EXCLUDE strategy drops the later copies before the transformer sees them.
    duplicatesStrategy = DuplicatesStrategy.INCLUDE
    mergeServiceFiles()

    // No `minimize()`: gRPC resolves transports and codecs through service loaders
    // and reflection, and minimisation drops classes nothing references statically.
}

tasks.build {
    dependsOn(tasks.shadowJar)
}
