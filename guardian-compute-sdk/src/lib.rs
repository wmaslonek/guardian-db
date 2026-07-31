//! # Guardian Compute SDK
//!
//! Guest-side runtime for writing Guardian Compute tasks (see
//! `docs/rfcs/0002-guardian-compute.md` in the guardian-db repository).
//! It hides the raw ABI — the exported `gdb_alloc`, the `(ptr, len) -> i64`
//! entrypoint signature and the packed output — behind one attribute:
//!
//! ```ignore
//! use guardian_compute_sdk::guardian_task;
//!
//! #[guardian_task]
//! fn generate_thumbnail(input: &[u8]) -> Result<Vec<u8>, TaskFailure> {
//!     // ... pure bytes in, bytes out ...
//! #   Ok(input.to_vec())
//! }
//! ```
//!
//! Build with `cargo build --target wasm32-unknown-unknown --release`
//! (`crate-type = ["cdylib"]`), publish the `.wasm` to the blob store, and
//! name the function in `ExecuteRequest::entrypoint` / `TaskSpec::entrypoint`.
//!
//! ## Determinism
//!
//! The default executor sandbox grants no host capabilities: no clock, no
//! randomness, no filesystem. A task that sticks to pure computation is
//! deterministic and therefore eligible for k-of-n redundant execution.
//!
//! ## Errors
//!
//! The wire ABI has no error channel: a failed task is a WASM trap, which the
//! executor reports as `TaskError::Trapped`. Returning `Err` from a task
//! panics with the error message — the run fails cleanly, though the message
//! itself stays on the guest side (host functions may carry it out once
//! granted; see the `gdb.log` capability).

// Lets the macro's absolute `::guardian_compute_sdk::` paths resolve inside
// this crate's own tests (same trick guardian-db uses for the odm derive).
#[cfg(test)]
extern crate self as guardian_compute_sdk;

pub use guardian_compute_sdk_macros::guardian_task;

/// A task-level failure. Any `E: Display` works as a task error type; this
/// one is provided for tasks that don't want to define their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFailure(pub String);

impl TaskFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl core::fmt::Display for TaskFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for TaskFailure {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<&str> for TaskFailure {
    fn from(message: &str) -> Self {
        Self(message.to_string())
    }
}

/// The raw guest side of the Guardian Compute ABI. `#[guardian_task]` expands
/// to calls into this module; it is public so tasks can also be written by
/// hand when the macro does not fit.
pub mod abi {
    /// Allocator export the host calls to place the input bytes
    /// (`gdb_alloc(len) -> ptr` in the RFC's ABI).
    ///
    /// Lives in the SDK so every task gets it for free; `#[unsafe(no_mangle)]`
    /// `extern "C"` symbols in a dependency are exported from the final
    /// `cdylib` wasm module. The buffer is intentionally leaked: the host
    /// writes into it and the instance is discarded after the run (the
    /// executor creates a fresh store per execution).
    ///
    /// Only compiled for wasm targets — on host targets the symbol would be
    /// useless and could collide across crates in one test binary.
    #[cfg(target_arch = "wasm32")]
    #[unsafe(no_mangle)]
    pub extern "C" fn gdb_alloc(len: i32) -> i32 {
        let capacity = usize::try_from(len).unwrap_or(0).max(1);
        let mut buffer = Vec::<u8>::with_capacity(capacity);
        let ptr = buffer.as_mut_ptr();
        core::mem::forget(buffer);
        ptr as usize as i32
    }

    /// Borrows the input the host wrote at `ptr..ptr+len`.
    ///
    /// # Safety
    /// `ptr`/`len` must be the pair the host passed to the entrypoint —
    /// i.e. a live buffer produced by [`gdb_alloc`] in this instance.
    pub unsafe fn input<'a>(ptr: i32, len: i32) -> &'a [u8] {
        let len = usize::try_from(len).unwrap_or(0);
        if len == 0 {
            return &[];
        }
        unsafe { core::slice::from_raw_parts(ptr as usize as *const u8, len) }
    }

    /// Hands `output` to the host: leaks the buffer and packs its location
    /// as `(ptr << 32) | len`, the entrypoint's return value in the ABI.
    ///
    /// Meaningful only inside a wasm32 instance, where linear-memory offsets
    /// fit in 32 bits; on a 64-bit host the pointer would truncate.
    pub fn emit(output: Vec<u8>) -> i64 {
        let mut output = output;
        output.shrink_to_fit();
        let len = output.len() as u32;
        if len == 0 {
            return 0;
        }
        let ptr = output.as_mut_ptr() as usize as u32;
        core::mem::forget(output);
        pack(ptr, len)
    }

    /// Packs an output location as the ABI's `(ptr << 32) | len` return value.
    pub fn pack(ptr: u32, len: u32) -> i64 {
        (i64::from(ptr) << 32) | i64::from(len)
    }

    /// Splits a packed ABI return value back into `(ptr, len)` — the inverse
    /// of [`pack`], and exactly what the executor-side runtime does.
    pub fn unpack(packed: i64) -> (u32, u32) {
        ((packed >> 32) as u32, packed as u32)
    }

