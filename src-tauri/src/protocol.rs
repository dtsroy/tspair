//! Wire protocol: newline-delimited JSON over TCP.
//!
//! Message types (each is a JSON object with a "type" field):
//!
//! ```text
//! HELLO      {id}        first message on a new connection
//! PING       {}          heartbeat
//! PONG       {}
//! REQ_USE    {}          "I want to route via you" (ask permission)
//! GRANT      {}          provider approved; requester may set exit node
//! DENY       {reason}    provider refused (loop risk / simultaneous request)
//! USE_FAILED {reason}    requester granted but failed to apply the exit node
//! STATE      {state}     broadcast of sender's current ExitState
//! ```

use crate::exit_state::ExitState;
use bytes::{BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use std::io;
use tokio_util::codec::{Decoder, Encoder};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Msg {
    Hello { id: String },
    Ping,
    Pong,
    ReqUse,
    Grant,
    Deny {
        #[serde(default)]
        reason: String,
    },
    UseFailed {
        /// v1 sometimes sends USE_FAILED with no reason at all.
        #[serde(default)]
        reason: String,
    },
    State { state: ExitState },
}

/// Hard cap on a single buffered frame; anything bigger is dropped rather
/// than buffered forever (v1 was implicitly bounded by its 64 KiB recv buf).
const MAX_FRAME: usize = 1024 * 1024;

/// Newline-delimited JSON codec. Skips empty and malformed lines rather than
/// killing the link (same lenient behavior as v1's `FrameReader`).
#[derive(Default)]
pub struct MsgCodec {
    overflow: bool,
}

impl MsgCodec {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Decoder for MsgCodec {
    type Item = Msg;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Msg>> {
        loop {
            let Some(pos) = src.iter().position(|&b| b == b'\n') else {
                if src.len() > MAX_FRAME {
                    // Unparseable garbage without a newline — drop it.
                    src.clear();
                    self.overflow = true;
                }
                return Ok(None);
            };
            let line = src.split_to(pos + 1);
            // An earlier overflow means bytes were discarded mid-frame; the
            // next "line" is the tail of something we can no longer parse.
            if self.overflow {
                self.overflow = false;
                continue;
            }
            let Ok(s) = std::str::from_utf8(&line[..pos]) else {
                continue;
            };
            let s = s.trim();
            if s.is_empty() {
                continue;
            }
            match serde_json::from_str(s) {
                Ok(msg) => return Ok(Some(msg)),
                Err(_) => continue, // ignore malformed lines
            }
        }
    }
}

impl Encoder<Msg> for MsgCodec {
    type Error = io::Error;

    fn encode(&mut self, msg: Msg, dst: &mut BytesMut) -> io::Result<()> {
        let line = serde_json::to_vec(&msg)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        dst.reserve(line.len() + 1);
        dst.put(line.as_slice());
        dst.put_u8(b'\n');
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(codec: &mut MsgCodec, buf: &mut BytesMut, data: &[u8]) -> Vec<Msg> {
        buf.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some(m) = codec.decode(buf).unwrap() {
            out.push(m);
        }
        out
    }

    #[test]
    fn round_trip_all_variants() {
        let msgs = vec![
            Msg::Hello { id: "abc-123".into() },
            Msg::Ping,
            Msg::Pong,
            Msg::ReqUse,
            Msg::Grant,
            Msg::Deny { reason: "routing loop".into() },
            Msg::UseFailed { reason: "tailscale error".into() },
            Msg::State { state: ExitState::UsingOther },
        ];
        for m in msgs {
            let mut codec = MsgCodec::new();
            let mut buf = BytesMut::new();
            codec.encode(m.clone(), &mut buf).unwrap();
            assert_eq!(feed(&mut codec, &mut buf, &[]), vec![m]);
        }
    }

    #[test]
    fn wire_format_matches_v1() {
        // The Rust app must interoperate with the archived Python app's
        // framing: {"type":"..."} tags, compact JSON, trailing newline.
        let encoded = |m: Msg| {
            let mut codec = MsgCodec::new();
            let mut buf = BytesMut::new();
            codec.encode(m, &mut buf).unwrap();
            buf.to_vec()
        };
        assert_eq!(
            encoded(Msg::Hello { id: "x".into() }),
            b"{\"type\":\"HELLO\",\"id\":\"x\"}\n".to_vec()
        );
        assert_eq!(encoded(Msg::ReqUse), b"{\"type\":\"REQ_USE\"}\n".to_vec());
        assert_eq!(
            encoded(Msg::State { state: ExitState::None }),
            b"{\"type\":\"STATE\",\"state\":\"none\"}\n".to_vec()
        );
    }

    #[test]
    fn skips_malformed_and_empty_lines() {
        let mut codec = MsgCodec::new();
        let mut buf = BytesMut::new();
        let msgs = feed(
            &mut codec,
            &mut buf,
            b"{\"type\":\"REQ_USE\"}\n\nnot json\n{\"type\":\"PING\"}\n",
        );
        assert_eq!(msgs, vec![Msg::ReqUse, Msg::Ping]);
    }

    #[test]
    fn frames_split_across_reads() {
        let mut codec = MsgCodec::new();
        let mut buf = BytesMut::new();
        assert_eq!(feed(&mut codec, &mut buf, b"{\"type\":\"PI"), vec![]);
        assert_eq!(feed(&mut codec, &mut buf, b"NG\"}\n"), vec![Msg::Ping]);
    }
}
