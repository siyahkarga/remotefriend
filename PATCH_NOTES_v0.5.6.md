> Historical document (applies to an older version).

# v0.5.6 Hotfix Changes

## Performance

- Target 15 FPS → configurable 30 FPS.
- Fixed the `processing time + 66 ms` pacing bug.
- A single global H.264 and a single global JPEG broadcast pipeline.
- Small broadcast queues; stale frames are skipped when lagging.
- Fixed the first-frame deadlock in the client.
- Removed the full RGBA frame clone and unnecessary texture uploads in the client.
- Added browser H.264/JPEG decode backpressure.
- Added a monitor enumeration cache, a persistent input worker and file streaming.

## Security

- Protocol `2`, transport generation `4`.
- The internet web password is verified on the host.
- Fixed TLS handshake signature verification.
- CSPRNG host ID, 256-bit persistent host secret and 128-bit dial-back token.
- Random default session password.
- Origin check, CSP and other HTTP security headers.
- Bounded channels, session limits, timeouts and size limits.
- Safe incoming file directory/name/chunk ordering/atomic completion.
- Password removed from localStorage.
- Documented that the VPS relay is not E2E encrypted.

## Breaking change

Not protocol-compatible with older binaries. Host, client and rendezvous must be updated together.

## Verification status

Static consistency, TOML/JSON and browser JavaScript syntax checks were run on this source package. The final `cargo build` could not be run because the environment that prepared the package had no Rust toolchain; a release build/test in CI or on a local machine is mandatory before real use.
