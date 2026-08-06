import java.security.MessageDigest

plugins {
    `java-library`
    `maven-publish`
    id("com.google.protobuf") version "0.9.5"
}

group = "be.theking90000.mumble"
version = "0.1.0-SNAPSHOT"

base {
    archivesName.set("controller")
}

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(17))
    }
    withSourcesJar()
    withJavadocJar()
}

sourceSets {
    main {
        proto {
            srcDir("../contract/src/main/proto")
        }
    }
}

dependencies {
    implementation("com.google.protobuf:protobuf-java:3.25.8")
    implementation("io.grpc:grpc-protobuf:1.81.0")
    implementation("io.grpc:grpc-stub:1.81.0")
    implementation("io.grpc:grpc-netty-shaded:1.81.0")
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
            if (name == "generateProto") {
                generateDescriptorSet = true
                descriptorSetOptions.path = layout.buildDirectory
                    .file("descriptors/controller-v1.pb")
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
    options.release.set(8)
    options.encoding = "UTF-8"
    options.compilerArgs.addAll(listOf("-Xlint:all", "-Werror"))
}

tasks.withType<Javadoc>().configureEach {
    options.encoding = "UTF-8"
    exclude("be/theking90000/mumble/controller/internal/**")
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
    mainClass.set("be.theking90000.mumble.controller.ControllerInteropMain")
    javaLauncher.set(javaToolchains.launcherFor {
        languageVersion.set(JavaLanguageVersion.of(8))
    })
    args(
        providers.gradleProperty("interopEndpoint").get(),
        providers.gradleProperty("interopTokenFile").get()
    )
    standardInput = System.`in`
}

val verifyControllerDescriptor = tasks.register("verifyControllerDescriptor") {
    dependsOn(tasks.named("generateProto"))
    val descriptor = layout.buildDirectory.file("descriptors/controller-v1.pb")
    val expectedDigest = rootProject.file("contract/controller-v1.pb.sha256")
    inputs.file(descriptor)
    inputs.file(expectedDigest)
    doLast {
        val bytes = descriptor.get().asFile.readBytes()
        val actual = MessageDigest.getInstance("SHA-256")
            .digest(bytes)
            .joinToString("") { byte: Byte -> "%02x".format(byte.toInt() and 0xff) }
        val expected = expectedDigest.readText().trim()
        if (actual != expected) {
            throw GradleException(
                "controller-v1 descriptor changed: expected $expected, got $actual; " +
                    "review field compatibility and update contract/controller-v1.pb.sha256 intentionally"
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
            artifactId = "controller"
            from(components["java"])
        }
    }
}
