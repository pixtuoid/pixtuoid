#![no_main]

use libfuzzer_sys::{fuzz_mutator, fuzz_target, fuzzer_mutate};
use pixtuoid_core::harness::fuzz;

fuzz_target!(|bytes: &[u8]| {
    fuzz::remote(bytes);
});

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    fuzz::json_mutate(data, size, max_size, seed)
        .unwrap_or_else(|| fuzzer_mutate(data, size, max_size))
});
