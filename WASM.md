# WebAssembly (wasm32) build path

This branch (`wasm`) carries the WebAssembly support for `llama-cpp-rs`. It
exists because the [nobodywho](https://nobodywho.ooo) project needs to run
local LLMs in a browser tab, and that requires the underlying Rust wrapper
plus llama.cpp itself to compile to wasm32.

## Branch composition

This branch is built from two upstream sources:

1. **Base** — [`marek-hradil/llama-cpp-rs` `main`](https://github.com/marek-hradil/llama-cpp-rs)
   at commit `8550f04e`. Marek's branch carries the
   llguidance / EOS-fix / lark / `with_logits_ith_mut` patches that
   `nobodywho/core/Cargo.toml` depends on today.
2. **Emscripten support** — three commits cherry-picked from
   [`AsbjornOlling/llama-cpp-rs` `wasm`](https://github.com/AsbjornOlling/llama-cpp-rs/tree/wasm)
   (dated March 2026):
   - `first pass: wasm32-unknown-emscripten builds (still untested)`
   - `clean up emscripten build flags config`
   - `cargo toml stuff`

`nobodywho-ooo/llama-cpp-rs` `main` is left untouched (stale at
v0.1.109 from June 2025).

## What works on this branch

| Step | Status |
|---|---|
| Marek's patches (llguidance, EOS, lark, `with_logits_ith_mut`) | ✅ inherited from `marek-hradil/main` |
| `TargetOs::Emscripten` variant + `parse_target_os` handles `wasm32-unknown-emscripten` | ✅ Asbjørn's `10e0f8b` |
| Emscripten sysroot auto-detection via `emcc --cflags --sysroot=` | ✅ Asbjørn's `10e0f8b` |
| Emscripten cmake toolchain auto-detection via `which emcc` | ✅ Asbjørn's `10e0f8b` |
| `bindgen` configured with sysroot + `--target=wasm32-unknown-emscripten` + `-fvisibility=default` (workaround for [bindgen #1941](https://github.com/rust-lang/rust-bindgen/issues/1941)) | ✅ Asbjørn's `10e0f8b` |
| `cc` shim build via `em++` with `-fwasm-exceptions` (native wasm EH, not JS polyfill) | ✅ Asbjørn's `10e0f8b` |
| cmake config disables every GPU backend (`GGML_VULKAN`, `GGML_CUDA`, etc.) | ✅ Asbjørn's `10e0f8b` |
| `LLAMA_WASM_MEM64=OFF` so the wasm32 linker doesn't choke on wasm64 objects | ✅ Asbjørn's `10e0f8b` |

## What's still TODO

### 1. End-to-end build verification

Asbjørn's commit message says "still untested" (March 2026). Someone needs
to run `cargo build --target wasm32-unknown-emscripten` against this
branch with `emcc` on PATH and verify a `.wasm` artifact is produced
without errors. Likely some flag tuning required.

Prerequisites for the build: `rustup target add wasm32-unknown-emscripten`,
`emsdk` installed and activated, `emcc` on `PATH`.

### 2. `LlamaModel::load_from_buffer`

The current `LlamaModel::load_from_file` wraps `llama_model_load_from_file`,
which takes a path. A browser tab has no filesystem, so wasm consumers
need a buffer-based loader.

Upstream llama.cpp does **not** currently expose `llama_model_load_from_buffer`
in `llama.h`. Options:

1. **Patch llama.cpp** (submodule under `llama-cpp-sys-2/llama.cpp/`) to
   add a memory-backed loader. Most invasive but cleanest.
2. **Use Emscripten's MEMFS.** Write the GGUF bytes to `/tmp/model.gguf`
   from JS, then call the existing path-based loader. Simpler. Works for
   wasm only.
3. **Mmap from a file descriptor**, opened via `memfd_create` (Linux) or
   equivalent. Not portable to wasm; rejected.

Option 2 is the pragmatic first cut. Option 1 is the long-term right answer.

### 3. Bindgen header coverage on wasm

Asbjørn's commit added `-fvisibility=default` to work around an upstream
bindgen issue, but the headers should be verified to actually emit every
symbol nobodywho calls into. In particular `llama_*`, `ggml_*`, `gguf_*`,
and (if enabled) `mtmd_*`.

### 4. Smoke test

A `wasm-bindgen-test` (or just a Cargo example) that loads a tiny GGUF
from an embedded byte array, runs one decode step. `wasm-pack test
--headless --chrome` should pass before merging this branch to anything.

## Out of scope on this branch

- **WebGPU acceleration.** llama.cpp's WebGPU backend is upstream-experimental
  and not built on Emscripten in any meaningful way today. CPU-only wasm
  is the goal for v1.
- **Multimodal (`mtmd`).** The audio/image input paths assume a filesystem
  and aren't critical for v1.
- **`wasm32-unknown-unknown`.** Doesn't have a sysroot. Emscripten is the
  practical target. WASI (`wasm32-wasip2`) could follow later.

## Downstream consumer

`nobodywho/core/Cargo.toml` points at this branch:

```toml
llama-cpp-2 = {
    git = "https://github.com/nobodywho-ooo/llama-cpp-rs",
    branch = "wasm",
    default-features = false,
    features = ["openmp", "android-static-stdcxx", "mtmd", "llguidance"]
}
```

On `target_arch = "wasm32"`, the build script auto-detects the
Emscripten target and configures cmake/bindgen accordingly. On native
targets, the branch behaves identically to `marek-hradil/main`.

See `nobodywho/wasm/README.md` in the nobodywho repo for the binding side.
