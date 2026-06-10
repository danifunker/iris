# macOS App Sandbox kills Cranelift JIT — it doesn't use MAP_JIT

**Symptom (Mac App Store / TestFlight build only):** the emulator window opens,
then the process dies *before any video* with:

```
EXC_BAD_ACCESS / SIGKILL (Code Signature Invalid)
termination namespace = CODESIGNING, "Invalid Page"
faulting thread = REX3-Processor, PC in an anonymous (image-less) executable page
```

i.e. the `REX3-Processor` thread jumps into a freshly-JITed draw shader and the
kernel kills it. Same fault hits the CPU thread if the MIPS JIT (`IRIS_JIT=1`)
is on.

**Cause:** `cranelift-jit` (0.116) allocates code pages with
`memmap2::MmapMut::map_anon` (plain `mmap(PROT_READ|WRITE)`) then flips them to
executable with `region::protect(.., READ_EXECUTE)` (an `mprotect`). It **never
uses `MAP_JIT`** and never calls `pthread_jit_write_protect_np`. See
`cranelift-jit-*/src/memory.rs`.

Under the macOS hardened runtime the App Sandbox grants only
`com.apple.security.cs.allow-jit`, which permits **MAP_JIT pages only**. A plain
`mmap`+`mprotect(PROT_EXEC)` page is "unsigned executable memory"; executing it
is a code-signing violation → `SIGKILL`. The two entitlements that *would* allow
it — `allow-unsigned-executable-memory` and `disable-executable-page-protection`
— are **rejected by App Review**, so they are not an option for the Mac App
Store. (A Developer-ID/notarized build can carry them, which is why the JIT
works there and only the MAS build crashes.)

**Fix:** force the interpreter on the sandboxed build. The GUI sets
`IRIS_NO_JIT=1` under `#[cfg(feature = "appstore")]` (iris-gui `main.rs`, before
any worker thread starts). The core honours it:

- `Rex3::new` (rex3.rs): when `IRIS_NO_JIT` is set, build with `rex_jit: None`
  and `jit_enabled = false`. Must skip **constructing** `RexJit`, not just
  dispatch — `RexJit::new` spawns a warm-up compiler thread that allocates
  executable memory immediately from the saved shader profile.
- `run_jit_dispatch` (jit/dispatch.rs): `IRIS_NO_JIT` overrides `IRIS_JIT`.

The REX3 interpreter and the MIPS interpreter are the normal fallbacks (the JITs
are caches with interpreter fall-through), so correctness is unaffected — only
draw/CPU throughput. `lightning` is still fine (it's the no-debug perf flag, not
GNU lightning JIT).

**Long-term option (not done):** patch/vendor the JIT memory allocator to use
`MAP_JIT` + `pthread_jit_write_protect_np` W^X toggling, which would let the MAS
build keep the JIT under `allow-jit`. cranelift-jit 0.116 exposes no hook for
this, so it means carrying a patched allocator.
