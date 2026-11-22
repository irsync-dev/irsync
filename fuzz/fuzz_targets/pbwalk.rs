#![no_main]
use pbwalk::Parser as _;
// Any bytes as a capture file, unwrapped and as-is: pbwalk must not panic.
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    for extra in [&[][..], &["--no-unwrap"][..]] {
        let args = pbwalk::Args::parse_from(["pbwalk", "--full", "x"].iter().chain(extra));
        let _ = pbwalk::run(&args, data, &mut std::io::sink());
    }
});
