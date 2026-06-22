//! Server-side framing and client-side cursor/dedup for the explicit streaming
//! protocol v2 (PR-026).
//!
//! The framer assigns monotonically increasing cursors and enforces that every
//! opened stream is closed by exactly one terminal frame. The cursor tracker
//! lets a client dedup replays and detect gaps so it can reconnect from the
//! last accepted cursor. None of this uses a quiet-window / silence heuristic.

use opensks_contracts::{
    ENGINE_STREAM_FRAME_SCHEMA, EngineStreamFrame, PublicEngineError, STREAM_PROTOCOL_VERSION,
};
use serde_json::Value;

/// Per-frame serialized byte ceiling. A larger frame is rejected rather than
/// written, so one oversized provider payload cannot blow up the transport.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StreamError {
    #[error("stream already terminated")]
    AlreadyTerminated,
    #[error("frame exceeds {MAX_FRAME_BYTES} byte limit ({0} bytes)")]
    FrameTooLarge(usize),
    #[error("serialize error: {0}")]
    Serialize(String),
}

/// Server-side framer for one stream. Assigns cursors and enforces a single
/// terminal frame.
#[derive(Debug)]
pub struct StreamFramer {
    stream_id: String,
    cursor: u64,
    terminated: bool,
}

impl StreamFramer {
    /// Open a stream. Returns the framer plus the `StreamOpened` frame (cursor 0).
    pub fn open(
        stream_id: impl Into<String>,
        request_id: impl Into<String>,
        project_id: impl Into<String>,
        conversation_id: impl Into<String>,
        run_id: Option<String>,
    ) -> (Self, EngineStreamFrame) {
        let stream_id = stream_id.into();
        let opened = EngineStreamFrame::StreamOpened {
            schema: ENGINE_STREAM_FRAME_SCHEMA.to_string(),
            stream_id: stream_id.clone(),
            request_id: request_id.into(),
            project_id: project_id.into(),
            conversation_id: conversation_id.into(),
            run_id,
            protocol_version: STREAM_PROTOCOL_VERSION.to_string(),
            cursor: 0,
        };
        (
            Self {
                stream_id,
                cursor: 0,
                terminated: false,
            },
            opened,
        )
    }

    fn guard(&self) -> Result<(), StreamError> {
        if self.terminated {
            Err(StreamError::AlreadyTerminated)
        } else {
            Ok(())
        }
    }

    fn next_cursor(&mut self) -> u64 {
        self.cursor += 1;
        self.cursor
    }

    pub fn event(&mut self, event: Value) -> Result<EngineStreamFrame, StreamError> {
        self.guard()?;
        Ok(EngineStreamFrame::Event {
            schema: ENGINE_STREAM_FRAME_SCHEMA.to_string(),
            stream_id: self.stream_id.clone(),
            cursor: self.next_cursor(),
            event,
        })
    }

    pub fn snapshot(&mut self, projection: Value) -> Result<EngineStreamFrame, StreamError> {
        self.guard()?;
        Ok(EngineStreamFrame::Snapshot {
            schema: ENGINE_STREAM_FRAME_SCHEMA.to_string(),
            stream_id: self.stream_id.clone(),
            cursor: self.next_cursor(),
            projection,
        })
    }

    pub fn heartbeat(&mut self, server_time_ms: u64) -> Result<EngineStreamFrame, StreamError> {
        self.guard()?;
        Ok(EngineStreamFrame::Heartbeat {
            schema: ENGINE_STREAM_FRAME_SCHEMA.to_string(),
            stream_id: self.stream_id.clone(),
            cursor: self.next_cursor(),
            server_time_ms,
        })
    }

    pub fn complete(
        &mut self,
        reason_code: impl Into<String>,
    ) -> Result<EngineStreamFrame, StreamError> {
        self.guard()?;
        let cursor = self.next_cursor();
        self.terminated = true;
        Ok(EngineStreamFrame::StreamCompleted {
            schema: ENGINE_STREAM_FRAME_SCHEMA.to_string(),
            stream_id: self.stream_id.clone(),
            cursor,
            reason_code: reason_code.into(),
        })
    }

    pub fn fail(
        &mut self,
        error: PublicEngineError,
        resumable: bool,
    ) -> Result<EngineStreamFrame, StreamError> {
        self.guard()?;
        let cursor = self.next_cursor();
        self.terminated = true;
        Ok(EngineStreamFrame::StreamFailed {
            schema: ENGINE_STREAM_FRAME_SCHEMA.to_string(),
            stream_id: self.stream_id.clone(),
            cursor,
            error,
            resumable,
        })
    }

    pub fn is_terminated(&self) -> bool {
        self.terminated
    }
}

