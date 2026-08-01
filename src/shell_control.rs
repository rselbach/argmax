//! Bounded parent-to-shell editing-buffer and probe-recovery controls.
//!
//! Controls use direction-specific, versioned ASCII grammars so arbitrary
//! buffer bytes are never interpreted as shell syntax:
//!
//! `argmax-control-v1:replace:REQUEST:CURSOR:LENGTH:LOWER_HEX\0`
//!
//! `argmax-control-v1:resync:REQUEST\0`
//!
//! `CURSOR` and `LENGTH` are UTF-8 byte counts. Replacement identifiers reserve
//! an ordinary synchronization-probe nonce. Probe-resynchronization identifiers
//! use an independent bounded sequence and correlate a shell response reporting
//! its current adapter counter. Correlation recovers transport state; it is not
//! authentication against another same-user process that already holds the
//! private control or event descriptor.

use std::error::Error;
use std::fmt;

/// Largest decoded editing buffer accepted by every shell adapter.
pub const MAX_CONTROL_BUFFER_BYTES: usize = crate::integration::MAX_SYNC_EVENT_CHARACTERS;

/// Largest replacement identifier representable by every supported adapter.
///
/// Fish intentionally exhausts its probe counter at this signed 32-bit bound.
pub const MAX_CONTROL_REQUEST_ID: u64 = 2_147_483_647;

/// Largest probe-resynchronization identifier supported by every adapter.
pub const MAX_PROBE_RESYNC_REQUEST_ID: u64 = 2_147_483_647;

const REPLACEMENT_CONTROL_PREFIX: &[u8] = b"argmax-control-v1:replace:";
const PROBE_RESYNC_CONTROL_PREFIX: &[u8] = b"argmax-control-v1:resync:";
const MAX_REQUEST_ID_DIGITS: usize = 10;
const MAX_BUFFER_SIZE_DIGITS: usize = 5;

/// Hard maximum frame size excluding the NUL terminator.
///
/// The largest replacement remains the limiting grammar; adding the shorter
/// resynchronization control does not increase retained decoder storage.
pub const MAX_CONTROL_FRAME_BYTES: usize = REPLACEMENT_CONTROL_PREFIX.len()
    + MAX_REQUEST_ID_DIGITS
    + 1
    + MAX_BUFFER_SIZE_DIGITS
    + 1
    + MAX_BUFFER_SIZE_DIGITS
    + 1
    + MAX_CONTROL_BUFFER_BYTES * 2;

/// Hard maximum control wire size including its NUL terminator.
pub const MAX_CONTROL_WIRE_BYTES: usize = MAX_CONTROL_FRAME_BYTES + 1;

/// Monotonic identifier shared with one reserved synchronization probe.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ControlRequestId(u64);

impl ControlRequestId {
    /// Validates a nonzero identifier supported by every shell adapter.
    ///
    /// # Errors
    ///
    /// Returns [`ControlRequestIdError`] for zero or a value above the shared
    /// shell-adapter bound.
    pub const fn new(value: u64) -> Result<Self, ControlRequestIdError> {
        if value == 0 {
            return Err(ControlRequestIdError::Zero);
        }
        if value > MAX_CONTROL_REQUEST_ID {
            return Err(ControlRequestIdError::OutOfRange {
                maximum: MAX_CONTROL_REQUEST_ID,
            });
        }
        Ok(Self(value))
    }

    /// Returns the numeric request identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Invalid replacement-control identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlRequestIdError {
    /// Zero is never a synchronization-probe nonce.
    Zero,
    /// The identifier cannot be represented by every shell adapter.
    OutOfRange {
        /// Largest accepted identifier.
        maximum: u64,
    },
}

impl fmt::Display for ControlRequestIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("shell control request identifier is zero"),
            Self::OutOfRange { maximum } => write!(
                formatter,
                "shell control request identifier exceeds shared maximum {maximum}"
            ),
        }
    }
}

impl Error for ControlRequestIdError {}

/// Monotonic identifier for one explicit adapter-counter resynchronization.
///
/// This sequence is independent from replacement controls and ordinary snapshot
/// nonces. [`crate::shell_events::ShellSessionState`] allocates values without
/// wrapping or reuse within one parent session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProbeResyncRequestId(u64);

