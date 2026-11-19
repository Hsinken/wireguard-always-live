//! Size-bounded encoding and decoding of IPC envelopes.
//!
//! Both peers must use these functions rather than calling `serde_json`
//! directly. Decoding refuses input over [`MAX_MESSAGE_BYTES`] before parsing
//! it, and encoding stops buffering as soon as the output would pass the limit.
//! The transport still has to cap how many bytes it reads per message before
//! calling [`decode_request`].
//!
//! On a byte stream, such as the Windows named pipe, each message travels as
//! one frame: a 4-byte little-endian length, then that many bytes of one
//! encoded message. [`read_frame`] checks the length against the limit before
//! it reads or allocates the body.

use std::fmt;
use std::io;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::ids::RequestId;
use crate::ipc::{
    ErrorCode, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, RequestEnvelope, ResponseEnvelope,
};

const LIMIT: usize = MAX_MESSAGE_BYTES as usize;

/// Why an incoming message was rejected.
///
/// Carries no text from the input: serde's own error messages can quote it, and
/// it may hold values that must not reach a log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Larger than [`MAX_MESSAGE_BYTES`]. The bytes were not parsed.
    TooLarge { len: usize },
    /// Names a protocol version this build does not speak.
    UnsupportedVersion {
        version: u32,
        request_id: Option<RequestId>,
    },
    /// Not JSON, or not a valid envelope: unknown operation or field, invalid
    /// identifier or text, too many items, or nested too deeply.
    Malformed {
        request_id: Option<RequestId>,
        line: usize,
        column: usize,
    },
}

impl DecodeError {
    /// The code to answer a rejected request with.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::TooLarge { .. } => ErrorCode::MessageTooLarge,
            Self::UnsupportedVersion { .. } => ErrorCode::UnsupportedVersion,
            Self::Malformed { .. } => ErrorCode::MalformedRequest,
        }
    }

    /// The request ID, if it could be read, so a rejection can still be paired
    /// with its request.
    pub fn request_id(&self) -> Option<&RequestId> {
        match self {
            Self::TooLarge { .. } => None,
            Self::UnsupportedVersion { request_id, .. } | Self::Malformed { request_id, .. } => {
                request_id.as_ref()
            }
        }
    }

    fn malformed(request_id: Option<RequestId>, error: &serde_json::Error) -> Self {
        Self::Malformed {
            request_id,
            line: error.line(),
            column: error.column(),
        }
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { len } => {
                write!(f, "message is {len} bytes, limit is {MAX_MESSAGE_BYTES}")
            }
            Self::UnsupportedVersion { version, .. } => write!(
                f,
                "protocol version {version} is not supported, expected {PROTOCOL_VERSION}"
            ),
            Self::Malformed { line, column, .. } => {
                write!(f, "malformed message at line {line}, column {column}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Why an outgoing message could not be encoded.
#[derive(Debug)]
pub enum EncodeError {
    /// The encoded message would be larger than [`MAX_MESSAGE_BYTES`]. Nothing
    /// past the limit was buffered.
    TooLarge,
    Serialize(serde_json::Error),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "message would exceed {MAX_MESSAGE_BYTES} bytes"),
            Self::Serialize(e) => write!(f, "cannot serialize message: {e}"),
        }
    }
}

impl std::error::Error for EncodeError {}

pub fn decode_request(bytes: &[u8]) -> Result<RequestEnvelope, DecodeError> {
    decode(bytes)
}

pub fn decode_response(bytes: &[u8]) -> Result<ResponseEnvelope, DecodeError> {
    decode(bytes)
}

pub fn encode_request(envelope: &RequestEnvelope) -> Result<Vec<u8>, EncodeError> {
    encode_within(envelope, LIMIT)
}

pub fn encode_response(envelope: &ResponseEnvelope) -> Result<Vec<u8>, EncodeError> {
    encode_within(envelope, LIMIT)
}