    /// What a `#[guardian_task]` function may return. `Err` panics with the
    /// error message, which surfaces to the executor as a trap.
    pub trait IntoTaskOutput {
        fn into_task_output(self) -> Vec<u8>;
    }

    impl IntoTaskOutput for Vec<u8> {
        fn into_task_output(self) -> Vec<u8> {
            self
        }
    }

    impl<E: core::fmt::Display> IntoTaskOutput for Result<Vec<u8>, E> {
        fn into_task_output(self) -> Vec<u8> {
            match self {
                Ok(output) => output,
                Err(error) => panic!("task failed: {error}"),
            }
        }
    }
}

/// Typed task I/O over CBOR (feature `cbor`, RFC 0002 §8.2: "opaque bytes on
/// the protocol, CBOR as the SDK convention"). `#[guardian_task(cbor)]`
/// expands to calls into this module.
#[cfg(feature = "cbor")]
pub mod cbor {
    use serde::Serialize;
    use serde::de::DeserializeOwned;

    /// Decodes the task input. A malformed input panics — a trap on the
    /// executor, which is the correct verdict for garbage in.
    pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> T {
        ciborium::from_reader(bytes).unwrap_or_else(|e| panic!("invalid CBOR input: {e}"))
    }

    /// Encodes a task output value.
    pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
        let mut out = Vec::new();
        ciborium::into_writer(value, &mut out)
            .unwrap_or_else(|e| panic!("CBOR encode failed: {e}"));
        out
    }

    /// Terminal step of a typed task: `Ok` is CBOR-encoded and emitted,
    /// `Err` panics with the message (a trap on the executor).
    pub fn emit_result<T: Serialize, E: core::fmt::Display>(result: Result<T, E>) -> i64 {
        match result {
            Ok(value) => crate::abi::emit(encode(&value)),
            Err(error) => panic!("task failed: {error}"),
        }
    }
}

/// Bindings for the executor's opt-in host capabilities (feature `host`).
///
/// These map 1:1 to the `HostGrants` of the guardian-db runtime: calling them
/// makes the module *import* `gdb.log` / `gdb.store_get`, so it will only
/// instantiate on executors whose owner granted those capabilities — an
/// ungranted executor rejects the task with `HostCapabilityDenied` before any
/// code runs. Modules that never call them import nothing and run anywhere.
///
/// On non-wasm targets (host-side builds of examples/tests) the bindings are
/// inert: `log` is a no-op and `store_get` returns `None`.
#[cfg(feature = "host")]
pub mod host {
    #[cfg(target_arch = "wasm32")]
    #[link(wasm_import_module = "gdb")]
    unsafe extern "C" {
        #[link_name = "log"]
        fn gdb_log(ptr: i32, len: i32);
        #[link_name = "store_get"]
        fn gdb_store_get(key_ptr: i32, key_len: i32, dest_ptr: i32, dest_cap: i32) -> i32;
        #[link_name = "llm_generate"]
        fn gdb_llm_generate(
            model_ptr: i32,
            model_len: i32,
            prompt_ptr: i32,
            prompt_len: i32,
        ) -> i64;
    }

