// SPDX-License-Identifier: AGPL-3.0-or-later

//! The bounded framing under both adapters: SSE and NDJSON edge cases, and hostile
//! input that must end in a typed error, never unbounded memory or a panic.

use futures_util::StreamExt;
use gitcko_model::stream::{Decode, Lines, MAX_EVENT, MAX_LINE, Ndjson, Sse, decode};
use gitcko_model::{ErrorKind, Event, FinishReason, ProviderError};
use proptest::prelude::*;
use tokio_util::sync::CancellationToken;

/// The framers append to a caller's buffer; tests want the frames back.
trait PushAll {
    fn push_all(&mut self, bytes: &[u8]) -> Result<Vec<String>, ProviderError>;
}

macro_rules! push_all {
    ($($t:ty),*) => {$(
        impl PushAll for $t {
            fn push_all(&mut self, bytes: &[u8]) -> Result<Vec<String>, ProviderError> {
                let mut out = Vec::new();
                self.push(bytes, &mut out).map(|()| out)
            }
        }
    )*};
}
push_all!(Lines, Sse, Ndjson);

fn sse(bytes: &[u8]) -> Vec<String> {
    Sse::default().push_all(bytes).unwrap()
}

#[test]
fn sse_skips_comments_and_other_fields() {
    let events = sse(b": OPENROUTER PROCESSING\n\nevent: x\nid: 1\ndata: {\"a\":1}\n\n");
    assert_eq!(events, ["{\"a\":1}"]);
}

#[test]
fn sse_joins_multi_line_data_and_accepts_crlf() {
    assert_eq!(sse(b"data: one\r\ndata:two\r\n\r\n"), ["one\ntwo"]);
}

#[test]
fn sse_keeps_an_empty_data_line_and_drops_an_unfinished_event() {
    let mut decoder = Sse::default();
    assert_eq!(decoder.push_all(b"data\n\ndata: cut off").unwrap(), [""]);
    assert!(decoder.push_all(b"").unwrap().is_empty());
}

#[test]
fn a_line_split_across_pushes_joins() {
    let mut lines = Lines::default();
    assert!(lines.push_all(b"da").unwrap().is_empty());
    assert!(lines.push_all(b"ta: x\r").unwrap().is_empty());
    assert_eq!(lines.push_all(b"\nnext").unwrap(), ["data: x"]);
    assert_eq!(lines.finish().unwrap().as_deref(), Some("next"));
}

#[test]
fn a_line_of_exactly_the_limit_passes_and_one_more_byte_fails() {
    let mut line = vec![b'a'; MAX_LINE];
    line.extend_from_slice(b"\r\n");
    assert_eq!(Lines::default().push_all(&line).unwrap()[0].len(), MAX_LINE);
    let long = vec![b'a'; MAX_LINE + 1];
    let error = Lines::default()
        .push_all(&[&long[..], b"\n"].concat())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::TooLarge("line"));
}

#[test]
fn an_endless_line_fails_once_past_the_limit_however_it_is_pushed() {
    let mut lines = Lines::default();
    let chunk = vec![b'a'; 64 * 1024];
    let error = (0..64)
        .find_map(|_| lines.push_all(&chunk).err())
        .expect("a 4 MiB line without a newline is refused");
    assert_eq!(error.kind, ErrorKind::TooLarge("line"));
}

#[test]
fn an_event_past_the_limit_fails() {
    let line = format!("data: {}\n", "a".repeat(MAX_LINE - 6));
    let mut decoder = Sse::default();
    let error = (0..=MAX_EVENT / MAX_LINE)
        .find_map(|_| decoder.push_all(line.as_bytes()).err())
        .expect("an event over 4 MiB is refused");
    assert_eq!(error.kind, ErrorKind::TooLarge("event"));
}

#[test]
fn a_line_that_is_not_utf8_is_invalid() {
    let error = Lines::default().push_all(b"\xff\n").unwrap_err();
    assert!(matches!(error.kind, ErrorKind::InvalidResponse(_)));
}