impl ProbeResyncRequestId {
    /// Validates a nonzero identifier supported by every shell adapter.
    ///
    /// # Errors
    ///
    /// Returns [`ProbeResyncRequestIdError`] for zero or a value above the
    /// shared shell-adapter bound.
    pub const fn new(value: u64) -> Result<Self, ProbeResyncRequestIdError> {
        if value == 0 {
            return Err(ProbeResyncRequestIdError::Zero);
        }
        if value > MAX_PROBE_RESYNC_REQUEST_ID {
            return Err(ProbeResyncRequestIdError::OutOfRange {
                maximum: MAX_PROBE_RESYNC_REQUEST_ID,
            });
        }
        Ok(Self(value))
    }

    /// Returns the numeric request identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Invalid probe-resynchronization request identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeResyncRequestIdError {
    /// Zero is never a valid request identifier.
    Zero,
    /// The identifier cannot be represented by every shell adapter.
    OutOfRange {
        /// Largest accepted identifier.
        maximum: u64,
    },
}

impl fmt::Display for ProbeResyncRequestIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("probe resync request identifier is zero"),
            Self::OutOfRange { maximum } => write!(
                formatter,
                "probe resync request identifier exceeds shared maximum {maximum}"
            ),
        }
    }
}

impl Error for ProbeResyncRequestIdError {}

/// One validated request to replace the shell-native editing buffer.
#[derive(Clone, Eq, PartialEq)]
pub struct ReplacementControl {
    request_id: ControlRequestId,
    buffer: Box<str>,
    cursor: usize,
}

impl ReplacementControl {
    /// Validates an inert UTF-8 replacement and its byte cursor.
    ///
    /// # Errors
    ///
    /// Returns [`ControlEncodeError`] when the buffer is oversized, contains a
    /// NUL byte, or has an invalid UTF-8 byte cursor.
    pub fn new(
        request_id: ControlRequestId,
        buffer: impl Into<String>,
        cursor: usize,
    ) -> Result<Self, ControlEncodeError> {
        let buffer = buffer.into();
        validate_replacement(&buffer, cursor)?;
        Ok(Self {
            request_id,
            buffer: buffer.into_boxed_str(),
            cursor,
        })
    }

    /// Correlated synchronization-probe identifier.
    #[must_use]
    pub const fn request_id(&self) -> ControlRequestId {
        self.request_id
    }

    /// Exact replacement text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.buffer
    }

    /// UTF-8 byte cursor within the replacement.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replacement size in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Whether the replacement is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Encodes this request as one complete NUL-framed control.
    #[must_use]
    pub fn encode(&self) -> EncodedControlFrame {
        let request = self.request_id.get().to_string();
        let cursor = self.cursor.to_string();
        let length = self.buffer.len().to_string();
        let capacity = REPLACEMENT_CONTROL_PREFIX.len()
            + request.len()
            + cursor.len()
            + length.len()
            + self.buffer.len() * 2
            + 4;
        let mut bytes = Vec::with_capacity(capacity);
        bytes.extend_from_slice(REPLACEMENT_CONTROL_PREFIX);
        bytes.extend_from_slice(request.as_bytes());
        bytes.push(b':');
        bytes.extend_from_slice(cursor.as_bytes());
        bytes.push(b':');
        bytes.extend_from_slice(length.as_bytes());
        bytes.push(b':');
        encode_lower_hex(self.buffer.as_bytes(), &mut bytes);
        bytes.push(0);
        EncodedControlFrame(bytes.into_boxed_slice())
    }
}

impl fmt::Debug for ReplacementControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReplacementControl")
            .field("request_id", &self.request_id)
            .field("buffer_bytes", &self.buffer.len())
            .field("cursor", &self.cursor)
            .finish()
    }
}

/// One request for the shell adapter's current ordinary probe counter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeResyncControl {
    request_id: ProbeResyncRequestId,
}

impl ProbeResyncControl {
    /// Creates a correlated adapter-counter resynchronization request.
    #[must_use]
    pub const fn new(request_id: ProbeResyncRequestId) -> Self {
        Self { request_id }
    }

    /// Returns the independent resynchronization request identifier.
    #[must_use]
    pub const fn request_id(self) -> ProbeResyncRequestId {
        self.request_id
    }

