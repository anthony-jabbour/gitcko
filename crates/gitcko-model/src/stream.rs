// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bounded framing for provider streams: one line splitter under both SSE (OpenRouter)
//! and NDJSON (Ollama), so a hostile endless line or event can't grow memory without
//! bound. Push-based and synchronous, so the fuzz target and the tests feed it bytes
//! directly; [`decode`] drives it from an async byte stream.

use futures_util::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::{ErrorKind, Event, EventStream, ProviderError};

/// The longest line accepted, terminator excluded.
pub const MAX_LINE: usize = 1 << 20;
/// The largest SSE event (its `data:` lines joined).
pub const MAX_EVENT: usize = 4 << 20;
/// The largest non-streaming body, such as an error response.
pub const MAX_BODY: usize = 8 << 20;

fn too_large(what: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::TooLarge(what))
}

/// Splits bytes into lines ending in LF or CRLF, holding at most [`MAX_LINE`] bytes.
#[derive(Debug, Default)]
pub struct Lines {
    buf: Vec<u8>,
}

impl Lines {
    /// The complete lines in `bytes`, after whatever earlier pushes left unfinished.
    pub fn push(&mut self, mut bytes: &[u8]) -> Result<Vec<String>, ProviderError> {
        let mut lines = Vec::new();
        while let Some(end) = bytes.iter().position(|b| *b == b'\n') {
            self.extend(&bytes[..end])?;
            lines.push(self.take()?);
            bytes = &bytes[end + 1..];
        }
        self.extend(bytes)?;
        Ok(lines)
    }

    /// The last line, if the stream ended without a terminator.
    pub fn finish(&mut self) -> Result<Option<String>, ProviderError> {
        if self.buf.is_empty() {
            return Ok(None);
        }
        self.take().map(Some)
    }

    fn extend(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        // A trailing CR may still be followed by LF, so it counts toward the limit
        // only once the line is known to go on.
        if self.buf.len() + bytes.len() > MAX_LINE + 1 {
            return Err(too_large("line"));
        }
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    fn take(&mut self) -> Result<String, ProviderError> {
        let mut line = std::mem::take(&mut self.buf);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.len() > MAX_LINE {
            return Err(too_large("line"));
        }
        String::from_utf8(line)
            .map_err(|_| ProviderError::new(ErrorKind::InvalidResponse("a line is not UTF-8")))
    }
}

/// Server-sent events, keeping only `data:` (the WHATWG event-stream rules).
/// Comment lines such as OpenRouter's `: OPENROUTER PROCESSING` are skipped, and an
/// event cut off by the end of the stream is dropped, as the spec says.
#[derive(Debug, Default)]
pub struct Sse {
    lines: Lines,
    data: Option<String>,
}

impl Sse {
    /// The data of each event completed by `bytes`.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, ProviderError> {
        let mut events = Vec::new();
        for line in self.lines.push(bytes)? {
            if line.is_empty() {
                events.extend(self.data.take());
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((&line, ""));
            if field != "data" {
                // A comment (empty field name), or `event`/`id`/`retry`, which no
                // provider we speak needs.
                continue;
            }
            let value = value.strip_prefix(' ').unwrap_or(value);
            let data = match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data
                }
                None => self.data.insert(String::new()),
            };
            if data.len() + value.len() > MAX_EVENT {
                return Err(too_large("event"));
            }
            data.push_str(value);
        }
        Ok(events)
    }
}

/// Newline-delimited JSON: each non-blank line is one value.
#[derive(Debug, Default)]
pub struct Ndjson {
    lines: Lines,
}

impl Ndjson {
    /// The lines completed by `bytes`, blank ones skipped.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, ProviderError> {
        let mut lines = self.lines.push(bytes)?;
        lines.retain(|l| !l.trim().is_empty());
        Ok(lines)
    }

    /// The last value, when the stream didn't end with a newline.
    pub fn finish(&mut self) -> Result<Option<String>, ProviderError> {
        Ok(self.lines.finish()?.filter(|l| !l.trim().is_empty()))
    }
}

/// An adapter's wire decoder: bytes in, provider-neutral events out. Each adapter
/// owns the state (tool-call fragments, `[DONE]`, usage) between pushes.
pub trait Decode: Send + 'static {
    /// The events completed by `bytes`. An error ends the call.
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Event>, ProviderError>;
    /// The stream ended: the last events, or why the stream was incomplete.
    fn finish(&mut self) -> Result<Vec<Event>, ProviderError>;
}

/// Runs `decoder` over a response body. Ends after `Finish` or the first error;
/// canceling yields `Canceled` and drops `body`, which closes the connection.
pub fn decode<S, B, D>(body: S, decoder: D, cancel: CancellationToken) -> EventStream
where
    S: Stream<Item = Result<B, ProviderError>> + Send + 'static,
    B: AsRef<[u8]>,
    D: Decode,
{
    struct State<S, D> {
        body: Option<std::pin::Pin<Box<S>>>,
        decoder: D,
        pending: std::collections::VecDeque<Result<Event, ProviderError>>,
        cancel: CancellationToken,
    }
    let state = State {
        body: Some(Box::pin(body)),
        decoder,
        pending: Default::default(),
        cancel,
    };
    futures_util::stream::unfold(state, |mut s| async move {
        loop {
            if let Some(item) = s.pending.pop_front() {
                if matches!(item, Ok(Event::Finish { .. }) | Err(_)) {
                    s.pending.clear();
                    s.body = None;
                }
                return Some((item, s));
            }
            let body = s.body.as_mut()?;
            let next = tokio::select! {
                biased;
                () = s.cancel.cancelled() => {
                    s.body = None;
                    return Some((Err(ProviderError::new(ErrorKind::Canceled)), s));
                }
                next = body.next() => next,
            };
            let decoded = match next {
                Some(Ok(bytes)) => s.decoder.push(bytes.as_ref()),
                Some(Err(error)) => Err(error),
                None => {
                    s.body = None;
                    s.decoder.finish()
                }
            };
            match decoded {
                Ok(events) => s.pending.extend(events.into_iter().map(Ok)),
                Err(error) => s.pending.push_back(Err(error)),
            }
            if s.body.is_none() && s.pending.is_empty() {
                return None;
            }
        }
    })
    .boxed()
}
