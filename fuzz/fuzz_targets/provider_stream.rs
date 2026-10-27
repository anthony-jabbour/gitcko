// SPDX-License-Identifier: AGPL-3.0-or-later

//! Provider bytes never panic a decoder, and how they're split into chunks never
//! changes what comes out (TESTING §10, `provider_stream`).

#![no_main]

use gitcko_model::stream::{Decode, Ndjson};
use libfuzzer_sys::fuzz_target;

/// What `decoder` makes of `bytes` pushed in the given pieces: the events (as text,
/// to compare) and the error that ended the call, if one did. An error drops the
/// events of the push it came from, so only the clean runs compare exactly.
fn run(mut decoder: impl Decode, pieces: &[&[u8]]) -> (Vec<String>, Option<String>) {
    let mut out = Vec::new();
    for piece in pieces {
        match decoder.push(piece) {
            Ok(events) => out.extend(events.iter().map(|e| format!("{e:?}"))),
            Err(error) => return (out, Some(format!("{error:?}"))),
        }
        // As `stream::decode` does, stop reading at `Finish`.
        if out.last().is_some_and(|e| e.starts_with("Finish")) {
            return (out, None);
        }
    }
    match decoder.finish() {
        Ok(events) => out.extend(events.iter().map(|e| format!("{e:?}"))),
        Err(error) => return (out, Some(format!("{error:?}"))),
    }
    (out, None)
}

fuzz_target!(|data: &[u8]| {
    let Some((&cut, bytes)) = data.split_first() else {
        return;
    };
    let at = usize::from(cut) * bytes.len() / 255;
    let (head, tail) = bytes.split_at(at);

    let whole = run(gitcko_model_openai::Decoder::default(), &[bytes]);
    let split = run(gitcko_model_openai::Decoder::default(), &[head, tail]);
    assert_eq!(whole.1.is_some(), split.1.is_some(), "chunking changed whether it failed");
    if whole.1.is_none() {
        assert_eq!(whole, split, "chunking changed the OpenRouter events");
    } else {
        assert!(split.0.starts_with(&whole.0), "chunking reordered events");
    }

    let mut ndjson = Ndjson::default();
    let mut lines = ndjson.push(head).unwrap_or_default();
    lines.extend(ndjson.push(tail).unwrap_or_default());
    let _ = ndjson.finish();
    assert!(lines.iter().all(|l| !l.trim().is_empty()));
});
