import java.security.MessageDigest

plugins {
    `java-library`
    `maven-publish`
    id("com.google.protobuf") version "0.9.5"
}

group = "be.theking90000.mumble"
version = "0.1.0-SNAPSHOT"

base {
    archivesName.set("controller-spaces")
}

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(17))
    }
    withSourcesJar()
    withJavadocJar()
}

val spacesDescriptor = layout.buildDirectory.file("descriptors/controller-spaces-v1.pb")
val expectedSpacesDescriptorDigest = project.file("../../contract/controller-spaces-v1.pb.sha256")
val profileMetadataDirectory = layout.buildDirectory.dir("generated/sources/profileMetadata/java")
val generateControllerProfileMetadata = tasks.register("generateControllerProfileMetadata") {
    inputs.file(expectedSpacesDescriptorDigest)
    outputs.dir(profileMetadataDirectory)
    doLast {
        val digest = expectedSpacesDescriptorDigest.readText().trim()
        require(digest.matches(Regex("[0-9a-f]{64}"))) {
            "Controller descriptor digest must be 64 lowercase hexadecimal characters"
        }
        val output = profileMetadataDirectory.get().file(
            "be/theking90000/mumble/controller/spaces/internal/ProfileMetadata.java"
        ).asFile
        output.parentFile.mkdirs()
        output.writeText(
            """
            package be.theking90000.mumble.controller.spaces.internal;

            public final class ProfileMetadata {
                public static final String SPACES_PROFILE_ID = "mumble.controller.spaces";
                public static final int SPACES_SCHEMA_VERSION = 1;
                public static final String SPACES_DESCRIPTOR_DIGEST = "$digest";

                private ProfileMetadata() {
                }

            }
            """.trimIndent() + "\n"
        )
    }
}

sourceSets {
    main {
        java.srcDir(profileMetadataDirectory)
        proto {
            srcDir("../../contract/src/main/proto")
        }
    }
    create("spacesContract") {
        proto {
            srcDir("../../contract/src/main/proto")
        }
    }
}

dependencies {
    api(project(":core:clients:controller-core"))
    implementation("com.google.protobuf:protobuf-java:3.25.8")
    compileOnly("javax.annotation:javax.annotation-api:1.3.2")

    testImplementation(platform("org.junit:junit-bom:5.14.1"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

protobuf {
    protoc {
        artifact = "com.google.protobuf:protoc:3.25.8"
    }
    plugins {
        create("grpc") {
            artifact = "io.grpc:protoc-gen-grpc-java:1.81.0"
        }
    }
    generateProtoTasks {
        all().configureEach {
            plugins {
                create("grpc")
            }
            if (name == "generateSpacesContractProto") {
                generateDescriptorSet = true
                descriptorSetOptions.path = layout.buildDirectory
                    .file("descriptors/controller-spaces-v1.pb")
                    .get()
                    .asFile
                    .absolutePath
                descriptorSetOptions.includeImports = true
                // Source info carries comments and spans, which would make the pinned digest
                // change on edits that cannot break a single wire field.
                descriptorSetOptions.includeSourceInfo = false
            }
        }
    }
}

tasks.withType<JavaCompile>().configureEach {
    dependsOn(generateControllerProfileMetadata)
    options.release.set(8)
    options.encoding = "UTF-8"
    options.compilerArgs.addAll(listOf("-Xlint:all", "-Werror"))
}

tasks.named("sourcesJar") {
    dependsOn(generateControllerProfileMetadata)
}

tasks.withType<Javadoc>().configureEach {
    options.encoding = "UTF-8"
    exclude("be/theking90000/mumble/controller/**/internal/**")
    val docletOptions = options as StandardJavadocDocletOptions
    docletOptions.addBooleanOption("Xdoclint:all", true)
    docletOptions.addBooleanOption("Werror", true)
}

tasks.test {
    useJUnitPlatform()
    javaLauncher.set(javaToolchains.launcherFor {
        languageVersion.set(JavaLanguageVersion.of(8))
    })
}

tasks.register<JavaExec>("controllerInterop") {
    dependsOn(tasks.testClasses)
    classpath = sourceSets.test.get().runtimeClasspath
    mainClass.set("be.theking90000.mumble.controller.spaces.ControllerInteropMain")
    javaLauncher.set(javaToolchains.launcherFor {
        languageVersion.set(JavaLanguageVersion.of(8))
    })
    // Resolved at execution time. Reading the properties while configuring would
    // break every invocation that merely realizes this task, such as `gradlew tasks`
    // or an IDE sync, with a missing-value failure rather than a missing argument.
    val interopEndpoint = providers.gradleProperty("interopEndpoint")
    val interopTokenFile = providers.gradleProperty("interopTokenFile")
    argumentProviders.add(CommandLineArgumentProvider {
        listOf(interopEndpoint.get(), interopTokenFile.get())
    })
    standardInput = System.`in`
}

val verifyControllerDescriptor = tasks.register("verifyControllerDescriptor") {
    dependsOn(tasks.named("generateSpacesContractProto"))
    inputs.file(spacesDescriptor)
    inputs.file(expectedSpacesDescriptorDigest)
    doLast {
        val bytes = spacesDescriptor.get().asFile.readBytes()
        val actual = MessageDigest.getInstance("SHA-256")
            .digest(bytes)
            .joinToString("") { byte: Byte -> "%02x".format(byte.toInt() and 0xff) }
        val expected = expectedSpacesDescriptorDigest.readText().trim()
        if (actual != expected) {
            throw GradleException(
                "controller-spaces-v1 descriptor changed: expected $expected, got $actual; " +
                    "review field compatibility and update the Spaces descriptor pin intentionally"
            )
        }
    }
}

tasks.check {
    dependsOn(verifyControllerDescriptor)
}

publishing {
    publications {
        create<MavenPublication>("mavenJava") {
            artifactId = "controller-spaces"
            from(components["java"])
        }
    }
}