/// Bytes in front of each frame's message: its length, little-endian.
pub const FRAME_PREFIX_BYTES: usize = 4;

/// Why a frame could not be read. Like [`DecodeError`], it never carries
/// bytes from the stream.
#[derive(Debug)]
pub enum FrameError {
    /// The stream ended before the first byte of a frame: the peer is done.
    Closed,
    /// The stream ended in the middle of a frame.
    Truncated,
    /// The prefix announced an empty message.
    Empty,
    /// The prefix announced more than [`MAX_MESSAGE_BYTES`]. Nothing after
    /// the prefix was read.
    TooLarge {
        len: u32,
    },
    Io(io::Error),
}

impl FrameError {
    /// The code to answer with, when the peer can still be answered at all.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::TooLarge { .. } => ErrorCode::MessageTooLarge,
            Self::Empty => ErrorCode::MalformedRequest,
            Self::Closed | Self::Truncated | Self::Io(_) => ErrorCode::Internal,
        }
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "the peer closed the connection"),
            Self::Truncated => write!(f, "the connection ended in the middle of a message"),
            Self::Empty => write!(f, "empty message"),
            Self::TooLarge { len } => {
                write!(f, "message is {len} bytes, limit is {MAX_MESSAGE_BYTES}")
            }
            Self::Io(e) => write!(f, "cannot read the message: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Writes one encoded message as a frame, in a single write, and flushes.
/// Refuses an empty message or one over [`MAX_MESSAGE_BYTES`] without
/// writing anything.
pub fn write_frame<W: io::Write + ?Sized>(writer: &mut W, message: &[u8]) -> io::Result<()> {
    let len = match u32::try_from(message.len()) {
        Ok(len) if len > 0 && len <= MAX_MESSAGE_BYTES => len,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a frame holds 1 to MAX_MESSAGE_BYTES bytes",
            ));
        }
    };
    let mut frame = Vec::with_capacity(FRAME_PREFIX_BYTES + message.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(message);
    writer.write_all(&frame)?;
    writer.flush()
}

/// Reads one frame and returns its message, still to be decoded.
pub fn read_frame<R: io::Read + ?Sized>(reader: &mut R) -> Result<Vec<u8>, FrameError> {
    let mut prefix = [0u8; FRAME_PREFIX_BYTES];
    let mut got = 0;
    while got < prefix.len() {
        match reader.read(&mut prefix[got..]) {
            Ok(0) if got == 0 => return Err(FrameError::Closed),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    let len = u32::from_le_bytes(prefix);
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len > MAX_MESSAGE_BYTES {
        return Err(FrameError::TooLarge { len });
    }
    let mut message = vec![0; len as usize];
    reader.read_exact(&mut message).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            FrameError::Truncated
        } else {
            FrameError::Io(e)
        }
    })?;
    Ok(message)
}

/// The fields every envelope starts with, read leniently so a request that is
/// otherwise invalid can still get an answer naming its ID and version.
#[derive(Deserialize)]
struct Header {
    protocol_version: u32,
    #[serde(default, deserialize_with = "lenient_request_id")]
    request_id: Option<RequestId>,
}

fn lenient_request_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<RequestId>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(value.as_str().and_then(|id| RequestId::new(id).ok()))
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, DecodeError> {
    if bytes.len() > LIMIT {
        return Err(DecodeError::TooLarge { len: bytes.len() });
    }
    let header: Header =
        serde_json::from_slice(bytes).map_err(|e| DecodeError::malformed(None, &e))?;
    if header.protocol_version != PROTOCOL_VERSION {
        return Err(DecodeError::UnsupportedVersion {
            version: header.protocol_version,
            request_id: header.request_id,
        });
    }
    serde_json::from_slice(bytes).map_err(|e| DecodeError::malformed(header.request_id, &e))
}

