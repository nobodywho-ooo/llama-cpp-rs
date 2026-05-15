use cmake::Config;
use glob::glob;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::DirEntry;

enum WindowsVariant {
    Msvc,
    Other,
}

enum AppleVariant {
    MacOS,
    WatchOS,
    Other,
}

enum TargetOs {
    Windows(WindowsVariant),
    Apple(AppleVariant),
    Linux,
    Android,
    /// `wasm32-unknown-unknown` (or `wasm32-wasip1`) built against the
    /// `wasi-sdk` sysroot. The resulting wasm has plain WASI imports for
    /// libc, which the JS host polyfills (or wasm-bindgen + browser shim
    /// handle).
    WasmUnknown,
}

macro_rules! debug_log {
    ($($arg:tt)*) => {
        if std::env::var("BUILD_DEBUG").is_ok() {
            println!("cargo:warning=[DEBUG] {}", format!($($arg)*));
        }
    };
}

fn parse_target_os() -> Result<(TargetOs, String), String> {
    let target = env::var("TARGET").unwrap();

    if target.contains("windows") {
        if target.ends_with("-windows-msvc") {
            Ok((TargetOs::Windows(WindowsVariant::Msvc), target))
        } else {
            Ok((TargetOs::Windows(WindowsVariant::Other), target))
        }
    } else if target.contains("apple") {
        if target.ends_with("-apple-darwin") {
            Ok((TargetOs::Apple(AppleVariant::MacOS), target))
        } else if target.contains("watchos") {
            Ok((TargetOs::Apple(AppleVariant::WatchOS), target))
        } else {
            Ok((TargetOs::Apple(AppleVariant::Other), target))
        }
    } else if target.contains("android")
        || target == "aarch64-linux-android"
        || target == "armv7-linux-androideabi"
        || target == "i686-linux-android"
        || target == "x86_64-linux-android"
    {
        // Handle both full android targets and short names like arm64-v8a that cargo ndk might use
        Ok((TargetOs::Android, target))
    } else if target.contains("linux") {
        Ok((TargetOs::Linux, target))
    } else if target.starts_with("wasm32-") {
        // wasm32-unknown-unknown, wasm32-wasip1, wasm32-wasi*: all use the
        // wasi-sdk toolchain. The actual sysroot/target triple is resolved
        // from `$WASI_SDK_PATH` later.
        Ok((TargetOs::WasmUnknown, target))
    } else {
        Err(target)
    }
}

fn get_cargo_target_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out_dir = env::var("OUT_DIR")?;
    let path = PathBuf::from(out_dir);
    let target_dir = path
        .ancestors()
        .nth(3)
        .ok_or("OUT_DIR is not deep enough")?;
    Ok(target_dir.to_path_buf())
}

fn extract_lib_names(out_dir: &Path, build_shared_libs: bool, target_os: &TargetOs) -> Vec<String> {
    let lib_pattern = match target_os {
        TargetOs::Windows(_) => "*.lib",
        TargetOs::Apple(_) => {
            if build_shared_libs {
                "*.dylib"
            } else {
                "*.a"
            }
        }
        TargetOs::Linux | TargetOs::Android => {
            if build_shared_libs {
                "*.so"
            } else {
                "*.a"
            }
        }
        // wasm produces static .a archives only; BUILD_SHARED_LIBS is forced
        // OFF for wasm in the cmake config below.
        TargetOs::WasmUnknown => "*.a",
    };
    let libs_dir = out_dir.join("lib*");
    let pattern = libs_dir.join(lib_pattern);
    debug_log!("Extract libs {}", pattern.display());

    let mut lib_names: Vec<String> = Vec::new();

    // Process the libraries based on the pattern
    for entry in glob(pattern.to_str().unwrap()).unwrap() {
        match entry {
            Ok(path) => {
                let stem = path.file_stem().unwrap();
                let stem_str = stem.to_str().unwrap();

                // Remove the "lib" prefix if present
                let lib_name = if stem_str.starts_with("lib") {
                    stem_str.strip_prefix("lib").unwrap_or(stem_str)
                } else {
                    if path.extension() == Some(std::ffi::OsStr::new("a")) {
                        let target = path.parent().unwrap().join(format!("lib{}.a", stem_str));
                        std::fs::rename(&path, &target).unwrap_or_else(|e| {
                            panic!("Failed to rename {path:?} to {target:?}: {e:?}");
                        })
                    }
                    stem_str
                };
                lib_names.push(lib_name.to_string());
            }
            Err(e) => println!("cargo:warning=error={}", e),
        }
    }
    lib_names
}

fn extract_lib_assets(out_dir: &Path, target_os: &TargetOs) -> Vec<PathBuf> {
    let shared_lib_pattern = match target_os {
        TargetOs::Windows(_) => "*.dll",
        TargetOs::Apple(_) => "*.dylib",
        TargetOs::Linux | TargetOs::Android => "*.so",
        // No shared libraries on wasm — wasi-sdk produces static .a only.
        // Return early so the caller gets an empty Vec rather than searching
        // for shared assets that don't exist.
        TargetOs::WasmUnknown => return Vec::new(),
    };

    let shared_libs_dir = match target_os {
        TargetOs::Windows(_) => "bin",
        _ => "lib",
    };
    let libs_dir = out_dir.join(shared_libs_dir);
    let pattern = libs_dir.join(shared_lib_pattern);
    debug_log!("Extract lib assets {}", pattern.display());
    let mut files = Vec::new();

    for entry in glob(pattern.to_str().unwrap()).unwrap() {
        match entry {
            Ok(path) => {
                files.push(path);
            }
            Err(e) => eprintln!("cargo:warning=error={}", e),
        }
    }

    files
}

