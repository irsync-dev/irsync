#![no_main]
use pbwalk::Parser as _;
// Any bytes as a record: the walker gives every byte to exactly one field.
// Any bytes as a capture file, unwrapped and as-is, with and without the
// hex columns: pbwalk must not panic.
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let record = pbwalk::Schema::builtin()
        .message("irsync.capture.v1.Record")
        .unwrap();
    assert_eq!(pbwalk::own_bytes(&pbwalk::walk(&record, data)), data);
    for extra in [
        &[][..],
        &["--no-unwrap"][..],
        &["-C"][..],
        &["-C", "--no-unwrap"][..],
    ] {
        let args = pbwalk::Args::parse_from(["pbwalk", "--full", "x"].iter().chain(extra));
        let _ = pbwalk::run(&args, data, &mut std::io::sink());
    }
});
