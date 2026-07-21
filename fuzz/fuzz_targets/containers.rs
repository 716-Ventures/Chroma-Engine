#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    chroma_engine::fuzzing::fuzz_containers(data);
});