#[test]
fn ndjson_skips_blank_lines_and_keeps_an_unterminated_last_value() {
    let mut decoder = Ndjson::default();
    assert_eq!(
        decoder.push_all(b"{\"a\":1}\n\n  \r\n{\"b\"").unwrap(),
        ["{\"a\":1}"]
    );
    assert_eq!(decoder.push_all(b":2}").unwrap(), Vec::<String>::new());
    assert_eq!(decoder.finish().unwrap().as_deref(), Some("{\"b\":2}"));
}

proptest! {
    /// However the bytes are chunked, the same events come out.
    #[test]
    fn chunking_never_changes_sse_events(
        events in prop::collection::vec("[a-z{}\":, ]{0,20}", 0..6),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..8),
    ) {
        let body: String = events.iter().map(|e| format!(": c\r\ndata: {e}\n\n")).collect();
        let bytes = body.as_bytes();
        let mut at: Vec<usize> = cuts.iter().map(|i| i.index(bytes.len() + 1)).collect();
        at.sort_unstable();
        let mut decoder = Sse::default();
        let mut seen = Vec::new();
        let mut from = 0;
        for to in at.into_iter().chain([bytes.len()]) {
            seen.extend(decoder.push_all(&bytes[from..to]).unwrap());
            from = to;
        }
        prop_assert_eq!(seen, events);
    }
}

/// Text per line, then `Finish` when the body ends.
struct Echo;

impl Decode for Echo {
    fn push(&mut self, bytes: &[u8], events: &mut Vec<Event>) -> Result<(), ProviderError> {
        let text = String::from_utf8_lossy(bytes).into_owned();
        if text == "fail" {
            return Err(ProviderError::new(ErrorKind::Server));
        }
        events.push(Event::TextDelta { text });
        Ok(())
    }
    fn finish(&mut self, events: &mut Vec<Event>) -> Result<(), ProviderError> {
        events.push(Event::Finish {
            reason: FinishReason::Stop,
        });
        Ok(())
    }
}

fn body(
    chunks: &[&'static str],
) -> impl futures_util::Stream<Item = Result<&'static [u8], ProviderError>> + Send + 'static {
    futures_util::stream::iter(chunks.iter().map(|c| Ok(c.as_bytes())).collect::<Vec<_>>())
}

#[tokio::test]
async fn decode_ends_after_finish_and_after_the_first_error() {
    let events: Vec<_> = decode(body(&["a", "b"]), Echo, CancellationToken::new())
        .collect()
        .await;
    assert_eq!(events.len(), 3);
    assert_eq!(
        events[2],
        Ok(Event::Finish {
            reason: FinishReason::Stop
        })
    );

    let events: Vec<_> = decode(body(&["a", "fail", "b"]), Echo, CancellationToken::new())
        .collect()
        .await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1], Err(ProviderError::new(ErrorKind::Server)));
}

#[tokio::test(start_paused = true)]
async fn cancel_while_waiting_for_bytes_yields_canceled_and_drops_the_body() {
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    struct Flag(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let flag = Flag(dropped.clone());
    let pending =
        futures_util::stream::pending::<Result<&'static [u8], ProviderError>>().map(move |item| {
            let _ = &flag;
            item
        });
    let cancel = CancellationToken::new();
    let mut events = decode(pending, Echo, cancel.clone());
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        cancel.cancel();
    });
    assert_eq!(
        events.next().await,
        Some(Err(ProviderError::new(ErrorKind::Canceled)))
    );
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(events.next().await, None);
}

#[test]
fn an_unterminated_line_never_holds_more_than_the_limit_and_a_cr() {
    let mut lines = Lines::default();
    lines.push_all(&vec![b'a'; MAX_LINE + 1]).unwrap();
    assert_eq!(
        lines.push_all(b"a").unwrap_err().kind,
        ErrorKind::TooLarge("line")
    );
}

#[tokio::test]
async fn cancel_wins_over_bytes_that_are_ready() {
    // `select!` picks a random branch unless biased, so try enough times to see it.
    for _ in 0..64 {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut events = decode(body(&["a"]), Echo, cancel);
        assert_eq!(
            events.next().await,
            Some(Err(ProviderError::new(ErrorKind::Canceled)))
        );
        assert_eq!(events.next().await, None);
    }
}
