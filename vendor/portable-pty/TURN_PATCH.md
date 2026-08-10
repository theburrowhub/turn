# Turn's portable-pty patch

This directory vendors `portable-pty` 0.9.0 (MIT, upstream license retained) with one narrow Unix API
extension:

- `CommandBuilder::preserve_fd` adds an explicit descriptor allowlist.
- Unix pre-exec cleanup continues closing every descriptor above stderr except that allowlist.
- Allowlisted descriptors remain `FD_CLOEXEC` in the parent; the forked child clears
  the flag only after cleanup, eliminating cross-thread spawn inheritance.

Turn uses the API only to carry a checkout lock into a main-checkout process. Remove the patch and return to
the crates.io release when upstream offers an equivalent preservation API.

The vendored baseline is the crates.io `portable-pty` 0.9.0 package. Apart from this note, the only changed
upstream files are `src/cmdbuilder.rs` and `src/unix.rs`.
