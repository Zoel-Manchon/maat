# Security policy

## Supported versions

Maat is currently a prototype. Security fixes are applied to the latest
version on the `main` branch.

## Reporting a vulnerability

Do not publish a vulnerability as a public issue when it could expose users to
data loss, unsafe file replacement or terminal escape injection.

Report it privately through GitHub's **Security → Report a vulnerability**
feature once the repository is published. Include reproduction steps, affected
platforms and a minimal proof of concept.

## Security boundaries

Maat's SHA-256 check detects content changes between the last read/write and a
save attempt. It does not provide authentication, file locking, malware
analysis, sandboxing or forensic guarantees.