    /// Encodes this request as one complete NUL-framed control.
    #[must_use]
    pub fn encode(self) -> EncodedControlFrame {
        let request = self.request_id.get().to_string();
        let mut bytes = Vec::with_capacity(PROBE_RESYNC_CONTROL_PREFIX.len() + request.len() + 1);
        bytes.extend_from_slice(PROBE_RESYNC_CONTROL_PREFIX);
        bytes.extend_from_slice(request.as_bytes());
        bytes.push(0);
        EncodedControlFrame(bytes.into_boxed_slice())
    }
}

/// Complete parent-to-shell control wire bytes, including the NUL terminator.
#[derive(Clone, Eq, PartialEq)]
pub struct EncodedControlFrame(Box<[u8]>);

impl EncodedControlFrame {
    /// Returns exact bytes to queue on the private full-duplex control stream.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns the complete framed size.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether the encoded frame is empty.
    ///
    /// Valid encoded controls are never empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for EncodedControlFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedControlFrame")
            .field("wire_bytes", &self.0.len())
            .finish()
    }
}

/// Invalid replacement supplied to the control encoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlEncodeError {
    /// Replacement exceeds the adapter-wide byte bound.
    BufferTooLarge {
        /// Observed byte count.
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
    /// Shell editing buffers cannot represent NUL.
    NulBuffer,
    /// Cursor is outside the buffer or splits a UTF-8 scalar.
    InvalidCursor {
        /// Supplied byte cursor.
        cursor: usize,
        /// Replacement byte count.
        buffer_bytes: usize,
    },
}

impl fmt::Display for ControlEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "shell control buffer is {bytes} bytes; limit is {limit}"
                )
            }
            Self::NulBuffer => formatter.write_str("shell control buffer contains NUL"),
            Self::InvalidCursor {
                cursor,
                buffer_bytes,
            } => write!(
                formatter,
                "shell control cursor {cursor} is invalid for {buffer_bytes} bytes"
            ),
        }
    }
}

impl Error for ControlEncodeError {}

/// One complete result from the parent-to-shell control stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodedControlFrame {
    /// A validated inert replacement request.
    Replacement(ReplacementControl),
    /// A validated request for the adapter's current ordinary probe counter.
    ProbeResync(ProbeResyncControl),
    /// One isolated malformed frame; later frames remain decodable.
    Rejected(ControlFrameError),
}

/// Closed, content-free reason a control frame was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlFrameError {
    /// A terminator appeared without frame bytes.
    EmptyFrame,
    /// Frame was not addressed from the parent to a shell adapter.
    WrongDirection,
    /// Direction was correct but the version or operation was unsupported.
    UnsupportedProtocol,
    /// Required colon-separated fields were missing or duplicated.
    InvalidGrammar,
    /// Request identifier was not canonical unsigned decimal.
    InvalidRequestId,
    /// Request identifier was zero or exceeded the shared adapter range.
    RequestIdOutOfRange,
    /// Cursor was not canonical unsigned decimal.
    InvalidCursor,
    /// Declared byte count was not canonical unsigned decimal.
    InvalidLength,
    /// Declared replacement exceeded the adapter-wide byte bound.
    BufferTooLarge {
        /// Declared byte count.
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
    /// Hex payload length did not match the declared byte count.
    HexLengthMismatch,
    /// Payload contained something other than lowercase hexadecimal ASCII.
    InvalidHex,
    /// Decoded shell buffer contained NUL.
    NulBuffer,
    /// Decoded shell buffer was not UTF-8.
    InvalidUtf8,
    /// Cursor was outside the decoded buffer.
    CursorOutOfRange,
    /// Cursor split a UTF-8 scalar.
    CursorNotUtf8Boundary,
    /// Frame exceeded the configured retained-byte bound.
    FrameTooLarge {
        /// Observed bytes, saturated at [`usize::MAX`].
        observed_bytes: usize,
        /// Configured maximum frame size.
        limit: usize,
    },
    /// End of input arrived before a NUL terminator.
    TruncatedFrame {
        /// Buffered bytes at end of input.
        observed_bytes: usize,
    },
}

