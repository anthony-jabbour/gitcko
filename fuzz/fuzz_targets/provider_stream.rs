// SPDX-License-Identifier: AGPL-3.0-or-later

//! Provider bytes never panic a decoder, and how they're split into chunks never
//! changes what comes out, error included (TESTING §10, `provider_stream`).

#![no_main]

use gitcko_model::Event;
use gitcko_model::stream::Decode;
use libfuzzer_sys::fuzz_target;

/// What `decoder` makes of `bytes` pushed in the given pieces: the events and the
/// error that ended the call, if one did, as text so the two runs compare. Like
/// `stream::decode`, it stops at `Finish` or the first error.
fn run(mut decoder: impl Decode, pieces: &[&[u8]]) -> String {
    let mut events = Vec::new();
    let mut result = Ok(());
    for piece in pieces {
        result = decoder.push(piece, &mut events);
        if result.is_err() || matches!(events.last(), Some(Event::Finish { .. })) {
            return format!("{events:?} {result:?}");
        }
    }
    if result.is_ok() {
        result = decoder.finish(&mut events);
    }
    format!("{events:?} {result:?}")
}

/// Splitting the bytes anywhere changes nothing: the same events and the same error.
fn check<D: Decode>(new: impl Fn() -> D, bytes: &[u8], head: &[u8], tail: &[u8]) {
    assert_eq!(run(new(), &[bytes]), run(new(), &[head, tail]), "chunking changed the call");
}

fuzz_target!(|data: &[u8]| {
    let Some((&cut, bytes)) = data.split_first() else {
        return;
    };
    let at = usize::from(cut) * bytes.len() / 255;
    let (head, tail) = bytes.split_at(at);

    check(|| gitcko_model_openai::Decoder::default(), bytes, head, tail);
    check(|| gitcko_model_ollama::Decoder::new(true), bytes, head, tail);
});