    /// Emits a message into the executor's tracing (capability `gdb.log`).
    pub fn log(message: &str) {
        #[cfg(target_arch = "wasm32")]
        unsafe {
            gdb_log(message.as_ptr() as usize as i32, message.len() as i32);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = message;
    }

    /// Reads a value from the executor's local store (capability
    /// `gdb.store_get`). Returns `None` when the key is absent.
    ///
    /// Implements the ABI's two-call protocol: the host returns the value's
    /// full length, so a first read into a 4 KiB buffer is retried with an
    /// exact-size buffer when the value is larger.
    pub fn store_get(key: &[u8]) -> Option<Vec<u8>> {
        #[cfg(target_arch = "wasm32")]
        {
            let mut buffer = vec![0u8; 4096];
            let full_len = unsafe {
                gdb_store_get(
                    key.as_ptr() as usize as i32,
                    key.len() as i32,
                    buffer.as_mut_ptr() as usize as i32,
                    buffer.len() as i32,
                )
            };
            if full_len < 0 {
                return None;
            }
            let full_len = full_len as usize;
            if full_len <= buffer.len() {
                buffer.truncate(full_len);
                return Some(buffer);
            }
            // Truncated: retry with an exact-size buffer.
            let mut exact = vec![0u8; full_len];
            let second = unsafe {
                gdb_store_get(
                    key.as_ptr() as usize as i32,
                    key.len() as i32,
                    exact.as_mut_ptr() as usize as i32,
                    exact.len() as i32,
                )
            };
            if second < 0 {
                return None;
            }
            exact.truncate(second as usize);
            Some(exact)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = key;
            None
        }
    }

    /// Generates text with an LLM model served by the executor (capability
    /// `gdb.llm_generate`, RFC 0004 phase 5). Buffered: returns once the
    /// generation finishes; `None` on failure (model not served, backend
    /// down, response over the executor's cap budget…).
    ///
    /// Calling this makes the module import `gdb.llm_generate`, so it only
    /// instantiates on executors serving LLM models (`Inference` class).
    /// Generation is non-deterministic — tasks using it are ineligible for
    /// k-of-n redundant execution — and can be slow: size the task's
    /// `ResourceLimits::timeout_ms` to cover it, since the wall clock keeps
    /// ticking during the host call.
    pub fn llm_generate(model: &str, prompt: &str) -> Option<String> {
        #[cfg(target_arch = "wasm32")]
        {
            let packed = unsafe {
                gdb_llm_generate(
                    model.as_ptr() as usize as i32,
                    model.len() as i32,
                    prompt.as_ptr() as usize as i32,
                    prompt.len() as i32,
                )
            };
            if packed < 0 {
                return None;
            }
            let (ptr, len) = crate::abi::unpack(packed);
            if len == 0 {
                return Some(String::new());
            }
            // The host wrote the response into a buffer it obtained from this
            // instance's own `gdb_alloc`; like the input buffer, it lives for
            // the rest of the (single-run) instance.
            let bytes =
                unsafe { core::slice::from_raw_parts(ptr as usize as *const u8, len as usize) };
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (model, prompt);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::abi::{self, IntoTaskOutput};
    use super::*;

    /// The packing scheme is a pure function; the pointer-flavored halves
    /// (`emit`, `input` with real addresses, the generated wrapper) only make
    /// sense inside a wasm32 instance — a 64-bit host pointer does not fit in
    /// the ABI's `i32` — and are covered end to end by the golden wasm test
    /// in the main crate (`tests/compute_sdk.rs`).
    #[test]
    fn pack_and_unpack_roundtrip() {
        for (ptr, len) in [(0u32, 0u32), (4096, 17), (u32::MAX, u32::MAX)] {
            assert_eq!(abi::unpack(abi::pack(ptr, len)), (ptr, len));
        }
    }

    #[test]
    fn empty_output_packs_to_zero_and_reads_back_empty() {
        assert_eq!(abi::emit(Vec::new()), 0);
        assert_eq!(unsafe { abi::input(0, 0) }, b"");
    }

    #[test]
    fn result_output_unwraps_ok_and_panics_on_err() {
        assert_eq!(
            Ok::<_, TaskFailure>(b"ok".to_vec()).into_task_output(),
            b"ok"
        );
        let failure = std::panic::catch_unwind(|| {
            Err::<Vec<u8>, _>(TaskFailure::new("entrada corrompida")).into_task_output()
        });
        assert!(failure.is_err());
    }

    /// Compile-time assertion of the macro expansion: both accepted
    /// signatures produce an ABI-shaped `extern "C" fn(i32, i32) -> i64`
    /// wrapper named `__guardian_task_<name>` and leave the original function
    /// callable. (Calling the wrapper with real pointers is wasm32-only.)
    mod macro_expansion {
        use crate::guardian_task;

        #[guardian_task]
        fn echo(input: &[u8]) -> Vec<u8> {
            input.to_vec()
        }

        #[guardian_task]
        fn checked_upper(input: &[u8]) -> Result<Vec<u8>, crate::TaskFailure> {
            if input.is_empty() {
                return Err(crate::TaskFailure::new("empty input"));
            }
            Ok(input.to_ascii_uppercase())
        }

        #[test]
        fn expansion_produces_abi_wrappers_and_keeps_originals() {
            let _echo_wrapper: extern "C" fn(i32, i32) -> i64 = __guardian_task_echo;
            let _upper_wrapper: extern "C" fn(i32, i32) -> i64 = __guardian_task_checked_upper;
            // Originals stay ordinary Rust functions.
            assert_eq!(echo(b"x"), b"x");
            assert_eq!(checked_upper(b"a").unwrap(), b"A");
        }
    }

    #[cfg(feature = "cbor")]
    mod cbor_layer {
        use crate::guardian_task;

        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct Doc {
            text: String,
        }

        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct Stats {
            words: usize,
        }

        #[guardian_task(cbor)]
        fn word_count(doc: Doc) -> Result<Stats, crate::TaskFailure> {
            Ok(Stats {
                words: doc.text.split_whitespace().count(),
            })
        }

        #[test]
        fn typed_expansion_produces_abi_wrapper_and_keeps_original() {
            let _wrapper: extern "C" fn(i32, i32) -> i64 = __guardian_task_word_count;
            let stats = word_count(Doc {
                text: "um dois tres".into(),
            })
            .unwrap();
            assert_eq!(stats, Stats { words: 3 });
        }

        #[test]
        fn encode_decode_roundtrip() {
            let doc = Doc {
                text: "conteudo".into(),
            };
            let decoded: Doc = crate::cbor::decode(&crate::cbor::encode(&doc));
            assert_eq!(decoded, doc);
        }

        #[test]
        fn malformed_cbor_input_panics() {
            let garbage = std::panic::catch_unwind(|| crate::cbor::decode::<Doc>(b"\xff\xff"));
            assert!(garbage.is_err());
        }
    }
}