impl fmt::Display for ControlFrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFrame => formatter.write_str("empty shell control frame"),
            Self::WrongDirection => formatter.write_str("shell control has the wrong direction"),
            Self::UnsupportedProtocol => {
                formatter.write_str("unsupported shell control version or operation")
            }
            Self::InvalidGrammar => formatter.write_str("invalid shell control grammar"),
            Self::InvalidRequestId => {
                formatter.write_str("shell control request identifier is not canonical decimal")
            }
            Self::RequestIdOutOfRange => {
                formatter.write_str("shell control request identifier is out of range")
            }
            Self::InvalidCursor => {
                formatter.write_str("shell control cursor is not canonical decimal")
            }
            Self::InvalidLength => {
                formatter.write_str("shell control length is not canonical decimal")
            }
            Self::BufferTooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "shell control buffer is {bytes} bytes; limit is {limit}"
                )
            }
            Self::HexLengthMismatch => {
                formatter.write_str("shell control hex length does not match its declaration")
            }
            Self::InvalidHex => {
                formatter.write_str("shell control payload is not lowercase hexadecimal")
            }
            Self::NulBuffer => formatter.write_str("shell control buffer contains NUL"),
            Self::InvalidUtf8 => formatter.write_str("shell control buffer is not UTF-8"),
            Self::CursorOutOfRange => formatter.write_str("shell control cursor is out of range"),
            Self::CursorNotUtf8Boundary => {
                formatter.write_str("shell control cursor splits a UTF-8 scalar")
            }
            Self::FrameTooLarge {
                observed_bytes,
                limit,
            } => write!(
                formatter,
                "shell control frame is {observed_bytes} bytes; limit is {limit}"
            ),
            Self::TruncatedFrame { observed_bytes } => write!(
                formatter,
                "shell control frame ended after {observed_bytes} bytes without a terminator"
            ),
        }
    }
}

impl Error for ControlFrameError {}

/// Incremental NUL-frame decoder with bounded retained storage.
pub struct ShellControlDecoder {
    pending: Vec<u8>,
    oversized_bytes: Option<usize>,
    frame_limit: usize,
}

impl ShellControlDecoder {
    /// Creates a decoder at the hard protocol frame limit.
    #[must_use]
    pub fn new() -> Self {
        Self::with_frame_limit(MAX_CONTROL_FRAME_BYTES)
    }

    /// Creates a decoder with a caller limit capped by the protocol maximum.
    #[must_use]
    pub fn with_frame_limit(frame_limit: usize) -> Self {
        let frame_limit = frame_limit.min(MAX_CONTROL_FRAME_BYTES);
        Self {
            pending: Vec::with_capacity(frame_limit.min(4 * 1024)),
            oversized_bytes: None,
            frame_limit,
        }
    }

    /// Configured maximum retained bytes per unterminated frame.
    #[must_use]
    pub const fn frame_limit(&self) -> usize {
        self.frame_limit
    }

    /// Number of bytes retained for a partial frame.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Whether an oversized frame is being discarded through its terminator.
    #[must_use]
    pub const fn is_discarding(&self) -> bool {
        self.oversized_bytes.is_some()
    }

    /// Consumes a stream chunk and emits complete results in wire order.
    pub fn push<F>(&mut self, chunk: &[u8], mut emit: F)
    where
        F: FnMut(DecodedControlFrame),
    {
        for &byte in chunk {
            if byte == 0 {
                emit(self.finish_terminated_frame());
                continue;
            }
            if let Some(observed_bytes) = &mut self.oversized_bytes {
                *observed_bytes = observed_bytes.saturating_add(1);
            } else if self.pending.len() < self.frame_limit {
                self.pending.push(byte);
            } else {
                self.oversized_bytes = Some(self.pending.len().saturating_add(1));
                self.pending.clear();
            }
        }
    }

    /// Rejects one partial frame at end of stream and clears decoder state.
    pub fn finish(&mut self) -> Option<DecodedControlFrame> {
        if let Some(observed_bytes) = self.oversized_bytes.take() {
            return Some(DecodedControlFrame::Rejected(
                ControlFrameError::FrameTooLarge {
                    observed_bytes,
                    limit: self.frame_limit,
                },
            ));
        }
        if self.pending.is_empty() {
            return None;
        }
        let observed_bytes = self.pending.len();
        self.pending.clear();
        Some(DecodedControlFrame::Rejected(
            ControlFrameError::TruncatedFrame { observed_bytes },
        ))
    }