fn macos_link_search_path() -> Option<String> {
    let output = Command::new("clang")
        .arg("--print-search-dirs")
        .output()
        .ok()?;
    if !output.status.success() {
        println!(
            "failed to run 'clang --print-search-dirs', continuing without a link search path"
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("libraries: =") {
            let path = line.split('=').nth(1)?;
            return Some(format!("{}/lib/darwin", path));
        }
    }

    println!("failed to determine link search path, continuing without it");
    None
}

fn validate_android_ndk(ndk_path: &str) -> Result<(), String> {
    let ndk_path = Path::new(ndk_path);

    if !ndk_path.exists() {
        return Err(format!(
            "Android NDK path does not exist: {}",
            ndk_path.display()
        ));
    }

    let toolchain_file = ndk_path.join("build/cmake/android.toolchain.cmake");
    if !toolchain_file.exists() {
        return Err(format!(
            "Android NDK toolchain file not found: {}\n\
             This indicates an incomplete NDK installation.",
            toolchain_file.display()
        ));
    }

    Ok(())
}

/// In-place patch of llama.cpp to make `common/` build under wasi-libc.
///
/// llama.cpp's `common/` library is partially CLI-shaped: it pulls in
/// cpp-httplib for HTTP, `console.cpp` for `termios.h`-driven terminal IO,
/// `arg.cpp` for `sys/syslimits.h`-based argument parsing, etc. None of
/// that compiles against wasi-libc, and the Rust bindings don't need any
/// of those code paths (they're for `main`, `server`, and friends — not
/// the library API).
///
/// This patch surgically removes the offending bits from llama.cpp's
/// CMakeLists files. Each edit is idempotent (re-applying on a patched
/// file is a no-op), and cargo's git checkout cache scopes the
/// modification per-fork-commit so other builds aren't affected.
fn patch_out_cpp_httplib(llama_src: &Path) {
    // 1. common/CMakeLists.txt: remove the `cpp-httplib` entry from the
    //    target_link_libraries block, and strip the source files in
    //    `common/` that include POSIX headers wasi-libc doesn't ship.
    let common_cmake = llama_src.join("common/CMakeLists.txt");
    if let Ok(content) = std::fs::read_to_string(&common_cmake) {
        let mut patched = content.replace(
            "target_link_libraries(${TARGET} PRIVATE\n    build_info\n    cpp-httplib\n)",
            "target_link_libraries(${TARGET} PRIVATE\n    build_info\n)",
        );
        // Source files that drag in <termios.h>, <net/if.h>, <sys/syslimits.h>,
        // or the cpp-httplib headers. The Rust bindings (wrapper_common.cpp,
        // wrapper_oai.cpp) only need chat.*, common.*, json-schema-to-grammar,
        // sampling.*, log.*, unicode.*, jinja/*, and assorted parser helpers —
        // not any of these CLI/HTTP utilities.
        for source_line in &[
            "    arg.cpp\n",
            "    arg.h\n",
            "    console.cpp\n",
            "    console.h\n",
            "    download.cpp\n",
            "    download.h\n",
            "    hf-cache.cpp\n",
            "    hf-cache.h\n",
            "    http.h\n",
        ] {
            patched = patched.replace(source_line, "");
        }
        if patched != content {
            std::fs::write(&common_cmake, &patched)
                .expect("rewriting common/CMakeLists.txt failed");
        }
    }

    // 2. Top-level CMakeLists.txt: remove the `add_subdirectory(vendor/cpp-httplib)`
    //    line so the project never even tries to compile it.
    let main_cmake = llama_src.join("CMakeLists.txt");
    if let Ok(content) = std::fs::read_to_string(&main_cmake) {
        let patched = content.replace(
            "    add_subdirectory(vendor/cpp-httplib)\n",
            "",
        );
        if patched != content {
            std::fs::write(&main_cmake, &patched)
                .expect("rewriting CMakeLists.txt failed");
        }
    }

    // 3. common/common.cpp: two source-level patches.
    //
    //    (a) `fs_get_cache_directory()` has a chain of `#elif defined(...)`
    //    branches ending in `#error Unknown architecture`. There's no wasm32
    //    case even though wasm32-wasip1 defines `__wasi__`. Insert a wasi
    //    branch that delegates to a "not implemented" abort — wasm consumers
    //    don't have a filesystem cache anyway (models load from in-memory
    //    bytes).
    //
    //    (b) `set_process_priority()` has a Win32 path and a POSIX path; the
    //    POSIX path calls `setpriority(PRIO_PROCESS, …)`. wasi-libc doesn't
    //    define `PRIO_PROCESS` (process priorities aren't part of WASI).
    //    Insert a wasi no-op that just returns true — there's no preemption
    //    concept on wasm anyway.
    let common_cpp = llama_src.join("common/common.cpp");
    if let Ok(content) = std::fs::read_to_string(&common_cpp) {
        let mut patched = content.clone();

        // (a) fs_get_cache_directory unknown-arch.
        let cache_needle = "#elif defined(__EMSCRIPTEN__)\n        GGML_ABORT(\"not implemented on this platform\");\n#else";
        let cache_replacement = "#elif defined(__EMSCRIPTEN__)\n        GGML_ABORT(\"not implemented on this platform\");\n#elif defined(__wasi__)\n        GGML_ABORT(\"not implemented on this platform\");\n#else";
        if patched.contains(cache_needle) && !patched.contains("#elif defined(__wasi__)\n        GGML_ABORT") {
            patched = patched.replace(cache_needle, cache_replacement);
        }

        // (b) set_process_priority: insert a wasi no-op between the
        //     #if defined(_WIN32) / #else POSIX branches.
        let prio_needle = "#else // MacOS and POSIX\n#include <sys/types.h>\n#include <sys/resource.h>\n\nbool set_process_priority(enum ggml_sched_priority prio) {";
        let prio_replacement = "#elif defined(__wasi__)\n\nbool set_process_priority(enum ggml_sched_priority /*prio*/) {\n    // wasi has no process-priority concept; treat any priority as ok.\n    return true;\n}\n\n#else // MacOS and POSIX\n#include <sys/types.h>\n#include <sys/resource.h>\n\nbool set_process_priority(enum ggml_sched_priority prio) {";
        if patched.contains(prio_needle) && !patched.contains("#elif defined(__wasi__)\n\nbool set_process_priority") {
            patched = patched.replace(prio_needle, prio_replacement);
        }

        if patched != content {
            std::fs::write(&common_cpp, &patched)
                .expect("rewriting common/common.cpp failed");
        }
    }
}

/// Locate the wasi-sdk install root. Probes (in order):
///
/// 1. `$WASI_SDK_PATH` env var
/// 2. `~/wasi-sdk-*-{arch}-{os}` (the unpacked layout from a github release)
/// 3. `/opt/wasi-sdk` (conventional system install)
///
/// Panics with an instructive message if none of these resolve. The wasm32
/// build path requires wasi-sdk's clang + wasi-sysroot to compile llama.cpp's
/// C/C++ (Apple's system clang doesn't ship a wasm32 backend).
fn detect_wasi_sdk_root() -> String {
    // 1. Explicit env var.
    if let Ok(path) = env::var("WASI_SDK_PATH") {
        if Path::new(&path).join("bin/clang").exists() {
            return path;
        }
    }
    // 2. Home directory: look for `wasi-sdk-*` directory.
    if let Ok(home) = env::var("HOME") {
        if let Ok(entries) = std::fs::read_dir(&home) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("wasi-sdk-")
                    && entry.path().join("bin/clang").exists()
                {
                    return entry.path().to_string_lossy().into_owned();
                }
            }
        }
    }
    // 3. Conventional `/opt` install.
    if Path::new("/opt/wasi-sdk/bin/clang").exists() {
        return "/opt/wasi-sdk".into();
    }
    panic!(
        "Could not find wasi-sdk. Set $WASI_SDK_PATH, or unpack a release \
         tarball under ~/wasi-sdk-XX (e.g. wasi-sdk-33.0-arm64-macos), or \
         install to /opt/wasi-sdk. Releases: https://github.com/WebAssembly/wasi-sdk/releases"
    );
}

/// Configure a cc::Build for `wasm32-unknown-unknown` via wasi-sdk. Suppresses
/// cc's defaults (so `-fno-exceptions` doesn't slip in and conflict with
/// `-fexceptions`), re-adds the flags we need, and forces PIC. Uses wasi-sdk's
/// clang and points at the wasi-sysroot for libc headers.
///
/// The C code targets `wasm32-wasip1` so it has a real libc to link against;
/// the Rust side targets `wasm32-unknown-unknown` for wasm-bindgen. At link
/// time the two wasm32 sub-targets merge — the result has WASI imports for
/// libc which the JS host polyfills (e.g. via `@bjorn3/browser_wasi_shim`).
fn configure_wasm_unknown_cc(build: &mut cc::Build) {
    let sdk = detect_wasi_sdk_root();
    let clang = format!("{sdk}/bin/clang");
    let sysroot = format!("{sdk}/share/wasi-sysroot");

    build.compiler(&clang);
    build.cpp_link_stdlib(None);
    build.no_default_flags(true);

    let opt_level = env::var("OPT_LEVEL").unwrap_or_else(|_| "0".into());
    build.flag(&format!("-O{opt_level}"));
    // Target wasi-libc so libc symbols (fread, malloc, etc.) resolve.
    build.flag("--target=wasm32-wasip1");
    build.flag(&format!("--sysroot={sysroot}"));
    build.flag("-ffunction-sections");
    build.flag("-fdata-sections");
    // Legacy (-fexceptions) rather than -fwasm-exceptions: wasi-sdk's
    // prebuilt libc++ uses the legacy exception model, and mixing it with
    // the new -fwasm-exceptions model in our own C++ produces an
    // "uses a mix of legacy and new exception handling instructions"
    // wasm-validation error at module compile time.
    build.flag("-fexceptions");
    build.flag("-fPIC");
    // wasm SIMD128 — llama.cpp's GGML quantization kernels (q4_K dot
    // products, etc.) have intrinsic-using fast paths under
    // `__wasm_simd128__`. Without this flag they fall back to scalar
    // code, ~3-5x slower for matmul-heavy inference.
    build.flag("-msimd128");
    // Allow #include <signal.h> against wasi-libc — see the matching cmake
    // CMAKE_C_FLAGS define and the rustc-link-lib=wasi-emulated-signal below.
    build.flag("-D_WASI_EMULATED_SIGNAL");
    // Same dance for <sys/resource.h> / getrusage / PRIO_PROCESS: enable
    // wasi-libc's wall-clock emulation. Link with -lwasi-emulated-process-clocks.
    build.flag("-D_WASI_EMULATED_PROCESS_CLOCKS");
}