fn encode_within<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, EncodeError> {
    let mut out = LimitedBuf {
        buf: Vec::new(),
        limit,
    };
    match serde_json::to_writer(&mut out, value) {
        Ok(()) => Ok(out.buf),
        // LimitedBuf is the only source of I/O errors.
        Err(e) if e.is_io() => Err(EncodeError::TooLarge),
        Err(e) => Err(EncodeError::Serialize(e)),
    }
}

struct LimitedBuf {
    buf: Vec<u8>,
    limit: usize,
}

impl io::Write for LimitedBuf {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() > self.limit.saturating_sub(self.buf.len()) {
            return Err(io::Error::other("message size limit reached"));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::Request;

    fn get_snapshot() -> RequestEnvelope {
        RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("r1").unwrap(),
            request: Request::GetSnapshot,
        }
    }

    #[test]
    fn encoding_stops_at_the_limit() {
        let exact = serde_json::to_vec(&get_snapshot()).unwrap();
        assert_eq!(encode_within(&get_snapshot(), exact.len()).unwrap(), exact);
        assert!(matches!(
            encode_within(&get_snapshot(), exact.len() - 1),
            Err(EncodeError::TooLarge)
        ));
    }

    #[test]
    fn frames_round_trip_back_to_back() {
        let first = encode_request(&get_snapshot()).unwrap();
        let mut stream = Vec::new();
        write_frame(&mut stream, &first).unwrap();
        write_frame(&mut stream, b"{}").unwrap();
        assert_eq!(stream.len(), 2 * FRAME_PREFIX_BYTES + first.len() + 2);
        let mut reader = stream.as_slice();
        assert_eq!(read_frame(&mut reader).unwrap(), first);
        assert_eq!(read_frame(&mut reader).unwrap(), b"{}");
        assert!(matches!(read_frame(&mut reader), Err(FrameError::Closed)));
    }

    #[test]
    fn a_frame_cut_short_is_truncated_not_closed() {
        let mut stream = Vec::new();
        write_frame(&mut stream, b"{\"a\":1}").unwrap();
        for cut in 1..stream.len() {
            let mut reader = &stream[..cut];
            assert!(
                matches!(read_frame(&mut reader), Err(FrameError::Truncated)),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn an_oversized_or_empty_prefix_is_refused_before_the_body() {
        struct PrefixOnly([u8; 4], usize);
        impl io::Read for PrefixOnly {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                assert!(self.1 < 4, "read past the prefix");
                buf[0] = self.0[self.1];
                self.1 += 1;
                Ok(1)
            }
        }
        let over = (MAX_MESSAGE_BYTES + 1).to_le_bytes();
        assert!(matches!(
            read_frame(&mut PrefixOnly(over, 0)),
            Err(FrameError::TooLarge { len }) if len == MAX_MESSAGE_BYTES + 1
        ));
        assert!(matches!(
            read_frame(&mut PrefixOnly([0; 4], 0)),
            Err(FrameError::Empty)
        ));
        let max = MAX_MESSAGE_BYTES.to_le_bytes();
        let mut stream = max.to_vec();
        stream.resize(FRAME_PREFIX_BYTES + LIMIT, b' ');
        assert_eq!(read_frame(&mut stream.as_slice()).unwrap().len(), LIMIT);
    }

    #[test]
    fn writing_refuses_empty_and_oversized_messages() {
        let mut out = Vec::new();
        assert!(write_frame(&mut out, b"").is_err());
        assert!(write_frame(&mut out, &vec![b' '; LIMIT + 1]).is_err());
        assert!(out.is_empty());
        write_frame(&mut out, &vec![b' '; LIMIT]).unwrap();
        assert_eq!(out.len(), FRAME_PREFIX_BYTES + LIMIT);
    }

    #[test]
    fn limited_buffer_never_holds_more_than_the_limit() {
        let mut out = LimitedBuf {
            buf: Vec::new(),
            limit: 4,
        };
        assert!(io::Write::write_all(&mut out, b"12345").is_err());
        assert!(out.buf.len() <= 4);
    }
}