    fn finish_terminated_frame(&mut self) -> DecodedControlFrame {
        if let Some(observed_bytes) = self.oversized_bytes.take() {
            return DecodedControlFrame::Rejected(ControlFrameError::FrameTooLarge {
                observed_bytes,
                limit: self.frame_limit,
            });
        }
        let frame = std::mem::take(&mut self.pending);
        match parse_control_frame(&frame) {
            Ok(control) => control,
            Err(error) => DecodedControlFrame::Rejected(error),
        }
    }
}

impl Default for ShellControlDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ShellControlDecoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShellControlDecoder")
            .field("pending_bytes", &self.pending.len())
            .field("oversized_bytes", &self.oversized_bytes)
            .field("frame_limit", &self.frame_limit)
            .finish()
    }
}

fn validate_replacement(buffer: &str, cursor: usize) -> Result<(), ControlEncodeError> {
    if buffer.len() > MAX_CONTROL_BUFFER_BYTES {
        return Err(ControlEncodeError::BufferTooLarge {
            bytes: buffer.len(),
            limit: MAX_CONTROL_BUFFER_BYTES,
        });
    }
    if buffer.as_bytes().contains(&0) {
        return Err(ControlEncodeError::NulBuffer);
    }
    if !buffer.is_char_boundary(cursor) {
        return Err(ControlEncodeError::InvalidCursor {
            cursor,
            buffer_bytes: buffer.len(),
        });
    }
    Ok(())
}

fn encode_lower_hex(source: &[u8], destination: &mut Vec<u8>) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for &byte in source {
        destination.push(DIGITS[usize::from(byte >> 4)]);
        destination.push(DIGITS[usize::from(byte & 0x0f)]);
    }
}

fn parse_control_frame(frame: &[u8]) -> Result<DecodedControlFrame, ControlFrameError> {
    if frame.is_empty() {
        return Err(ControlFrameError::EmptyFrame);
    }
    if let Some(request) = frame.strip_prefix(PROBE_RESYNC_CONTROL_PREFIX) {
        if request.contains(&b':') {
            return Err(ControlFrameError::InvalidGrammar);
        }
        let request = parse_decimal(request).ok_or(ControlFrameError::InvalidRequestId)?;
        let request_id = ProbeResyncRequestId::new(request)
            .map_err(|_| ControlFrameError::RequestIdOutOfRange)?;
        return Ok(DecodedControlFrame::ProbeResync(ProbeResyncControl::new(
            request_id,
        )));
    }
    let Some(fields) = frame.strip_prefix(REPLACEMENT_CONTROL_PREFIX) else {
        if frame.starts_with(b"argmax-control-") {
            return Err(ControlFrameError::UnsupportedProtocol);
        }
        return Err(ControlFrameError::WrongDirection);
    };
    let mut fields = fields.split(|byte| *byte == b':');
    let request = fields.next().ok_or(ControlFrameError::InvalidGrammar)?;
    let cursor = fields.next().ok_or(ControlFrameError::InvalidGrammar)?;
    let length = fields.next().ok_or(ControlFrameError::InvalidGrammar)?;
    let hex = fields.next().ok_or(ControlFrameError::InvalidGrammar)?;
    if fields.next().is_some() {
        return Err(ControlFrameError::InvalidGrammar);
    }

    let request = parse_decimal(request).ok_or(ControlFrameError::InvalidRequestId)?;
    let request_id =
        ControlRequestId::new(request).map_err(|_| ControlFrameError::RequestIdOutOfRange)?;
    let cursor = parse_decimal_usize(cursor).ok_or(ControlFrameError::InvalidCursor)?;
    let length = parse_decimal_usize(length).ok_or(ControlFrameError::InvalidLength)?;
    if length > MAX_CONTROL_BUFFER_BYTES {
        return Err(ControlFrameError::BufferTooLarge {
            bytes: length,
            limit: MAX_CONTROL_BUFFER_BYTES,
        });
    }
    if hex.len() != length.saturating_mul(2) {
        return Err(ControlFrameError::HexLengthMismatch);
    }
    let mut decoded = Vec::with_capacity(length);
    for pair in hex.chunks_exact(2) {
        let high = decode_lower_hex_digit(pair[0]).ok_or(ControlFrameError::InvalidHex)?;
        let low = decode_lower_hex_digit(pair[1]).ok_or(ControlFrameError::InvalidHex)?;
        decoded.push((high << 4) | low);
    }
    if decoded.contains(&0) {
        return Err(ControlFrameError::NulBuffer);
    }
    let buffer = String::from_utf8(decoded).map_err(|_| ControlFrameError::InvalidUtf8)?;
    if cursor > buffer.len() {
        return Err(ControlFrameError::CursorOutOfRange);
    }
    if !buffer.is_char_boundary(cursor) {
        return Err(ControlFrameError::CursorNotUtf8Boundary);
    }
    Ok(DecodedControlFrame::Replacement(ReplacementControl {
        request_id,
        buffer: buffer.into_boxed_str(),
        cursor,
    }))
}