/// Encode a frame to one NDJSON line, enforcing the per-frame byte ceiling.
pub fn encode_frame(frame: &EngineStreamFrame) -> Result<String, StreamError> {
    let line = serde_json::to_string(frame).map_err(|e| StreamError::Serialize(e.to_string()))?;
    if line.len() > MAX_FRAME_BYTES {
        return Err(StreamError::FrameTooLarge(line.len()));
    }
    Ok(line)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorDecision {
    Accept,
    DuplicateOrOld,
    Gap { expected: u64, got: u64 },
}

/// Client-side per-stream cursor tracker: dedups replays and surfaces gaps so a
/// client can reconnect from `last_cursor()`.
#[derive(Debug, Default)]
pub struct StreamCursorTracker {
    last: Option<u64>,
    terminated: bool,
}

impl StreamCursorTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn accept(&mut self, cursor: u64) -> CursorDecision {
        match self.last {
            None => {
                if cursor == 0 {
                    self.last = Some(0);
                    CursorDecision::Accept
                } else {
                    CursorDecision::Gap {
                        expected: 0,
                        got: cursor,
                    }
                }
            }
            Some(last) => {
                if cursor <= last {
                    CursorDecision::DuplicateOrOld
                } else if cursor == last + 1 {
                    self.last = Some(cursor);
                    CursorDecision::Accept
                } else {
                    CursorDecision::Gap {
                        expected: last + 1,
                        got: cursor,
                    }
                }
            }
        }
    }

    pub fn observe_terminal(&mut self) {
        self.terminated = true;
    }

    pub fn is_terminated(&self) -> bool {
        self.terminated
    }

    pub fn last_cursor(&self) -> Option<u64> {
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framer_assigns_monotonic_cursors_and_one_terminal() {
        let (mut framer, opened) = StreamFramer::open("s1", "req", "proj", "conv", None);
        assert_eq!(opened.cursor(), 0);
        assert_eq!(framer.event(Value::Null).unwrap().cursor(), 1);
        assert_eq!(framer.heartbeat(123).unwrap().cursor(), 2);
        assert_eq!(framer.event(Value::Null).unwrap().cursor(), 3);
        let term = framer.complete("done").unwrap();
        assert_eq!(term.cursor(), 4);
        assert!(term.is_terminal());
        assert!(framer.is_terminated());
        // No frames after a terminal.
        assert_eq!(
            framer.event(Value::Null),
            Err(StreamError::AlreadyTerminated)
        );
        assert_eq!(
            framer.complete("again"),
            Err(StreamError::AlreadyTerminated)
        );
    }

    #[test]
    fn delayed_events_are_not_truncated() {
        // A heartbeat-then-late-event sequence is fully accepted; nothing is
        // dropped by any silence/quiet-window heuristic (there is none).
        let (mut framer, opened) = StreamFramer::open("s1", "r", "p", "c", None);
        let frames = vec![
            opened,
            framer.heartbeat(1).unwrap(),
            framer.heartbeat(2).unwrap(),
            framer.event(serde_json::json!({"k":"late"})).unwrap(),
            framer.complete("ok").unwrap(),
        ];
        let mut tracker = StreamCursorTracker::new();
        for f in &frames {
            assert_eq!(tracker.accept(f.cursor()), CursorDecision::Accept);
            if f.is_terminal() {
                tracker.observe_terminal();
            }
        }
        assert!(tracker.is_terminated());
        assert_eq!(tracker.last_cursor(), Some(4));
    }

    #[test]
    fn tracker_dedups_and_detects_gaps_for_reconnect() {
        let mut t = StreamCursorTracker::new();
        assert_eq!(t.accept(0), CursorDecision::Accept);
        assert_eq!(t.accept(1), CursorDecision::Accept);
        assert_eq!(t.accept(2), CursorDecision::Accept);
        // Replay of an already-seen cursor is a duplicate.
        assert_eq!(t.accept(1), CursorDecision::DuplicateOrOld);
        assert_eq!(t.accept(2), CursorDecision::DuplicateOrOld);
        // A skip is a gap; reconnect should resume from last_cursor()+1.
        assert_eq!(
            t.accept(5),
            CursorDecision::Gap {
                expected: 3,
                got: 5
            }
        );
        assert_eq!(t.last_cursor(), Some(2));
    }

    #[test]
    fn interleaved_streams_decode_independently() {
        let (mut a, a_open) = StreamFramer::open("A", "ra", "p", "c", None);
        let (mut b, b_open) = StreamFramer::open("B", "rb", "p", "c", None);
        // Interleave frames from two streams on one transport.
        let interleaved = vec![
            a_open,
            b_open,
            a.event(Value::Null).unwrap(),
            b.event(Value::Null).unwrap(),
            b.complete("b-done").unwrap(),
            a.event(Value::Null).unwrap(),
            a.complete("a-done").unwrap(),
        ];
        let mut ta = StreamCursorTracker::new();
        let mut tb = StreamCursorTracker::new();
        for f in &interleaved {
            let decision = match f.stream_id() {
                "A" => ta.accept(f.cursor()),
                "B" => tb.accept(f.cursor()),
                other => panic!("unexpected stream {other}"),
            };
            assert_eq!(decision, CursorDecision::Accept, "frame {f:?}");
        }
        assert_eq!(ta.last_cursor(), Some(3));
        assert_eq!(tb.last_cursor(), Some(2));
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let (mut framer, _) = StreamFramer::open("s", "r", "p", "c", None);
        let big = serde_json::json!({ "blob": "x".repeat(MAX_FRAME_BYTES) });
        let frame = framer.event(big).unwrap();
        assert!(matches!(
            encode_frame(&frame),
            Err(StreamError::FrameTooLarge(_))
        ));
    }

    #[test]
    fn frames_round_trip_through_ndjson() {
        let (mut framer, opened) = StreamFramer::open("s", "r", "p", "c", Some("run-1".into()));
        let line = encode_frame(&opened).unwrap();
        let decoded: EngineStreamFrame = serde_json::from_str(&line).unwrap();
        assert_eq!(decoded, opened);
        let ev = framer.event(serde_json::json!({"n":1})).unwrap();
        let decoded_ev: EngineStreamFrame =
            serde_json::from_str(&encode_frame(&ev).unwrap()).unwrap();
        assert_eq!(decoded_ev.cursor(), 1);
    }
}
