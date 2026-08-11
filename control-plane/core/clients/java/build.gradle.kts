import java.security.MessageDigest

plugins {
    `java-library`
    `maven-publish`
    id("com.google.protobuf") version "0.9.5"
}

group = "be.theking90000.mumble"
version = "0.1.0-SNAPSHOT"

base {
    archivesName.set("controller-core")
}

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(17))
    }
    withSourcesJar()
    withJavadocJar()
}

val coreDescriptor = layout.buildDirectory.file("descriptors/controller-core-v1.pb")
val expectedCoreDescriptorDigest = project.file("../../contract/controller-core-v1.pb.sha256")

sourceSets {
    main {
        proto {
            srcDir("../../contract/src/main/proto")
        }
    }
}

dependencies {
    api("com.google.protobuf:protobuf-java:3.25.8")
    api("io.grpc:grpc-protobuf:1.81.0")
    api("io.grpc:grpc-stub:1.81.0")
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
                descriptorSetOptions.path = coreDescriptor.get().asFile.absolutePath
                descriptorSetOptions.includeImports = true
                descriptorSetOptions.includeSourceInfo = false
            }
        }
    }
}

val verifyCoreDescriptor = tasks.register("verifyCoreDescriptor") {
    dependsOn(tasks.named("generateProto"))
    inputs.file(coreDescriptor)
    inputs.file(expectedCoreDescriptorDigest)
    doLast {
        val bytes = coreDescriptor.get().asFile.readBytes()
        val actual = MessageDigest.getInstance("SHA-256")
            .digest(bytes)
            .joinToString("") { byte: Byte -> "%02x".format(byte.toInt() and 0xff) }
        val expected = expectedCoreDescriptorDigest.readText().trim()
        if (actual != expected) {
            throw GradleException(
                "controller-core-v1 descriptor changed: expected $expected, got $actual; " +
                    "review field compatibility and update the Core descriptor pin intentionally"
            )
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

tasks.check {
    dependsOn(verifyCoreDescriptor)
}

publishing {
    publications {
        create<MavenPublication>("mavenJava") {
            artifactId = "controller-core"
            from(components["java"])
        }
    }
}