fn parse_decimal(bytes: &[u8]) -> Option<u64> {
    if !is_canonical_decimal(bytes) {
        return None;
    }
    bytes.iter().try_fold(0_u64, |value, byte| {
        value.checked_mul(10)?.checked_add(u64::from(*byte - b'0'))
    })
}

fn parse_decimal_usize(bytes: &[u8]) -> Option<usize> {
    if !is_canonical_decimal(bytes) {
        return None;
    }
    bytes.iter().try_fold(0_usize, |value, byte| {
        value
            .checked_mul(10)?
            .checked_add(usize::from(*byte - b'0'))
    })
}

fn is_canonical_decimal(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.iter().all(u8::is_ascii_digit)
        && (bytes.len() == 1 || bytes[0] != b'0')
}

const fn decode_lower_hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identifier(value: u64) -> ControlRequestId {
        ControlRequestId::new(value).unwrap()
    }

    fn resync_identifier(value: u64) -> ProbeResyncRequestId {
        ProbeResyncRequestId::new(value).unwrap()
    }

    fn replacement(value: &str, cursor: usize) -> ReplacementControl {
        ReplacementControl::new(identifier(7), value, cursor).unwrap()
    }

    fn decode(wire: &[u8]) -> Vec<DecodedControlFrame> {
        let mut decoder = ShellControlDecoder::new();
        let mut frames = Vec::new();
        decoder.push(wire, |frame| frames.push(frame));
        frames
    }

    #[test]
    fn round_trips_unicode_multiline_and_byte_cursor() {
        let control = replacement("echo 世界\nprintf 'Dean Pelton'", 11);
        let encoded = control.encode();
        let decoded = decode(encoded.as_bytes());

        assert_eq!(decoded, [DecodedControlFrame::Replacement(control)]);
        assert!(encoded.as_bytes().starts_with(REPLACEMENT_CONTROL_PREFIX));
        assert!(encoded.as_bytes().ends_with(&[0]));
        assert!(
            encoded.as_bytes()[..encoded.len() - 1]
                .iter()
                .all(u8::is_ascii)
        );
    }

    #[test]
    fn empty_buffer_and_end_cursor_are_valid() {
        let control = replacement("", 0);
        assert_eq!(
            decode(control.encode().as_bytes()),
            [DecodedControlFrame::Replacement(control),]
        );

        let control = replacement("Troy \u{1f680}", "Troy \u{1f680}".len());
        assert_eq!(
            decode(control.encode().as_bytes()),
            [DecodedControlFrame::Replacement(control),]
        );
    }

    #[test]
    fn encoder_rejects_nul_size_and_nonboundary_cursor() {
        assert_eq!(
            ReplacementControl::new(identifier(1), "a\0b", 1),
            Err(ControlEncodeError::NulBuffer)
        );
        assert_eq!(
            ReplacementControl::new(identifier(1), "\u{00e9}", 1),
            Err(ControlEncodeError::InvalidCursor {
                cursor: 1,
                buffer_bytes: 2,
            })
        );
        assert_eq!(
            ReplacementControl::new(identifier(1), "x".repeat(16_385), 0),
            Err(ControlEncodeError::BufferTooLarge {
                bytes: 16_385,
                limit: MAX_CONTROL_BUFFER_BYTES,
            })
        );
    }

    #[test]
    fn request_identifiers_have_independent_shared_strict_ranges() {
        assert_eq!(ControlRequestId::new(0), Err(ControlRequestIdError::Zero));
        assert_eq!(
            ControlRequestId::new(MAX_CONTROL_REQUEST_ID + 1),
            Err(ControlRequestIdError::OutOfRange {
                maximum: MAX_CONTROL_REQUEST_ID,
            })
        );
        assert_eq!(
            ControlRequestId::new(MAX_CONTROL_REQUEST_ID).unwrap().get(),
            MAX_CONTROL_REQUEST_ID
        );
        assert_eq!(
            ProbeResyncRequestId::new(0),
            Err(ProbeResyncRequestIdError::Zero)
        );
        assert_eq!(
            ProbeResyncRequestId::new(MAX_PROBE_RESYNC_REQUEST_ID + 1),
            Err(ProbeResyncRequestIdError::OutOfRange {
                maximum: MAX_PROBE_RESYNC_REQUEST_ID,
            })
        );
        assert_eq!(
            ProbeResyncRequestId::new(MAX_PROBE_RESYNC_REQUEST_ID)
                .unwrap()
                .get(),
            MAX_PROBE_RESYNC_REQUEST_ID
        );
    }

    #[test]
    fn resync_round_trips_every_stream_partition() {
        let control = ProbeResyncControl::new(resync_identifier(42));
        let wire = control.encode();
        assert_eq!(wire.as_bytes(), b"argmax-control-v1:resync:42\0");
        for split in 0..=wire.len() {
            let mut decoder = ShellControlDecoder::new();
            let mut frames = Vec::new();
            decoder.push(&wire.as_bytes()[..split], |frame| frames.push(frame));
            decoder.push(&wire.as_bytes()[split..], |frame| frames.push(frame));
            assert_eq!(frames, [DecodedControlFrame::ProbeResync(control)]);
            assert_eq!(decoder.pending_len(), 0);
        }
    }

    #[test]
    fn every_partition_decodes_identically() {
        let control = replacement("git commit\n--amend \u{1f680}", 10);
        let wire = control.encode();
        for split in 0..=wire.len() {
            let mut decoder = ShellControlDecoder::new();
            let mut frames = Vec::new();
            decoder.push(&wire.as_bytes()[..split], |frame| frames.push(frame));
            decoder.push(&wire.as_bytes()[split..], |frame| frames.push(frame));
            assert_eq!(frames, [DecodedControlFrame::Replacement(control.clone())]);
            assert_eq!(decoder.pending_len(), 0);
        }
    }

    #[test]
    fn coalesced_frames_stay_in_wire_order() {
        let first = ReplacementControl::new(identifier(1), "git", 3).unwrap();
        let resync = ProbeResyncControl::new(resync_identifier(9));
        let second = ReplacementControl::new(identifier(2), "git status", 4).unwrap();
        let mut wire = first.encode().as_bytes().to_vec();
        wire.extend_from_slice(resync.encode().as_bytes());
        wire.extend_from_slice(second.encode().as_bytes());

        assert_eq!(
            decode(&wire),
            [
                DecodedControlFrame::Replacement(first),
                DecodedControlFrame::ProbeResync(resync),
                DecodedControlFrame::Replacement(second),
            ]
        );
    }

    #[test]
    fn malformed_frames_are_isolated_and_strict() {
        let cases: &[(&[u8], ControlFrameError)] = &[
            (b"\0", ControlFrameError::EmptyFrame),
            (b"prompt-ready\0", ControlFrameError::WrongDirection),
            (
                b"argmax-control-v2:replace:1:0:0:\0",
                ControlFrameError::UnsupportedProtocol,
            ),
            (
                b"argmax-control-v1:other:1:0:0:\0",
                ControlFrameError::UnsupportedProtocol,
            ),
            (
                b"argmax-control-v1:resync:1:extra\0",
                ControlFrameError::InvalidGrammar,
            ),
            (
                b"argmax-control-v1:resync:\0",
                ControlFrameError::InvalidRequestId,
            ),
            (
                b"argmax-control-v1:resync:01\0",
                ControlFrameError::InvalidRequestId,
            ),
            (
                b"argmax-control-v1:resync:0\0",
                ControlFrameError::RequestIdOutOfRange,
            ),
            (
                b"argmax-control-v1:resync:2147483648\0",
                ControlFrameError::RequestIdOutOfRange,
            ),
            (
                b"argmax-control-v1:replace:1:0:0::extra\0",
                ControlFrameError::InvalidGrammar,
            ),
            (
                b"argmax-control-v1:replace:01:0:0:\0",
                ControlFrameError::InvalidRequestId,
            ),
            (
                b"argmax-control-v1:replace:0:0:0:\0",
                ControlFrameError::RequestIdOutOfRange,
            ),
            (
                b"argmax-control-v1:replace:1:00:0:\0",
                ControlFrameError::InvalidCursor,
            ),
            (
                b"argmax-control-v1:replace:1:0:00:\0",
                ControlFrameError::InvalidLength,
            ),
            (
                b"argmax-control-v1:replace:1:0:1:7A\0",
                ControlFrameError::InvalidHex,
            ),
            (
                b"argmax-control-v1:replace:1:0:2:61\0",
                ControlFrameError::HexLengthMismatch,
            ),
            (
                b"argmax-control-v1:replace:1:0:1:00\0",
                ControlFrameError::NulBuffer,
            ),
            (
                b"argmax-control-v1:replace:1:0:1:ff\0",
                ControlFrameError::InvalidUtf8,
            ),
            (
                b"argmax-control-v1:replace:1:3:2:c3a9\0",
                ControlFrameError::CursorOutOfRange,
            ),
            (
                b"argmax-control-v1:replace:1:1:2:c3a9\0",
                ControlFrameError::CursorNotUtf8Boundary,
            ),
        ];

        for (wire, want) in cases {
            assert_eq!(decode(wire), [DecodedControlFrame::Rejected(*want)]);
        }
    }

    #[test]
    fn oversized_frame_retains_no_payload_and_recovers() {
        let mut decoder = ShellControlDecoder::with_frame_limit(8);
        let mut frames = Vec::new();
        decoder.push(b"0123456789", |frame| frames.push(frame));
        assert!(decoder.is_discarding());
        assert_eq!(decoder.pending_len(), 0);
        decoder.push(b"\0bad\0", |frame| frames.push(frame));
        assert_eq!(
            frames,
            [
                DecodedControlFrame::Rejected(ControlFrameError::FrameTooLarge {
                    observed_bytes: 10,
                    limit: 8,
                }),
                DecodedControlFrame::Rejected(ControlFrameError::WrongDirection),
            ]
        );
    }

    #[test]
    fn finish_rejects_partial_without_retaining_content() {
        let mut decoder = ShellControlDecoder::new();
        decoder.push(b"secret-control", |_| unreachable!());
        assert_eq!(decoder.pending_len(), 14);
        assert_eq!(
            decoder.finish(),
            Some(DecodedControlFrame::Rejected(
                ControlFrameError::TruncatedFrame { observed_bytes: 14 },
            ))
        );
        assert_eq!(decoder.pending_len(), 0);
        assert!(decoder.finish().is_none());
    }

    #[test]
    fn debug_output_never_exposes_replacement_content() {
        let secret = "Dean Pelton private token";
        let control = replacement(secret, secret.len());
        let encoded = control.encode();
        let resync = ProbeResyncControl::new(resync_identifier(17));
        let mut decoder = ShellControlDecoder::new();
        decoder.push(&encoded.as_bytes()[..8], |_| unreachable!());

        for debug in [
            format!("{control:?}"),
            format!("{encoded:?}"),
            format!("{resync:?}"),
            format!("{decoder:?}"),
        ] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn maximum_frame_matches_encoded_upper_bound() {
        assert_eq!(MAX_CONTROL_FRAME_BYTES, 32_817);
        assert_eq!(MAX_CONTROL_WIRE_BYTES, 32_818);
        let control = ReplacementControl::new(
            identifier(MAX_CONTROL_REQUEST_ID),
            "x".repeat(MAX_CONTROL_BUFFER_BYTES),
            MAX_CONTROL_BUFFER_BYTES,
        )
        .unwrap();
        assert_eq!(control.encode().len() - 1, MAX_CONTROL_FRAME_BYTES);
        assert!(
            ProbeResyncControl::new(resync_identifier(MAX_PROBE_RESYNC_REQUEST_ID))
                .encode()
                .len()
                - 1
                < MAX_CONTROL_FRAME_BYTES
        );
    }
}
