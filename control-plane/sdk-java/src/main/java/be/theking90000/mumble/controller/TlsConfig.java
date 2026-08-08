package be.theking90000.mumble.controller;

import java.nio.file.Path;
import java.util.Objects;
import java.util.Optional;

/**
 * Server-authenticated TLS configuration.
 *
 * <p>Client certificates and mutual TLS are not part of version 1 of the SDK.</p>
 */
public final class TlsConfig {
    private final Path trustCertificate;

    private TlsConfig(Path trustCertificate) {
        this.trustCertificate = trustCertificate;
    }

    /**
     * Uses the default trust managers configured for the running JVM.
     *
     * @return a TLS configuration backed by JVM system trust
     */
    public static TlsConfig systemTrust() {
        return new TlsConfig(null);
    }

    /**
     * Trusts servers using a PEM-encoded CA certificate file.
     *
     * @param certificate path to the PEM trust certificate
     * @return a TLS configuration using that certificate
     * @throws NullPointerException if {@code certificate} is {@code null}
     */
    public static TlsConfig trustCertificate(Path certificate) {
        return new TlsConfig(Objects.requireNonNull(certificate, "certificate"));
    }

    Optional<Path> trustCertificate() {
        return Optional.ofNullable(trustCertificate);
    }
}
