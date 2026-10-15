# Narrow ocr-rs dependency patch

`ocr-rs/` contains the required build files, Rust sources, C++ bridge, README and
Apache-2.0 license from the crates.io `ocr-rs` 2.4.1 archive. Archive SHA-256:
`e1df6571a2030bf337eb36a0e3f9f93544182234db08e34133b52a0445e75f17`.

`image/` contains the upstream crates.io `image` 0.25.8 crate. Archive SHA-256:
`529feb3e6769d234375c4cf1ee2ce713682b8e76538cb13f9fc23e1400a591e7`. Its only
changes are in `Cargo.toml` and `Cargo.toml.orig`: remove the unused AVIF encoder
feature from `default-formats`, and remove the optional `ravif` and `rgb` encoder
dependencies and their feature links. Subforge only decodes subtitle images, so
these APIs were unused. No codec source, decoder, or algorithm is changed; the
AVIF decoder feature remains available through `avif-native`.

This narrow patch removes the inactive lockfile chain through `ravif` → `rav1e`
→ `paste`, which strict `cargo audit --deny warnings` checks even when Cargo does
not activate the optional AVIF encoder. Keep the audit strict. Revisit this patch
when the upstream dependency graph no longer locks the unmaintained crate.

The Cargo manifest changes only:

- Disable `image` default features; Subforge enables its accepted image formats.
- Upgrade `imageproc` from 0.25 to pinned 0.27.0, disable default features and
  enable `rayon`. Subforge needs geometry and contours, not font rendering or FFT.

The sole source adaptation passes `Projection` by value and wraps the unchanged
white fill pixel in `Border::Constant` at `det.rs`'s `warp_into` call, as required
by imageproc's updated API. Recognition and detection algorithms are unchanged.

This removes the unmaintained `paste` and `ttf-parser` dependencies reported by
RUSTSEC-2024-0436 and RUSTSEC-2026-0192. Dependency audit remains strict. Remove this
patch when upstream offers an equivalent maintained dependency configuration.

MNN still comes from the separate immutable, checksum-verified source archive in
the Dockerfiles; upstream's mutable prebuilt download path is not used.

The `ocr-rs/cpp/src/mnn_wrapper.cpp` patch also verifies the session backend
after every MNN session creation path when CUDA was requested. It rejects and
releases a session if MNN reports a CPU backend, so a CUDA worker cannot silently
accept a CPU-only session after backend setup falls back. The check uses the
pinned MNN `Interpreter::getSessionInfo(..., BACKENDS, ...)` API and a two-entry
buffer for the backend type and this wrapper's single backend config. Non-CUDA
sessions retain their existing behavior. This confirms the session's selected
backend; MNN may still place individual unsupported operations on CPU. Keep this
guard limited to the pinned MNN API and the four session creation paths in the
wrapper; it does not alter MNN sources or the ocr-rs ABI.
