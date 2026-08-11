plugins {
    `java-library`
    `maven-publish`
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

dependencies {
    testImplementation(platform("org.junit:junit-bom:5.14.1"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(8)
    options.encoding = "UTF-8"
    options.compilerArgs.addAll(listOf("-Xlint:all", "-Werror"))
}

tasks.withType<Javadoc>().configureEach {
    options.encoding = "UTF-8"
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

publishing {
    publications {
        create<MavenPublication>("mavenJava") {
            artifactId = "controller-core"
            from(components["java"])
        }
    }
}
