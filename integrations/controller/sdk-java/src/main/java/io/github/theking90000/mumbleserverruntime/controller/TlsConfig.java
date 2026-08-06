package io.github.theking90000.mumbleserverruntime.controller;

import java.nio.file.Path;
import java.util.Objects;
import java.util.Optional;

/** Server-authenticated TLS configuration. Client certificates are not part of v1. */
public final class TlsConfig {
    private final Path trustCertificate;

    private TlsConfig(Path trustCertificate) {
        this.trustCertificate = trustCertificate;
    }

    public static TlsConfig systemTrust() {
        return new TlsConfig(null);
    }

    public static TlsConfig trustCertificate(Path certificate) {
        return new TlsConfig(Objects.requireNonNull(certificate, "certificate"));
    }

    Optional<Path> trustCertificate() {
        return Optional.ofNullable(trustCertificate);
    }
}