fn is_hidden(e: &DirEntry) -> bool {
    e.file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or_default()
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let (target_os, target_triple) =
        parse_target_os().unwrap_or_else(|t| panic!("Failed to parse target os {t}"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let target_dir = get_cargo_target_dir().unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("Failed to get CARGO_MANIFEST_DIR");
    let llama_src = Path::new(&manifest_dir).join("llama.cpp");
    let build_shared_libs = cfg!(feature = "dynamic-link");

    let build_shared_libs = std::env::var("LLAMA_BUILD_SHARED_LIBS")
        .map(|v| v == "1")
        .unwrap_or(build_shared_libs);
    let profile = env::var("LLAMA_LIB_PROFILE").unwrap_or("Release".to_string());
    let static_crt = env::var("LLAMA_STATIC_CRT")
        .map(|v| v == "1")
        .unwrap_or(false);

    println!("cargo:rerun-if-env-changed=LLAMA_LIB_PROFILE");
    println!("cargo:rerun-if-env-changed=LLAMA_BUILD_SHARED_LIBS");
    println!("cargo:rerun-if-env-changed=LLAMA_STATIC_CRT");

    debug_log!("TARGET: {}", target_triple);
    debug_log!("CARGO_MANIFEST_DIR: {}", manifest_dir);
    debug_log!("TARGET_DIR: {}", target_dir.display());
    debug_log!("OUT_DIR: {}", out_dir.display());
    debug_log!("BUILD_SHARED: {}", build_shared_libs);

    // Make sure that changes to the llama.cpp project trigger a rebuild.
    let rebuild_on_children_of = [
        llama_src.join("src"),
        llama_src.join("ggml/src"),
        llama_src.join("common"),
    ];
    for entry in walkdir::WalkDir::new(&llama_src)
        .into_iter()
        .filter_entry(|e| !is_hidden(e))
    {
        let entry = entry.expect("Failed to obtain entry");
        let rebuild = entry
            .file_name()
            .to_str()
            .map(|f| f.starts_with("CMake"))
            .unwrap_or_default()
            || rebuild_on_children_of
                .iter()
                .any(|src_folder| entry.path().starts_with(src_folder));
        if rebuild {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }

    // Speed up build
    env::set_var(
        "CMAKE_BUILD_PARALLEL_LEVEL",
        std::thread::available_parallelism()
            .unwrap()
            .get()
            .to_string(),
    );

    // Bindings
    let mut bindings_builder = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", llama_src.join("include").display()))
        .clang_arg(format!("-I{}", llama_src.join("ggml/include").display()))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .derive_partialeq(true)
        .allowlist_function("ggml_.*")
        .allowlist_type("ggml_.*")
        .allowlist_function("gguf_.*")
        .allowlist_type("gguf_.*")
        .allowlist_function("llama_.*")
        .allowlist_type("llama_.*")
        .allowlist_function("llama_rs_.*")
        .allowlist_type("llama_rs_.*")
        .prepend_enum_name(false);

    // mtmd: include the headers in bindgen on ALL targets where the feature is
    // on, so the FFI types/declarations exist in `bindings.rs`. We do NOT
    // compile the C++ implementation on wasm32-unknown-unknown (miniaudio uses
    // pthread sched APIs wasi-libc doesn't ship — see the cc::Build gate
    // below). The Rust wrapper module in llama-cpp-2 stays buildable; calls
    // into mtmd_* end up as wasm imports the JS host can polyfill (or
    // realistically: never called from the wasm binding's surface).
    if cfg!(feature = "mtmd") {
        bindings_builder = bindings_builder
            .header("wrapper_mtmd.h")
            .allowlist_function("mtmd_.*")
            .allowlist_type("mtmd_.*");
    }

    // Configure Android-specific bindgen settings
    if matches!(target_os, TargetOs::Android) {
        // Detect Android NDK from environment variables
        let android_ndk = env::var("ANDROID_NDK")
            .or_else(|_| env::var("ANDROID_NDK_ROOT"))
            .or_else(|_| env::var("NDK_ROOT"))
            .or_else(|_| env::var("CARGO_NDK_ANDROID_NDK"))
            .or_else(|_| {
                // Try to auto-detect NDK from Android SDK
                if let Some(home) = env::home_dir() {
                    let android_home = env::var("ANDROID_HOME")
                        .or_else(|_| env::var("ANDROID_SDK_ROOT"))
                        .unwrap_or_else(|_| format!("{}/Android/Sdk", home.display()));

                    let ndk_dir = format!("{}/ndk", android_home);
                    if let Ok(entries) = std::fs::read_dir(&ndk_dir) {
                        let mut versions: Vec<_> = entries
                            .filter_map(|e| e.ok())
                            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                            .collect();
                        versions.sort();
                        if let Some(latest) = versions.last() {
                            return Ok(format!("{}/{}", ndk_dir, latest));
                        }
                    }
                }
                Err(env::VarError::NotPresent)
            })
            .unwrap_or_else(|_| {
                panic!(
                    "Android NDK not found. Please set one of: ANDROID_NDK, NDK_ROOT, ANDROID_NDK_ROOT\n\
                     Current target: {}\n\
                     Download from: https://developer.android.com/ndk/downloads",
                    target_triple
                );
            });

        // Get Android API level
        let android_api = env::var("ANDROID_API_LEVEL")
            .or_else(|_| env::var("ANDROID_PLATFORM").map(|p| p.replace("android-", "")))
            .or_else(|_| env::var("CARGO_NDK_ANDROID_PLATFORM").map(|p| p.replace("android-", "")))
            .unwrap_or_else(|_| "28".to_string());

        // Determine host platform
        let host_tag = if cfg!(target_os = "macos") {
            "darwin-x86_64"
        } else if cfg!(target_os = "linux") {
            "linux-x86_64"
        } else if cfg!(target_os = "windows") {
            "windows-x86_64"
        } else {
            panic!("Unsupported host platform for Android NDK");
        };

        // Map Rust target to Android architecture
        let android_target_prefix = if target_triple.contains("aarch64") {
            "aarch64-linux-android"
        } else if target_triple.contains("armv7") {
            "arm-linux-androideabi"
        } else if target_triple.contains("x86_64") {
            "x86_64-linux-android"
        } else if target_triple.contains("i686") {
            "i686-linux-android"
        } else {
            panic!("Unsupported Android target: {}", target_triple);
        };

        // Setup Android toolchain paths
        let toolchain_path = format!("{}/toolchains/llvm/prebuilt/{}", android_ndk, host_tag);
        let sysroot = format!("{}/sysroot", toolchain_path);

        // Validate toolchain existence
        if !std::path::Path::new(&toolchain_path).exists() {
            panic!(
                "Android NDK toolchain not found at: {}\n\
                 Please ensure you have the correct Android NDK for your platform.",
                toolchain_path
            );
        }

        // Find clang builtin includes
        let clang_builtin_includes = {
            let clang_lib_path = format!("{}/lib/clang", toolchain_path);
            std::fs::read_dir(&clang_lib_path).ok().and_then(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .find(|entry| {
                        entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                            && entry
                                .file_name()
                                .to_str()
                                .map(|name| name.chars().next().unwrap_or('0').is_ascii_digit())
                                .unwrap_or(false)
                    })
                    .and_then(|entry| {
                        let include_path =
                            format!("{}/{}/include", clang_lib_path, entry.file_name().to_str()?);
                        if std::path::Path::new(&include_path).exists() {
                            Some(include_path)
                        } else {
                            None
                        }
                    })
            })
        };

        // Configure bindgen for Android
        bindings_builder = bindings_builder
            .clang_arg(format!("--sysroot={}", sysroot))
            .clang_arg(format!("-D__ANDROID_API__={}", android_api))
            .clang_arg("-D__ANDROID__");

        // Add include paths in correct order
        if let Some(ref builtin_includes) = clang_builtin_includes {
            bindings_builder = bindings_builder
                .clang_arg("-isystem")
                .clang_arg(builtin_includes);
        }

        bindings_builder = bindings_builder
            .clang_arg("-isystem")
            .clang_arg(format!("{}/usr/include/{}", sysroot, android_target_prefix))
            .clang_arg("-isystem")
            .clang_arg(format!("{}/usr/include", sysroot))
            .clang_arg("-include")
            .clang_arg("stdbool.h")
            .clang_arg("-include")
            .clang_arg("stdint.h");

        // Set additional clang args for cargo ndk compatibility
        if env::var("CARGO_SUBCOMMAND").as_deref() == Ok("ndk") {
            std::env::set_var(
                "BINDGEN_EXTRA_CLANG_ARGS",
                format!("--target={}", target_triple),
            );
        }
    }

    // Configure wasm-unknown / wasi-sdk bindgen settings: target wasm32-wasip1
    // (where wasi-libc lives) and point clang at the wasi-sysroot for headers.
    if matches!(target_os, TargetOs::WasmUnknown) {
        let sdk = detect_wasi_sdk_root();
        let sysroot = format!("{sdk}/share/wasi-sysroot");
        bindings_builder = bindings_builder
            .clang_arg(format!("--sysroot={sysroot}"))
            .clang_arg("--target=wasm32-wasip1")
            // The wasm32 clang backend defaults to hidden visibility, causing
            // bindgen to skip all function declarations. Override to default.
            // See: https://github.com/rust-lang/rust-bindgen/issues/1941
            .clang_arg("-fvisibility=default");
    }

    // Fix bindgen header discovery on Windows MSVC
    // Use cc crate to discover MSVC include paths by compiling a dummy file
    if matches!(target_os, TargetOs::Windows(WindowsVariant::Msvc)) {
        // Create a minimal dummy C file to extract compiler flags
        let out_dir = env::var("OUT_DIR").unwrap();
        let dummy_c = Path::new(&out_dir).join("dummy.c");
        std::fs::write(&dummy_c, "int main() { return 0; }").unwrap();

        // Use cc crate to get compiler with proper environment setup
        let mut build = cc::Build::new();
        build.file(&dummy_c);

        // Get the actual compiler command cc would use
        let compiler = build.try_get_compiler().unwrap();

        // Extract include paths by checking compiler's environment
        // cc crate sets up MSVC environment internally
        let env_include = compiler
            .env()
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("INCLUDE"))
            .map(|(_, v)| v);

        if let Some(include_paths) = env_include {
            for include_path in include_paths
                .to_string_lossy()
                .split(';')
                .filter(|s| !s.is_empty())
            {
                bindings_builder = bindings_builder
                    .clang_arg("-isystem")
                    .clang_arg(include_path);
                debug_log!("Added MSVC include path: {}", include_path);
            }
        }

        // Add MSVC compatibility flags
        bindings_builder = bindings_builder
            .clang_arg(format!("--target={}", target_triple))
            .clang_arg("-fms-compatibility")
            .clang_arg("-fms-extensions");

        debug_log!(
            "Configured bindgen with MSVC toolchain for target: {}",
            target_triple
        );
    }
    let bindings = bindings_builder
        .generate()
        .expect("Failed to generate bindings");

    // Write the generated bindings to an output file
    let bindings_path = out_dir.join("bindings.rs");
    bindings
        .write_to_file(bindings_path)
        .expect("Failed to write bindings");

    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=wrapper_common.h");
    println!("cargo:rerun-if-changed=wrapper_common.cpp");
    println!("cargo:rerun-if-changed=wrapper_oai.h");
    println!("cargo:rerun-if-changed=wrapper_oai.cpp");
    println!("cargo:rerun-if-changed=wrapper_utils.h");
    println!("cargo:rerun-if-changed=wrapper_mtmd.h");

    debug_log!("Bindings Created");

    let mut common_wrapper_build = cc::Build::new();
    common_wrapper_build
        .cpp(true)
        .file("wrapper_common.cpp")
        .file("wrapper_oai.cpp")
        .include(&llama_src)
        .include(llama_src.join("common"))
        .include(llama_src.join("include"))
        .include(llama_src.join("ggml/include"))
        .include(llama_src.join("vendor"))
        .flag_if_supported("-std=c++17")
        .pic(true);

    if matches!(target_os, TargetOs::Windows(WindowsVariant::Msvc)) {
        common_wrapper_build.flag("/std:c++17");
    }

    // When static-stdcxx is enabled on Android, suppress the cc crate's automatic
    // C++ stdlib linking (which defaults to c++_shared) so we can link c++_static instead.
    if matches!(target_os, TargetOs::Android) && cfg!(feature = "static-stdcxx") {
        common_wrapper_build.cpp_link_stdlib(None);
    }

    if matches!(target_os, TargetOs::WasmUnknown) {
        configure_wasm_unknown_cc(&mut common_wrapper_build);
    }

    common_wrapper_build.compile("llama_cpp_sys_2_common_wrapper");

    // Build with Cmake

    let mut config = Config::new(&llama_src);

    // Would require extra source files to pointlessly
    // be included in what's uploaded to and downloaded from
    // crates.io, so deactivating these instead
    config.define("LLAMA_BUILD_TESTS", "OFF");
    config.define("LLAMA_BUILD_EXAMPLES", "OFF");
    config.define("LLAMA_BUILD_SERVER", "OFF");
    config.define("LLAMA_BUILD_TOOLS", "OFF");
    config.define("LLAMA_BUILD_COMMON", "ON");
    config.define("LLAMA_CURL", "OFF");

    // Pass CMAKE_ environment variables down to CMake
    for (key, value) in env::vars() {
        if key.starts_with("CMAKE_") {
            config.define(&key, &value);
        }
    }

    // extract the target-cpu config value, if specified
    let target_cpu = std::env::var("CARGO_ENCODED_RUSTFLAGS")
        .ok()
        .and_then(|rustflags| {
            rustflags
                .split('\x1f')
                .find(|f| f.contains("target-cpu="))
                .and_then(|f| f.split("target-cpu=").nth(1))
                .map(|s| s.to_string())
        });

    // wasm targets don't use -march or x86/ARM feature flags — they have their
    // own SIMD model (wasm SIMD128 if enabled, scalar otherwise).
    if matches!(target_os, TargetOs::WasmUnknown) {
        config.define("GGML_NATIVE", "OFF");
    } else if target_cpu == Some("native".into()) {
        debug_log!("Detected target-cpu=native, compiling with GGML_NATIVE");
        config.define("GGML_NATIVE", "ON");
    }
    // if native isn't specified, enable specific features for ggml instead
    else {
        // rust code isn't using `target-cpu=native`, so llama.cpp shouldn't use GGML_NATIVE either
        config.define("GGML_NATIVE", "OFF");

        // if `target-cpu` is set set, also set -march for llama.cpp to the same value
        if let Some(ref cpu) = target_cpu {
            debug_log!("Setting baseline architecture: -march={}", cpu);
            config.cflag(format!("-march={}", cpu));
            config.cxxflag(format!("-march={}", cpu));
        }

        // I expect this env var to always be present
        let features = std::env::var("CARGO_CFG_TARGET_FEATURE")
            .expect("Env var CARGO_CFG_TARGET_FEATURE not found.");
        debug_log!("Compiling with target features: {}", features);

        // list of rust target_features here:
        //   https://doc.rust-lang.org/reference/attributes/codegen.html#the-target_feature-attribute
        // GGML config flags have been found by looking at:
        //   llama.cpp/ggml/src/ggml-cpu/CMakeLists.txt
        for feature in features.split(',') {
            match feature {
                "avx" => {
                    config.define("GGML_AVX", "ON");
                }
                "avx2" => {
                    config.define("GGML_AVX2", "ON");
                }
                "avx512bf16" => {
                    config.define("GGML_AVX512_BF16", "ON");
                }
                "avx512vbmi" => {
                    config.define("GGML_AVX512_VBMI", "ON");
                }
                "avx512vnni" => {
                    config.define("GGML_AVX512_VNNI", "ON");
                }
                "avxvnni" => {
                    config.define("GGML_AVX_VNNI", "ON");
                }
                "bmi2" => {
                    config.define("GGML_BMI2", "ON");
                }
                "f16c" => {
                    config.define("GGML_F16C", "ON");
                }
                "fma" => {
                    config.define("GGML_FMA", "ON");
                }
                "sse4.2" => {
                    config.define("GGML_SSE42", "ON");
                }
                _ => {
                    debug_log!(
                        "Unrecognized cpu feature: '{}' - skipping GGML config for it.",
                        feature
                    );
                    continue;
                }
            };
        }
    }

    config.define(
        "BUILD_SHARED_LIBS",
        if build_shared_libs { "ON" } else { "OFF" },
    );

    if matches!(target_os, TargetOs::Apple(_)) {
        config.define("GGML_BLAS", "OFF");
    }

    if (matches!(target_os, TargetOs::Windows(WindowsVariant::Msvc))
        && matches!(
            profile.as_str(),
            "Release" | "RelWithDebInfo" | "MinSizeRel"
        ))
    {
        // Debug Rust builds under MSVC turn off optimization even though we're ideally building the release profile of llama.cpp.
        // Looks like an upstream bug:
        // https://github.com/rust-lang/cmake-rs/issues/240
        // For now explicitly reinject the optimization flags that a CMake Release build is expected to have on in this scenario.
        // This fixes CPU inference performance when part of a Rust debug build.
        for flag in &["/O2", "/DNDEBUG", "/Ob2"] {
            config.cflag(flag);
            config.cxxflag(flag);
        }
    }

    config.static_crt(static_crt);

    if matches!(target_os, TargetOs::Android) {
        if cfg!(feature = "shared-stdcxx") && cfg!(feature = "static-stdcxx") {
            panic!("Features 'shared-stdcxx' and 'static-stdcxx' are mutually exclusive");
        }

        // Android NDK Build Configuration
        let android_ndk = env::var("ANDROID_NDK")
            .or_else(|_| env::var("NDK_ROOT"))
            .or_else(|_| env::var("ANDROID_NDK_ROOT"))
            .unwrap_or_else(|_| {
                panic!(
                    "Android NDK not found. Please set one of: ANDROID_NDK, NDK_ROOT, ANDROID_NDK_ROOT\n\
                     Download from: https://developer.android.com/ndk/downloads"
                );
            });

        // Validate NDK installation
        if let Err(error) = validate_android_ndk(&android_ndk) {
            panic!("Android NDK validation failed: {}", error);
        }

        // Rerun build script if NDK environment variables change
        println!("cargo:rerun-if-env-changed=ANDROID_NDK");
        println!("cargo:rerun-if-env-changed=NDK_ROOT");
        println!("cargo:rerun-if-env-changed=ANDROID_NDK_ROOT");

        // Set CMake toolchain file for Android
        let toolchain_file = format!("{}/build/cmake/android.toolchain.cmake", android_ndk);
        config.define("CMAKE_TOOLCHAIN_FILE", &toolchain_file);

        // Configure Android platform (API level)
        let android_platform = env::var("ANDROID_PLATFORM").unwrap_or_else(|_| {
            env::var("ANDROID_API_LEVEL")
                .map(|level| format!("android-{}", level))
                .unwrap_or_else(|_| "android-28".to_string())
        });

        println!("cargo:rerun-if-env-changed=ANDROID_PLATFORM");
        println!("cargo:rerun-if-env-changed=ANDROID_API_LEVEL");
        config.define("ANDROID_PLATFORM", &android_platform);

        // Map Rust target to Android ABI
        let android_abi = if target_triple.contains("aarch64") {
            "arm64-v8a"
        } else if target_triple.contains("armv7") {
            "armeabi-v7a"
        } else if target_triple.contains("x86_64") {
            "x86_64"
        } else if target_triple.contains("i686") {
            "x86"
        } else {
            panic!(
                "Unsupported Android target: {}\n\
                 Supported targets: aarch64-linux-android, armv7-linux-androideabi, i686-linux-android, x86_64-linux-android",
                target_triple
            );
        };

        config.define("ANDROID_ABI", android_abi);

        // Configure C++ standard library linkage for Android.
        // By default, the NDK toolchain uses c++_shared.
        // The shared-stdcxx and static-stdcxx features allow explicit control.
        if cfg!(feature = "static-stdcxx") {
            config.define("ANDROID_STL", "c++_static");
        } else if cfg!(feature = "shared-stdcxx") {
            config.define("ANDROID_STL", "c++_shared");
        }

        // Configure architecture-specific compiler flags
        match android_abi {
            "arm64-v8a" => {
                config.cflag("-march=armv8-a");
                config.cxxflag("-march=armv8-a");
            }
            "armeabi-v7a" => {
                config.cflag("-march=armv7-a");
                config.cxxflag("-march=armv7-a");
                config.cflag("-mfpu=neon");
                config.cxxflag("-mfpu=neon");
                config.cflag("-mthumb");
                config.cxxflag("-mthumb");
            }
            "x86_64" => {
                config.cflag("-march=x86-64");
                config.cxxflag("-march=x86-64");
            }
            "x86" => {
                config.cflag("-march=i686");
                config.cxxflag("-march=i686");
            }
            _ => {}
        }

        // Android-specific CMake configurations
        config.define("GGML_LLAMAFILE", "OFF");

        // Link Android system libraries
        println!("cargo:rustc-link-lib=log");
        println!("cargo:rustc-link-lib=android");
    }

    if matches!(target_os, TargetOs::WasmUnknown) {
        println!("cargo:rerun-if-env-changed=WASI_SDK_PATH");

        // Patch llama.cpp to drop the cpp-httplib link/subdir for wasm:
        // cpp-httplib requires <net/if.h>, signals, and other POSIX networking
        // that wasi-libc deliberately doesn't ship. llama.cpp links it from
        // `common/` unconditionally, so without this patch the build fails
        // long before our code ever runs.
        //
        // Idempotent: each replace is a no-op if the file's already patched
        // (e.g. on a second rebuild that didn't bust cargo's git checkout).
        patch_out_cpp_httplib(&llama_src);

        let sdk = detect_wasi_sdk_root();
        let sysroot = format!("{sdk}/share/wasi-sysroot");
        let clang = format!("{sdk}/bin/clang");
        let clangpp = format!("{sdk}/bin/clang++");
        let ar = format!("{sdk}/bin/llvm-ar");
        let ranlib = format!("{sdk}/bin/llvm-ranlib");

        // Don't use wasi-sdk's bundled cmake toolchain file — it hardcodes the
        // target as `wasm32-wasi`, which differs from our `wasm32-wasip1`. Set
        // the system + compilers directly instead.
        config.define("CMAKE_SYSTEM_NAME", "WASI");
        config.define("CMAKE_SYSTEM_VERSION", "1");
        config.define("CMAKE_SYSTEM_PROCESSOR", "wasm32");
        config.define("CMAKE_C_COMPILER", &clang);
        config.define("CMAKE_CXX_COMPILER", &clangpp);
        config.define("CMAKE_AR", &ar);
        config.define("CMAKE_RANLIB", &ranlib);
        config.define("CMAKE_C_COMPILER_TARGET", "wasm32-wasip1");
        config.define("CMAKE_CXX_COMPILER_TARGET", "wasm32-wasip1");
        config.define("CMAKE_SYSROOT", &sysroot);
        // wasi-sdk's `find_root_path` controls — match the canonical toolchain
        // so cmake's find_* commands look only in the sysroot.
        config.define("CMAKE_FIND_ROOT_PATH_MODE_PROGRAM", "NEVER");
        config.define("CMAKE_FIND_ROOT_PATH_MODE_LIBRARY", "ONLY");
        config.define("CMAKE_FIND_ROOT_PATH_MODE_INCLUDE", "ONLY");
        config.define("CMAKE_FIND_ROOT_PATH_MODE_PACKAGE", "ONLY");

        // Disable everything that's not wasm32: GPU backends, shared libs,
        // memory64, threading-heavy CPU features.
        config.define("BUILD_SHARED_LIBS", "OFF");
        config.define("GGML_VULKAN", "OFF");
        config.define("GGML_CUDA", "OFF");
        config.define("GGML_HIP", "OFF");
        config.define("GGML_OPENCL", "OFF");
        config.define("GGML_SYCL", "OFF");
        config.define("GGML_KOMPUTE", "OFF");
        config.define("GGML_RPC", "OFF");
        config.define("GGML_METAL", "OFF");
        config.define("GGML_ACCELERATE", "OFF");
        config.define("GGML_LLAMAFILE", "OFF");
        config.define("GGML_OPENMP", "OFF");
        config.define("GGML_CPU_HBM", "OFF");
        config.define("GGML_CPU", "ON");
        config.define("LLAMA_WASM_MEM64", "OFF");

        // Disable everything that pulls in network/socket/threading code:
        // wasi-libc has no real socket support, no pthread support without
        // wasi-threads (which we're not using), and cpp-httplib (vendored
        // under llama.cpp) doesn't compile against wasi-libc anyway.
        config.define("LLAMA_CURL", "OFF");
        config.define("LLAMA_BUILD_SERVER", "OFF");
        config.define("LLAMA_BUILD_TESTS", "OFF");
        config.define("LLAMA_BUILD_EXAMPLES", "OFF");
        // mmap requires fdatasync/PROT_* mode flags wasi-libc partly lacks.
        // load_from_buffer (the wasm path) uses fmemopen + fread instead.
        config.define("LLAMA_MMAP", "OFF");

        // C/CXX flags: wasm exceptions + PIC. We don't need to pass
        // --target/--sysroot here because CMAKE_C_COMPILER_TARGET and
        // CMAKE_SYSROOT above already cover that.
        //
        // -D_WASI_EMULATED_SIGNAL: llama.cpp's threading/utilities include
        // <signal.h>, which wasi-libc gates behind this feature flag. The
        // emulation is minimal (raise/signal as best-effort no-ops). The
        // matching link is `-lwasi-emulated-signal` added via rustc-link-arg
        // below.
        config.define(
            "CMAKE_C_FLAGS",
            "-fexceptions -fPIC -msimd128 -D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PROCESS_CLOCKS -DGGML_USE_LLAMAFILE=0",
        );
        config.define(
            "CMAKE_CXX_FLAGS",
            "-fexceptions -fPIC -msimd128 -D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PROCESS_CLOCKS -DGGML_USE_LLAMAFILE=0",
        );
        config.define("CMAKE_POSITION_INDEPENDENT_CODE", "ON");
    }

    if matches!(target_os, TargetOs::Linux)
        && target_triple.contains("aarch64")
        && target_cpu != Some("native".into())
    {
        // If the target-cpu is not specified as native, we take off the native ARM64 support.
        // It is useful in docker environments where the native feature is not enabled.
        config.define("GGML_NATIVE", "OFF");
        config.define("GGML_CPU_ARM_ARCH", "armv8-a");
    }

    // watchOS does not support Metal — override CMake's `if (APPLE)` auto-detection.
    // Accelerate/BLAS is available on watchOS so we leave that enabled.
    // Also define _DARWIN_C_SOURCE so that BSD types (u_int, u_char, u_short) are available
    // from <sys/types.h>. These types are guarded by:
    //   #if !defined(_POSIX_C_SOURCE) || defined(_DARWIN_C_SOURCE)
    // On macOS/iOS _DARWIN_C_SOURCE is set implicitly, but not on watchOS.
    // Ref: https://github.com/apple/darwin-xnu/blob/main/bsd/sys/types.h
    if matches!(target_os, TargetOs::Apple(AppleVariant::WatchOS)) {
        config.define("GGML_METAL", "OFF");
        config.cflag("-D_DARWIN_C_SOURCE");
        config.cxxflag("-D_DARWIN_C_SOURCE");
    }

    if cfg!(feature = "vulkan") {
        config.define("GGML_VULKAN", "ON");
        match target_os {
            TargetOs::Windows(_) => {
                let vulkan_path = env::var("VULKAN_SDK").expect(
                    "Please install Vulkan SDK and ensure that VULKAN_SDK env variable is set",
                );
                let vulkan_lib_path = Path::new(&vulkan_path).join("Lib");
                println!("cargo:rustc-link-search={}", vulkan_lib_path.display());
                println!("cargo:rustc-link-lib=vulkan-1");

                // workaround for this error: "FileTracker : error FTK1011: could not create the new file tracking log file"
                // it has to do with MSBuild FileTracker not respecting the path
                // limit configuration set in the windows registry.
                // I'm not sure why that's a thing, but this makes my builds work.
                // (crates that depend on llama-cpp-rs w/ vulkan easily exceed the default PATH_MAX on windows)
                env::set_var("TrackFileAccess", "false");
                // since we disabled TrackFileAccess, we can now run into problems with parallel
                // access to pdb files. /FS solves this.
                config.cflag("/FS");
                config.cxxflag("/FS");
            }
            TargetOs::Linux => {
                // If we are not using system provided vulkan SDK, add vulkan libs for linking
                if let Ok(vulkan_path) = env::var("VULKAN_SDK") {
                    let vulkan_lib_path = Path::new(&vulkan_path).join("lib");
                    println!("cargo:rustc-link-search={}", vulkan_lib_path.display());
                }
                println!("cargo:rustc-link-lib=vulkan");
            }
            _ => (),
        }
    }

    if cfg!(feature = "cuda") {
        config.define("GGML_CUDA", "ON");

        if cfg!(feature = "cuda-no-vmm") {
            config.define("GGML_CUDA_NO_VMM", "ON");
        }
    }

    if cfg!(feature = "rocm") {
        config.define("GGML_HIP", "ON");
    }

    // Android doesn't have OpenMP support AFAICT and openmp is a default feature. Do this here
    // rather than modifying the defaults in Cargo.toml just in case someone enables the OpenMP feature
    // and tries to build for Android anyway.
    if cfg!(feature = "openmp")
        && !matches!(target_os, TargetOs::Android | TargetOs::WasmUnknown)
    {
        config.define("GGML_OPENMP", "ON");
    } else {
        config.define("GGML_OPENMP", "OFF");
    }

    if cfg!(feature = "system-ggml") {
        config.define("LLAMA_USE_SYSTEM_GGML", "ON");
    }

    if cfg!(feature = "dynamic-backends") {
        // Pre-create the backends directory so CMake can install MODULE libs there.
        // GGML_BACKEND_DIR causes backends to install to this known path instead of
        // CMAKE_INSTALL_BINDIR, making them easy to locate in downstream build scripts.
        let backends_dir = out_dir.join("backends");
        std::fs::create_dir_all(&backends_dir).unwrap();
        config.define("GGML_BACKEND_DL", "ON");
        config.define("GGML_CPU_ALL_VARIANTS", "ON");
        config.define("GGML_BACKEND_DIR", backends_dir.to_str().unwrap());
        // BUILD_SHARED_LIBS=ON is already set above via the dynamic-link feature.
    }

    // General
    config
        .profile(&profile)
        .very_verbose(std::env::var("CMAKE_VERBOSE").is_ok()) // Not verbose by default
        .always_configure(false);

    let build_dir = config.build();

    if cfg!(feature = "dynamic-backends") {
        println!("cargo:backends_dir={}", out_dir.join("backends").display());
    }

    // Build mtmd directly with cc::Build, bypassing the cmake tools build.
    // Using LLAMA_BUILD_TOOLS=ON would pull in all tools (batched-bench, quantize, etc.)
    // and their CMakeLists.txt files, which are not included in the crate package.
    //
    // Skipped on wasm32-unknown-unknown — mtmd pulls in miniaudio which uses
    // pthread sched APIs wasi-libc doesn't ship; see the matching bindgen
    // gate above.
    if cfg!(feature = "mtmd") && !matches!(target_os, TargetOs::WasmUnknown) {
        let mtmd_src = llama_src.join("tools/mtmd");
        let mut mtmd_build = cc::Build::new();
        mtmd_build
            .cpp(true)
            .include(&mtmd_src)
            .include(&llama_src)
            .include(llama_src.join("include"))
            .include(llama_src.join("ggml/include"))
            .include(llama_src.join("common"))
            .include(llama_src.join("vendor"))
            .flag_if_supported("-std=c++17")
            .flag_if_supported("-Wno-cast-qual")
            .pic(true);

        if matches!(target_os, TargetOs::Windows(WindowsVariant::Msvc)) {
            mtmd_build.flag("/std:c++17");
        }

        // When static-stdcxx is enabled on Android, suppress the cc crate's automatic
        // C++ stdlib linking (which defaults to c++_shared) so we can link c++_static instead.
        if matches!(target_os, TargetOs::Android) && cfg!(feature = "static-stdcxx") {
            mtmd_build.cpp_link_stdlib(None);
        }

        if matches!(target_os, TargetOs::WasmUnknown) {
            configure_wasm_unknown_cc(&mut mtmd_build);
        }

        // Collect all .cpp files in tools/mtmd and its subdirectories
        for entry in glob(mtmd_src.join("**/*.cpp").to_str().unwrap()).unwrap() {
            match entry {
                Ok(path) => {
                    // Skip CLI / deprecation-warning binaries — we only want the library sources
                    let filename = path.file_name().unwrap().to_str().unwrap();
                    if filename == "mtmd-cli.cpp" || filename == "deprecation-warning.cpp" {
                        continue;
                    }
                    mtmd_build.file(&path);
                }
                Err(e) => println!("cargo:warning=mtmd glob error: {}", e),
            }
        }

        mtmd_build.compile("mtmd");
    }

    // Search paths
    println!("cargo:rustc-link-search={}", out_dir.join("lib").display());
    println!(
        "cargo:rustc-link-search={}",
        out_dir.join("lib64").display()
    );
    println!("cargo:rustc-link-search={}", build_dir.display());

    if cfg!(feature = "system-ggml") {
        // Extract library directory from CMake's found GGML package
        let cmake_cache = build_dir.join("build").join("CMakeCache.txt");
        if let Ok(cache_contents) = std::fs::read_to_string(&cmake_cache) {
            let mut ggml_lib_dirs = std::collections::HashSet::new();

            // Parse CMakeCache.txt to find where GGML libraries were found
            for line in cache_contents.lines() {
                if line.starts_with("GGML_LIBRARY:")
                    || line.starts_with("GGML_BASE_LIBRARY:")
                    || line.starts_with("GGML_CPU_LIBRARY:")
                {
                    if let Some(lib_path) = line.split('=').nth(1) {
                        if let Some(parent) = Path::new(lib_path).parent() {
                            ggml_lib_dirs.insert(parent.to_path_buf());
                        }
                    }
                }
            }

            // Add each unique library directory to the search path
            for lib_dir in ggml_lib_dirs {
                println!("cargo:rustc-link-search=native={}", lib_dir.display());
                debug_log!("Added system GGML library path: {}", lib_dir.display());
            }
        }
    }

    if cfg!(feature = "cuda") && !build_shared_libs {
        // Re-run build script if CUDA_PATH environment variable changes
        println!("cargo:rerun-if-env-changed=CUDA_PATH");

        // Add CUDA library directories to the linker search path
        for lib_dir in find_cuda_helper::find_cuda_lib_dirs() {
            println!("cargo:rustc-link-search=native={}", lib_dir.display());
        }

        // Platform-specific linking
        if cfg!(target_os = "windows") {
            // ✅ On Windows, use dynamic linking.
            // Static linking is problematic because NVIDIA does not provide culibos.lib,
            // and static CUDA libraries (like cublas_static.lib) are usually not shipped.

            println!("cargo:rustc-link-lib=cudart"); // Links to cudart64_*.dll
            println!("cargo:rustc-link-lib=cublas"); // Links to cublas64_*.dll
            println!("cargo:rustc-link-lib=cublasLt"); // Links to cublasLt64_*.dll

            // Link to CUDA driver API (nvcuda.dll via cuda.lib)
            if !cfg!(feature = "cuda-no-vmm") {
                println!("cargo:rustc-link-lib=cuda");
            }
        } else {
            // ✅ On non-Windows platforms (e.g., Linux), static linking is preferred and supported.
            // Static libraries like cudart_static and cublas_static depend on culibos.

            println!("cargo:rustc-link-lib=static=cudart_static");
            println!("cargo:rustc-link-lib=static=cublas_static");
            println!("cargo:rustc-link-lib=static=cublasLt_static");

            // Link to CUDA driver API (libcuda.so)
            if !cfg!(feature = "cuda-no-vmm") {
                println!("cargo:rustc-link-lib=cuda");
            }

            // culibos is required when statically linking cudart_static
            println!("cargo:rustc-link-lib=static=culibos");
        }
    }

    if cfg!(feature = "rocm") && !build_shared_libs {
        // Re-run build script if ROCM_PATH environment variable changes
        println!("cargo:rerun-if-env-changed=ROCM_PATH");
        println!("cargo:rerun-if-env-changed=HIP_PATH");

        // Find ROCm installation
        let rocm_path = env::var("ROCM_PATH")
            .or_else(|_| env::var("HIP_PATH"))
            .unwrap_or_else(|_| {
                if cfg!(target_os = "windows") {
                    "C:\\Program Files\\AMD\\ROCm".to_string()
                } else {
                    "/opt/rocm".to_string()
                }
            });

        let rocm_lib = Path::new(&rocm_path).join("lib");
        if !rocm_lib.exists() {
            panic!(
                "ROCm libraries not found at: {}\n\
                 Please install ROCm or set ROCM_PATH/HIP_PATH environment variable.\n\
                 Download from: https://rocm.docs.amd.com/",
                rocm_lib.display()
            );
        }

        println!("cargo:rustc-link-search=native={}", rocm_lib.display());

        // Link ROCm libraries
        println!("cargo:rustc-link-lib=dylib=amdhip64");
        println!("cargo:rustc-link-lib=dylib=rocblas");
        println!("cargo:rustc-link-lib=dylib=hipblas");
    }

    // Link libraries
    let llama_libs_kind = if build_shared_libs
        || (cfg!(feature = "system-ggml") && !cfg!(feature = "system-ggml-static"))
    {
        "dylib"
    } else {
        "static"
    };

    let llama_libs = extract_lib_names(&out_dir, build_shared_libs, &target_os);

    assert_ne!(llama_libs.len(), 0);

    let common_lib_dir = out_dir.join("build").join("common");
    if common_lib_dir.is_dir() {
        println!(
            "cargo:rustc-link-search=native={}",
            common_lib_dir.display()
        );
        let common_profile_dir = common_lib_dir.join(&profile);
        if common_profile_dir.is_dir() {
            println!(
                "cargo:rustc-link-search=native={}",
                common_profile_dir.display()
            );
        }
        println!("cargo:rustc-link-lib=static=common");
    }

    if cfg!(feature = "system-ggml") {
        println!("cargo:rustc-link-lib={llama_libs_kind}=ggml");
        println!("cargo:rustc-link-lib={llama_libs_kind}=ggml-base");
        println!("cargo:rustc-link-lib={llama_libs_kind}=ggml-cpu");
    }
    for lib in llama_libs {
        let link = format!("cargo:rustc-link-lib={}={}", llama_libs_kind, lib);
        debug_log!("LINK {link}",);
        println!("{link}",);
    }

    // OpenMP
    if cfg!(feature = "openmp") && target_triple.contains("gnu") {
        println!("cargo:rustc-link-lib=gomp");
    }

    match target_os {
        TargetOs::Windows(WindowsVariant::Msvc) => {
            println!("cargo:rustc-link-lib=advapi32");
            let crt_static = env::var("CARGO_CFG_TARGET_FEATURE")
                .unwrap_or_default()
                .contains("crt-static");
            if cfg!(debug_assertions) {
                if crt_static {
                    println!("cargo:rustc-link-lib=libcmtd");
                } else {
                    println!("cargo:rustc-link-lib=dylib=msvcrtd");
                }
            }
        }
        TargetOs::Linux => {
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
        TargetOs::Apple(ref variant) => {
            println!("cargo:rustc-link-lib=framework=Foundation");
            println!("cargo:rustc-link-lib=framework=Accelerate");
            if !matches!(variant, AppleVariant::WatchOS) {
                println!("cargo:rustc-link-lib=framework=Metal");
                println!("cargo:rustc-link-lib=framework=MetalKit");
            }
            println!("cargo:rustc-link-lib=c++");

            match variant {
                AppleVariant::MacOS => {
                    // On (older) OSX we need to link against the clang runtime,
                    // which is hidden in some non-default path.
                    //
                    // More details at https://github.com/alexcrichton/curl-rust/issues/279.
                    if let Some(path) = macos_link_search_path() {
                        println!("cargo:rustc-link-lib=clang_rt.osx");
                        println!("cargo:rustc-link-search={}", path);
                    }
                }
                AppleVariant::WatchOS | AppleVariant::Other => (),
            }
        }
        TargetOs::Android => {
            if cfg!(feature = "static-stdcxx") {
                println!("cargo:rustc-link-lib=c++_static");
                println!("cargo:rustc-link-lib=c++abi");
            } else if cfg!(feature = "shared-stdcxx") {
                println!("cargo:rustc-link-lib=c++_shared");
            }
            // When neither feature is set, the cc crate handles C++ stdlib
            // linking automatically (defaults to c++_shared on Android).
        }
        TargetOs::WasmUnknown => {
            // Tell wasm-ld where wasi-sdk's libraries live. libc / libm /
            // wasi-emulated-* are in `lib/wasm32-wasip1/`. libc++/libc++abi
            // are in subdirs segregated by exception-handling model; we
            // compile with `-fwasm-exceptions` so we want the `eh/` variant.
            let sdk = detect_wasi_sdk_root();
            println!(
                "cargo:rustc-link-search=native={sdk}/share/wasi-sysroot/lib/wasm32-wasip1"
            );
            println!(
                "cargo:rustc-link-search=native={sdk}/share/wasi-sysroot/lib/wasm32-wasip1/eh"
            );

            // wasi-sdk's compiler-rt (libclang_rt.builtins.a — provides
            // __fixdfti, __floattidf, etc.). Glob the clang/* version dir
            // because the version number bumps with each wasi-sdk release.
            for entry in glob(&format!("{sdk}/lib/clang/*/lib/wasm32-unknown-wasip1"))
                .unwrap()
                .flatten()
            {
                println!("cargo:rustc-link-search=native={}", entry.display());
            }

            // The Rust target is wasm32-unknown-unknown so rustc DOESN'T
            // auto-link any libc. The C/C++ side was compiled against
            // wasi-libc headers/ABI though, so it pulls in libc / libm /
            // libc++ symbols. Link them explicitly here so the final wasm
            // doesn't have ~150 undefined `env.{malloc,free,fmemopen,...}`
            // imports. Order matters: libc++abi depends on libc++, libc++
            // depends on libc, etc.
            println!("cargo:rustc-link-lib=static=c++");
            println!("cargo:rustc-link-lib=static=c++abi");
            println!("cargo:rustc-link-lib=static=c");
            println!("cargo:rustc-link-lib=static=wasi-emulated-signal");
            println!("cargo:rustc-link-lib=static=wasi-emulated-process-clocks");
            println!("cargo:rustc-link-lib=static=wasi-emulated-mman");
            println!("cargo:rustc-link-lib=static=wasi-emulated-getpid");
            // Compiler builtins for soft-float / int128 / etc. ops the wasm
            // backend can't lower natively (__fixdfti, __floattidf, ...).
            println!("cargo:rustc-link-lib=static=clang_rt.builtins");

            // Tell wasm-ld to produce a reactor module (long-lived, callable
            // multiple times) rather than a command module (one-shot, runs
            // ctors+dtors around each exported call). Without this, every
            // wasm-bindgen-emitted export wraps its body in
            // __wasm_call_ctors / __wasm_call_dtors, and the dtor pass iterates
            // atexit handlers — at least one of which has a wasm signature
            // that doesn't match how it gets called, producing
            // `RuntimeError: function signature mismatch` on the first export
            // invocation.
            // rustc invokes wasm-ld directly (not through a compiler driver),
            // so this is passed as-is — no `-Wl,` prefix.
            println!("cargo:rustc-link-arg=--mexec-model=reactor");
        }
        _ => (),
    }

    // copy DLLs to target
    if build_shared_libs {
        let libs_assets = extract_lib_assets(&out_dir, &target_os);
        for asset in libs_assets {
            let asset_clone = asset.clone();
            let filename = asset_clone.file_name().unwrap();
            let filename = filename.to_str().unwrap();
            let dst = target_dir.join(filename);
            debug_log!("HARD LINK {} TO {}", asset.display(), dst.display());
            if !dst.exists() {
                std::fs::hard_link(asset.clone(), dst).unwrap();
            }

            // Copy DLLs to examples as well
            if target_dir.join("examples").exists() {
                let dst = target_dir.join("examples").join(filename);
                debug_log!("HARD LINK {} TO {}", asset.display(), dst.display());
                if !dst.exists() {
                    std::fs::hard_link(asset.clone(), dst).unwrap();
                }
            }

            // Copy DLLs to target/profile/deps as well for tests
            let dst = target_dir.join("deps").join(filename);
            debug_log!("HARD LINK {} TO {}", asset.display(), dst.display());
            if !dst.exists() {
                std::fs::hard_link(asset.clone(), dst).unwrap();
            }
        }
    }
}
