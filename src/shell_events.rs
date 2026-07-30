//! Bounded decoding and reduction of shell-integration events.
//!
//! Shell hooks write NUL-terminated frames on a private file descriptor. Buffer
//! payloads remain raw bytes, while authoritative editing snapshots are accepted
//! only when their UTF-8 cursor is valid. The reducer consumes every decoded
//! frame so a rejected frame cannot leave stale suggestions authoritative.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

/// Hard maximum size of one frame, excluding its NUL terminator.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

/// Reserved input sequence that asks a shell-native editor for a snapshot.
///
/// A PTY input loop may inject this only after a complete editing key has been
/// forwarded and decoded. It must never inject it during bracketed paste, while
/// a foreground command owns the terminal, or after forwarding Enter.
pub const SYNC_PROBE_SEQUENCE: &[u8] = b"\x1b[argmax-sync~";

const BUFFER_BYTE_PREFIX: &[u8] = b"buffer:b:";
const BUFFER_CHARACTER_PREFIX: &[u8] = b"buffer:c:";
const PROBE_BUFFER_BYTE_PREFIX: &[u8] = b"probe-buffer:b:";
const PROBE_BUFFER_CHARACTER_PREFIX: &[u8] = b"probe-buffer:c:";
const PROBE_BUFFER_FISH_PREFIX: &[u8] = b"probe-buffer:f:";
const CAPABILITY_NATIVE: &[u8] = b"capability:native-buffer";
const CAPABILITY_PROBE_PREFIX: &[u8] = b"capability:sync-probe:";
const CAPABILITY_UNAVAILABLE: &[u8] = b"capability:unavailable";
const COMMAND_START_PREFIX: &[u8] = b"command-start:";
const COMMAND_START_UNKNOWN: &[u8] = b"command-start-unknown";
const COMMAND_STOP_PREFIX: &[u8] = b"command-stop:";
const WORKING_DIRECTORY_PREFIX: &[u8] = b"cwd:";
const PROMPT_READY: &[u8] = b"prompt-ready";
const RELOAD_REQUEST_PREFIX: &[u8] = crate::reload::RELOAD_REQUEST_PREFIX;
const MAX_WORKING_DIRECTORY_BYTES: usize = 16 * 1024;

/// Exact submitted command bytes reported by a shell lifecycle hook.
#[derive(Clone, Eq, PartialEq)]
pub struct SubmittedCommand(Vec<u8>);

impl fmt::Debug for SubmittedCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubmittedCommand")
            .field("byte_count", &self.0.len())
            .finish()
    }
}

impl SubmittedCommand {
    /// Returns the command bytes without lossy decoding.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns the command as text only when its bytes are valid UTF-8.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }

    /// Returns the command length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether the submitted command is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Monotonic nonce echoed by a reserved synchronization probe.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SnapshotNonce(u64);

impl SnapshotNonce {
    /// Returns the numeric nonce.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Cursor-bearing editing snapshot reported by a shell adapter.
///
/// A decoded value becomes authoritative only after [`ShellSessionState`]
/// correlates it with the active capability and local-input generation.
#[derive(Clone, Eq, PartialEq)]
pub struct BufferSnapshot {
    bytes: Vec<u8>,
    cursor: usize,
    probe_nonce: Option<SnapshotNonce>,
}

impl fmt::Debug for BufferSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BufferSnapshot")
            .field("byte_count", &self.bytes.len())
            .field("cursor", &self.cursor)
            .field("probe_nonce", &self.probe_nonce)
            .finish()
    }
}

impl BufferSnapshot {
    fn empty() -> Self {
        Self {
            bytes: Vec::new(),
            cursor: 0,
            probe_nonce: None,
        }
    }

    /// Returns the buffer bytes without lossy decoding.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the buffer as text only when its bytes are valid UTF-8.
    ///
    /// Valid decoded snapshots always return `Some`; the optional result keeps
    /// callers from accidentally assuming that arbitrary shell bytes are text.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes).ok()
    }

    /// Returns the validated UTF-8 byte offset of the cursor.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Returns the correlated probe nonce, or `None` for a native snapshot.
    #[must_use]
    pub const fn probe_nonce(&self) -> Option<SnapshotNonce> {
        self.probe_nonce
    }

    /// Returns the exact bytes before the cursor.
    #[must_use]
    pub fn before_cursor(&self) -> &[u8] {
        &self.bytes[..self.cursor]
    }

    /// Returns the exact bytes at and after the cursor.
    #[must_use]
    pub fn after_cursor(&self) -> &[u8] {
        &self.bytes[self.cursor..]
    }

    /// Returns the buffer length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// Validated shell exit status in the portable 8-bit range.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ShellExitStatus(u8);

impl ShellExitStatus {
    /// Returns the numeric status reported by the shell.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns whether the command completed successfully.
    #[must_use]
    pub const fn success(self) -> bool {
        self.0 == 0
    }
}

/// Validated absolute working directory reported at a shell prompt boundary.
#[derive(Clone, Eq, PartialEq)]
pub struct ShellWorkingDirectory(PathBuf);

impl ShellWorkingDirectory {
    /// Returns the exact path without lossy decoding.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Consumes the event and returns its exact path.
    #[must_use]
    pub fn into_path(self) -> PathBuf {
        self.0
    }
}

impl fmt::Debug for ShellWorkingDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShellWorkingDirectory")
            .field("path_bytes", &self.0.as_os_str().as_encoded_bytes().len())
            .finish()
    }
}

/// Correlation nonce carried by an explicit active-session reload request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReloadRequest(u32);

impl ReloadRequest {
    /// Returns the request nonce echoed by the wrapper acknowledgment.
    #[must_use]
    pub const fn nonce(self) -> u32 {
        self.0
    }
}

/// Runtime shell-adapter support for authoritative editing snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferSyncCapability {
    /// Capability has not yet been announced.
    Unknown,
    /// The shell reports native redraw callbacks without wire correlation.
    ///
    /// Such a callback is rejected after locally forwarded input remains
    /// unacknowledged; current adapters use numbered probes for live editing.
    Native,
    /// The reserved probe is bound without replacing a user binding.
    Probe,
    /// No safe native callback or collision-free probe binding is available.
    Unavailable,
}

impl BufferSyncCapability {
    /// Returns whether the adapter can provide authoritative live snapshots.
    #[must_use]
    pub const fn supports_live_snapshots(self) -> bool {
        matches!(self, Self::Native | Self::Probe)
    }
}

/// One fresh runtime capability handshake for a decoder epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityAnnouncement {
    capability: BufferSyncCapability,
    last_probe_nonce: Option<SnapshotNonce>,
}

impl CapabilityAnnouncement {
    /// Returns the announced synchronization capability.
    #[must_use]
    pub const fn capability(self) -> BufferSyncCapability {
        self.capability
    }

    /// Returns the probe counter base announced by a probe adapter.
    #[must_use]
    pub const fn last_probe_nonce(self) -> Option<SnapshotNonce> {
        self.last_probe_nonce
    }
}

/// One syntactically and semantically validated shell event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellEvent {
    /// A cursor-bearing editing snapshot awaiting reducer correlation.
    Buffer(BufferSnapshot),
    /// The prompt is ready with an empty editing buffer.
    PromptReady,
    /// A command started, with exact submitted bytes where the shell exposes them.
    CommandStart(SubmittedCommand),
    /// A command started but this shell could not expose exact bytes at preexec.
    CommandStartUnknown,
    /// A foreground command stopped with the supplied status.
    CommandStop(ShellExitStatus),
    /// The shell adapter announced its live synchronization capability.
    Capability(CapabilityAnnouncement),
    /// The shell reported its current absolute working directory.
    WorkingDirectory(ShellWorkingDirectory),
    /// An inherited child requested a correlated live configuration reload.
    ReloadRequest(ReloadRequest),
}

/// Monotonic identifier for one decoder stream lifetime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StreamEpoch(u64);

impl StreamEpoch {
    /// Epoch for the first decoder in a session.
    pub const INITIAL: Self = Self(0);

    /// Returns the numeric epoch.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the next epoch, or `None` after the numeric space is exhausted.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(epoch) => Some(Self(epoch)),
            None => None,
        }
    }
}

/// Unique position of a frame within a session stream.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FramePosition {
    epoch: StreamEpoch,
    sequence: u128,
}

impl FramePosition {
    /// Returns the stream epoch.
    #[must_use]
    pub const fn epoch(self) -> StreamEpoch {
        self.epoch
    }

    /// Returns the zero-based sequence within the epoch.
    #[must_use]
    pub const fn sequence(self) -> u128 {
        self.sequence
    }
}

/// Authority generation advanced before each locally forwarded editing input.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InputGeneration {
    epoch: StreamEpoch,
    sequence: u64,
}

impl InputGeneration {
    /// Returns the stream epoch in which this generation was issued.
    #[must_use]
    pub const fn epoch(self) -> StreamEpoch {
        self.epoch
    }

    /// Returns the local-input sequence within the epoch.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

/// A valid event paired with its stream position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequencedShellEvent {
    position: FramePosition,
    event: ShellEvent,
}

impl SequencedShellEvent {
    /// Returns this event's stream position.
    #[must_use]
    pub const fn position(&self) -> FramePosition {
        self.position
    }

    /// Borrows the decoded event.
    #[must_use]
    pub const fn event(&self) -> &ShellEvent {
        &self.event
    }
}

/// Reason a complete protocol frame was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// The frame contained no bytes before its terminator.
    EmptyFrame,
    /// The frame did not have a recognized event name.
    UnknownEvent,
    /// A cursor field was absent or lacked its payload separator.
    MissingCursor,
    /// A cursor contained something other than ASCII decimal digits.
    NonDecimalCursor,
    /// A cursor was outside the reported buffer.
    CursorOutOfRange,
    /// A byte cursor split a UTF-8 code point.
    CursorNotUtf8Boundary,
    /// An authoritative editing snapshot was not valid UTF-8.
    InvalidBufferUtf8,
    /// A probe snapshot or handshake omitted its decimal nonce.
    MissingProbeNonce,
    /// A probe nonce contained something other than ASCII decimal digits.
    NonDecimalProbeNonce,
    /// A probe nonce exceeded the supported numeric range.
    ProbeNonceOutOfRange,
    /// A Fish snapshot omitted the one print terminator required by its frame.
    MissingFishPrintTerminator,
    /// `command-stop:` had no status value.
    MissingExitStatus,
    /// A command status contained something other than ASCII decimal digits.
    NonDecimalExitStatus,
    /// A decimal command status was outside the shell's 0 through 255 range.
    ExitStatusOutOfRange,
    /// A working-directory frame was empty, relative, or exceeded its bound.
    InvalidWorkingDirectory,
    /// A reload request nonce was missing or not an unsigned 32-bit integer.
    InvalidReloadRequest,
    /// A lifecycle frame claimed an empty submitted command.
    EmptySubmittedCommand,
    /// The frame exceeded the configured byte limit.
    FrameTooLarge {
        /// Number of bytes observed, saturated at [`usize::MAX`].
        observed_bytes: usize,
        /// Configured maximum frame size.
        limit: usize,
    },
    /// End of input occurred before a frame terminator.
    TruncatedFrame {
        /// Number of bytes buffered at end of input.
        observed_bytes: usize,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFrame => formatter.write_str("empty shell event frame"),
            Self::UnknownEvent => formatter.write_str("unknown shell event"),
            Self::MissingCursor => formatter.write_str("shell buffer cursor is missing"),
            Self::NonDecimalCursor => {
                formatter.write_str("shell buffer cursor is not unsigned decimal")
            }
            Self::CursorOutOfRange => formatter.write_str("shell buffer cursor is out of range"),
            Self::CursorNotUtf8Boundary => {
                formatter.write_str("shell buffer cursor splits a UTF-8 code point")
            }
            Self::InvalidBufferUtf8 => formatter.write_str("shell buffer is not valid UTF-8"),
            Self::MissingProbeNonce => formatter.write_str("shell snapshot probe nonce is missing"),
            Self::NonDecimalProbeNonce => {
                formatter.write_str("shell snapshot probe nonce is not unsigned decimal")
            }
            Self::ProbeNonceOutOfRange => {
                formatter.write_str("shell snapshot probe nonce is out of range")
            }
            Self::MissingFishPrintTerminator => {
                formatter.write_str("Fish shell snapshot print terminator is missing")
            }
            Self::MissingExitStatus => formatter.write_str("shell exit status is missing"),
            Self::NonDecimalExitStatus => {
                formatter.write_str("shell exit status is not unsigned decimal")
            }
            Self::ExitStatusOutOfRange => {
                formatter.write_str("shell exit status is outside 0 through 255")
            }
            Self::InvalidWorkingDirectory => {
                formatter.write_str("shell working directory is not a bounded absolute path")
            }
            Self::InvalidReloadRequest => {
                formatter.write_str("active-session reload request is invalid")
            }
            Self::EmptySubmittedCommand => formatter.write_str("submitted command is empty"),
            Self::FrameTooLarge {
                observed_bytes,
                limit,
            } => write!(
                formatter,
                "shell event frame is {observed_bytes} bytes; limit is {limit}"
            ),
            Self::TruncatedFrame { observed_bytes } => write!(
                formatter,
                "shell event frame ended after {observed_bytes} bytes without a terminator"
            ),
        }
    }
}

impl Error for FrameError {}

/// A rejected frame paired with its stream position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectedFrame {
    position: FramePosition,
    error: FrameError,
}

impl RejectedFrame {
    /// Returns this frame's stream position.
    #[must_use]
    pub const fn position(&self) -> FramePosition {
        self.position
    }

    /// Returns why the frame was rejected.
    #[must_use]
    pub const fn error(&self) -> &FrameError {
        &self.error
    }
}

/// One complete result from the framed byte stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodedFrame {
    /// A valid event.
    Event(SequencedShellEvent),
    /// An isolated protocol error; later frames remain decodable.
    Rejected(RejectedFrame),
}

impl DecodedFrame {
    /// Returns the frame's stream position.
    #[must_use]
    pub const fn position(&self) -> FramePosition {
        match self {
            Self::Event(event) => event.position(),
            Self::Rejected(frame) => frame.position(),
        }
    }
}

/// Failure to advance a decoder or reducer stream epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamResetError {
    /// No later numeric epoch exists.
    EpochExhausted,
    /// A reducer reset did not provide a newer epoch.
    NonIncreasingEpoch,
}

impl fmt::Display for StreamResetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EpochExhausted => formatter.write_str("shell event stream epoch is exhausted"),
            Self::NonIncreasingEpoch => formatter.write_str("new shell event epoch must increase"),
        }
    }
}

impl Error for StreamResetError {}

/// Failure to advance local editing authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputGenerationError {
    /// Every local-input generation in the active epoch was issued.
    Exhausted,
}

impl fmt::Display for InputGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("local shell-input generation is exhausted")
    }
}

impl Error for InputGenerationError {}

/// A reserved synchronization probe could not be issued safely.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeRequestError {
    /// The active adapter did not complete a probe-capability handshake.
    CapabilityUnavailable,
    /// The shell is not at an idle editable prompt.
    NotAtEditablePrompt,
    /// A prior probe response is still outstanding.
    AlreadyPending,
    /// The adapter's probe nonce space was exhausted.
    NonceExhausted,
}

impl fmt::Display for ProbeRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CapabilityUnavailable => "shell snapshot probe capability is unavailable",
            Self::NotAtEditablePrompt => "shell is not at an editable prompt",
            Self::AlreadyPending => "a shell snapshot probe is already pending",
            Self::NonceExhausted => "shell snapshot probe nonce is exhausted",
        })
    }
}

impl Error for ProbeRequestError {}

/// Incrementally decodes NUL-framed shell events with bounded retained storage.
pub struct ShellEventDecoder {
    pending: Vec<u8>,
    oversized_bytes: Option<usize>,
    frame_limit: usize,
    epoch: StreamEpoch,
    next_sequence: u128,
}

impl fmt::Debug for ShellEventDecoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShellEventDecoder")
            .field("pending_bytes", &self.pending.len())
            .field("oversized_bytes", &self.oversized_bytes)
            .field("frame_limit", &self.frame_limit)
            .field("epoch", &self.epoch)
            .field("next_sequence", &self.next_sequence)
            .finish()
    }
}

impl ShellEventDecoder {
    /// Creates a decoder for an explicit stream epoch at the hard frame limit.
    #[must_use]
    pub fn new(epoch: StreamEpoch) -> Self {
        Self::with_frame_limit(epoch, MAX_FRAME_BYTES)
    }

    /// Creates a decoder with an explicit, hard-capped frame limit.
    #[must_use]
    pub fn with_frame_limit(epoch: StreamEpoch, frame_limit: usize) -> Self {
        let frame_limit = frame_limit.min(MAX_FRAME_BYTES);
        Self {
            pending: Vec::with_capacity(frame_limit.min(4 * 1024)),
            oversized_bytes: None,
            frame_limit,
            epoch,
            next_sequence: 0,
        }
    }

    /// Returns the current stream epoch.
    #[must_use]
    pub const fn epoch(&self) -> StreamEpoch {
        self.epoch
    }

    /// Returns the configured maximum retained frame bytes.
    #[must_use]
    pub const fn frame_limit(&self) -> usize {
        self.frame_limit
    }

    /// Returns the number of bytes retained for an incomplete frame.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Returns whether an oversized frame is being discarded through its NUL.
    #[must_use]
    pub const fn is_discarding(&self) -> bool {
        self.oversized_bytes.is_some()
    }

    /// Consumes a stream chunk and emits each completed result immediately.
    ///
    /// The decoder retains no result batch, so a chunk containing many tiny or
    /// malformed frames cannot amplify decoder-owned output memory. `emit` is
    /// called synchronously in wire order.
    pub fn push<F>(&mut self, chunk: &[u8], mut emit: F)
    where
        F: FnMut(DecodedFrame),
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

    /// Flushes one unterminated frame at end of input and clears it.
    pub fn finish(&mut self) -> Option<DecodedFrame> {
        if let Some(observed_bytes) = self.oversized_bytes.take() {
            return Some(self.rejected(FrameError::FrameTooLarge {
                observed_bytes,
                limit: self.frame_limit,
            }));
        }
        if self.pending.is_empty() {
            return None;
        }

        let observed_bytes = self.pending.len();
        self.pending.clear();
        Some(self.rejected(FrameError::TruncatedFrame { observed_bytes }))
    }

    /// Discards partial input and advances to a fresh stream epoch.
    ///
    /// Pass the returned epoch to [`ShellSessionState::reset_stream`] before
    /// applying frames from the reset decoder.
    ///
    /// # Errors
    ///
    /// Returns [`StreamResetError::EpochExhausted`] if no later epoch exists.
    pub fn reset_stream(&mut self) -> Result<StreamEpoch, StreamResetError> {
        let epoch = self.epoch.next().ok_or(StreamResetError::EpochExhausted)?;
        self.pending.clear();
        self.oversized_bytes = None;
        self.epoch = epoch;
        self.next_sequence = 0;
        Ok(epoch)
    }

    fn finish_terminated_frame(&mut self) -> DecodedFrame {
        if let Some(observed_bytes) = self.oversized_bytes.take() {
            return self.rejected(FrameError::FrameTooLarge {
                observed_bytes,
                limit: self.frame_limit,
            });
        }

        let frame = std::mem::take(&mut self.pending);
        match parse_frame(frame) {
            Ok(event) => {
                let position = self.take_position();
                DecodedFrame::Event(SequencedShellEvent { position, event })
            }
            Err(error) => self.rejected(error),
        }
    }

    fn rejected(&mut self, error: FrameError) -> DecodedFrame {
        let position = self.take_position();
        DecodedFrame::Rejected(RejectedFrame { position, error })
    }

    fn take_position(&mut self) -> FramePosition {
        let position = FramePosition {
            epoch: self.epoch,
            sequence: self.next_sequence,
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        position
    }
}

fn parse_frame(mut frame: Vec<u8>) -> Result<ShellEvent, FrameError> {
    if frame.is_empty() {
        return Err(FrameError::EmptyFrame);
    }
    if frame.starts_with(PROBE_BUFFER_BYTE_PREFIX) {
        frame.drain(..PROBE_BUFFER_BYTE_PREFIX.len());
        return parse_probe_buffer(frame, CursorUnit::Bytes).map(ShellEvent::Buffer);
    }
    if frame.starts_with(PROBE_BUFFER_CHARACTER_PREFIX) {
        frame.drain(..PROBE_BUFFER_CHARACTER_PREFIX.len());
        return parse_probe_buffer(frame, CursorUnit::Characters).map(ShellEvent::Buffer);
    }
    if frame.starts_with(PROBE_BUFFER_FISH_PREFIX) {
        frame.drain(..PROBE_BUFFER_FISH_PREFIX.len());
        return parse_probe_buffer(frame, CursorUnit::FishCharacters).map(ShellEvent::Buffer);
    }
    if frame.starts_with(BUFFER_BYTE_PREFIX) {
        frame.drain(..BUFFER_BYTE_PREFIX.len());
        return parse_buffer(frame, CursorUnit::Bytes, None).map(ShellEvent::Buffer);
    }
    if frame.starts_with(BUFFER_CHARACTER_PREFIX) {
        frame.drain(..BUFFER_CHARACTER_PREFIX.len());
        return parse_buffer(frame, CursorUnit::Characters, None).map(ShellEvent::Buffer);
    }
    if frame == PROMPT_READY {
        return Ok(ShellEvent::PromptReady);
    }
    if let Some(path) = frame.strip_prefix(WORKING_DIRECTORY_PREFIX) {
        if path.is_empty() || path.len() > MAX_WORKING_DIRECTORY_BYTES {
            return Err(FrameError::InvalidWorkingDirectory);
        }
        let path = PathBuf::from(OsString::from_vec(path.to_vec()));
        if !path.is_absolute() {
            return Err(FrameError::InvalidWorkingDirectory);
        }
        return Ok(ShellEvent::WorkingDirectory(ShellWorkingDirectory(path)));
    }
    if let Some(nonce) = frame.strip_prefix(RELOAD_REQUEST_PREFIX) {
        if nonce.is_empty()
            || nonce.len() > 10
            || !nonce.iter().all(u8::is_ascii_digit)
            || nonce.first() == Some(&b'0') && nonce.len() != 1
        {
            return Err(FrameError::InvalidReloadRequest);
        }
        let nonce = std::str::from_utf8(nonce)
            .ok()
            .and_then(|nonce| nonce.parse::<u32>().ok())
            .filter(|nonce| *nonce != 0)
            .ok_or(FrameError::InvalidReloadRequest)?;
        return Ok(ShellEvent::ReloadRequest(ReloadRequest(nonce)));
    }
    if frame == COMMAND_START_UNKNOWN {
        return Ok(ShellEvent::CommandStartUnknown);
    }
    if frame.starts_with(COMMAND_START_PREFIX) {
        frame.drain(..COMMAND_START_PREFIX.len());
        if frame.is_empty() {
            return Err(FrameError::EmptySubmittedCommand);
        }
        return Ok(ShellEvent::CommandStart(SubmittedCommand(frame)));
    }
    if frame == CAPABILITY_NATIVE {
        return Ok(ShellEvent::Capability(CapabilityAnnouncement {
            capability: BufferSyncCapability::Native,
            last_probe_nonce: None,
        }));
    }
    if let Some(nonce) = frame.strip_prefix(CAPABILITY_PROBE_PREFIX) {
        return Ok(ShellEvent::Capability(CapabilityAnnouncement {
            capability: BufferSyncCapability::Probe,
            last_probe_nonce: Some(parse_probe_nonce(nonce)?),
        }));
    }
    if frame == CAPABILITY_UNAVAILABLE {
        return Ok(ShellEvent::Capability(CapabilityAnnouncement {
            capability: BufferSyncCapability::Unavailable,
            last_probe_nonce: None,
        }));
    }
    let Some(status) = frame.strip_prefix(COMMAND_STOP_PREFIX) else {
        return Err(FrameError::UnknownEvent);
    };
    parse_exit_status(status).map(ShellEvent::CommandStop)
}

#[derive(Clone, Copy)]
enum CursorUnit {
    Bytes,
    Characters,
    FishCharacters,
}

fn parse_probe_buffer(mut frame: Vec<u8>, unit: CursorUnit) -> Result<BufferSnapshot, FrameError> {
    let Some(separator) = frame.iter().position(|byte| *byte == b':') else {
        return Err(FrameError::MissingProbeNonce);
    };
    let nonce = parse_probe_nonce(&frame[..separator])?;
    frame.drain(..=separator);
    parse_buffer(frame, unit, Some(nonce))
}

fn parse_buffer(
    mut frame: Vec<u8>,
    unit: CursorUnit,
    probe_nonce: Option<SnapshotNonce>,
) -> Result<BufferSnapshot, FrameError> {
    let Some(separator) = frame.iter().position(|byte| *byte == b':') else {
        return Err(FrameError::MissingCursor);
    };
    let cursor = parse_cursor(&frame[..separator])?;
    frame.drain(..=separator);
    if matches!(unit, CursorUnit::FishCharacters) && frame.pop() != Some(b'\n') {
        return Err(FrameError::MissingFishPrintTerminator);
    }
    let bytes = frame;
    let text = std::str::from_utf8(&bytes).map_err(|_| FrameError::InvalidBufferUtf8)?;
    let cursor = match unit {
        CursorUnit::Bytes => {
            if cursor > bytes.len() {
                return Err(FrameError::CursorOutOfRange);
            }
            if !text.is_char_boundary(cursor) {
                return Err(FrameError::CursorNotUtf8Boundary);
            }
            cursor
        }
        CursorUnit::Characters | CursorUnit::FishCharacters => text
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(text.len()))
            .nth(cursor)
            .ok_or(FrameError::CursorOutOfRange)?,
    };
    Ok(BufferSnapshot {
        bytes,
        cursor,
        probe_nonce,
    })
}

fn parse_probe_nonce(bytes: &[u8]) -> Result<SnapshotNonce, FrameError> {
    if bytes.is_empty() {
        return Err(FrameError::MissingProbeNonce);
    }
    if !bytes.iter().all(u8::is_ascii_digit) {
        return Err(FrameError::NonDecimalProbeNonce);
    }
    let mut value = 0_u64;
    for digit in bytes {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(*digit - b'0')))
            .ok_or(FrameError::ProbeNonceOutOfRange)?;
    }
    Ok(SnapshotNonce(value))
}

fn parse_cursor(bytes: &[u8]) -> Result<usize, FrameError> {
    if bytes.is_empty() {
        return Err(FrameError::MissingCursor);
    }
    if !bytes.iter().all(u8::is_ascii_digit) {
        return Err(FrameError::NonDecimalCursor);
    }
    let mut value = 0_usize;
    for digit in bytes {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(usize::from(*digit - b'0')))
            .ok_or(FrameError::CursorOutOfRange)?;
    }
    Ok(value)
}

fn parse_exit_status(status: &[u8]) -> Result<ShellExitStatus, FrameError> {
    if status.is_empty() {
        return Err(FrameError::MissingExitStatus);
    }
    if !status.iter().all(u8::is_ascii_digit) {
        return Err(FrameError::NonDecimalExitStatus);
    }

    let mut value = 0_u16;
    for digit in status {
        value = value.saturating_mul(10) + u16::from(*digit - b'0');
        if value > u16::from(u8::MAX) {
            return Err(FrameError::ExitStatusOutOfRange);
        }
    }
    let value = u8::try_from(value).map_err(|_| FrameError::ExitStatusOutOfRange)?;
    Ok(ShellExitStatus(value))
}

/// Whether reducer state can currently be trusted for suggestions or attribution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SynchronizationState {
    /// State follows an authoritative snapshot or prompt boundary.
    Synchronized,
    /// A rejected or impossible frame invalidated state.
    Desynchronized,
}

/// Whether a foreground command currently owns the terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForegroundCommandState {
    /// No foreground command is active.
    Idle,
    /// A foreground command is active.
    Running,
    /// Lost or impossible protocol state prevents a safe determination.
    Unknown,
}

/// Source used for exact completed-command attribution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributionSource {
    /// Exact bytes supplied directly by the shell's preexec lifecycle event.
    LifecycleFrame,
}

/// A completed command safe to pass to history and learning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletedCommand {
    command: SubmittedCommand,
    status: ShellExitStatus,
    source: AttributionSource,
}

impl CompletedCommand {
    /// Returns the exact submitted command.
    #[must_use]
    pub const fn command(&self) -> &SubmittedCommand {
        &self.command
    }

    /// Returns the reported command status.
    #[must_use]
    pub const fn status(&self) -> ShellExitStatus {
        self.status
    }

    /// Returns how exact command bytes were obtained.
    #[must_use]
    pub const fn source(&self) -> AttributionSource {
        self.source
    }
}

/// Impossible command-lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    /// A second start arrived while a command was already active.
    DuplicateCommandStart,
    /// A stop arrived while no command was active.
    CommandStopWithoutStart,
    /// A buffer snapshot arrived while a foreground command was active.
    BufferWhileCommandRunning,
    /// A prompt boundary arrived before the active command stopped.
    PromptWhileCommandRunning,
}

/// Why a syntactically valid buffer snapshot was not authoritative.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotRejection {
    /// A native snapshot arrived from a probe-only adapter.
    MissingProbeNonce,
    /// A numbered probe response arrived from a native-only adapter.
    UnexpectedProbeNonce(SnapshotNonce),
    /// A native callback could not prove which locally forwarded input it observed.
    UncorrelatedNativeSnapshot {
        /// Local-input generation current when the snapshot arrived.
        current: InputGeneration,
    },
    /// A response did not match the one outstanding probe request.
    ProbeNonceMismatch {
        /// Nonce required by the reducer, if a request was outstanding.
        expected: Option<SnapshotNonce>,
        /// Nonce carried by the response.
        received: SnapshotNonce,
    },
    /// The matching response belonged to input that has since changed.
    StaleProbeGeneration {
        /// Local-input generation that requested the snapshot.
        requested: InputGeneration,
        /// Local-input generation current when the response arrived.
        current: InputGeneration,
    },
}

/// Result of consuming one [`DecodedFrame`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateUpdate {
    /// An authoritative editing snapshot synchronized state.
    BufferSynchronized {
        /// Whether this event recovered previously desynchronized state.
        recovered: bool,
    },
    /// An authoritative prompt boundary synchronized an empty buffer.
    PromptReady {
        /// Whether this event recovered previously desynchronized state.
        recovered: bool,
    },
    /// A foreground command began.
    CommandStarted {
        /// Attribution selected for a later matching stop, if available.
        source: Option<AttributionSource>,
        /// Whether preexec bytes matched a preceding authoritative snapshot.
        preexec_matches_snapshot: Option<bool>,
    },
    /// A command stopped with exact attribution.
    CommandStopped(CompletedCommand),
    /// A command stopped but exact submitted bytes were unavailable.
    CommandStoppedWithoutAttribution(ShellExitStatus),
    /// Adapter live-buffer capability changed.
    CapabilityChanged(BufferSyncCapability),
    /// The shell reported an authoritative prompt working directory.
    WorkingDirectoryChanged(ShellWorkingDirectory),
    /// An active-session child requested a correlated configuration reload.
    ReloadRequested(ReloadRequest),
    /// A rejected protocol frame desynchronized state.
    FrameRejected(FrameError),
    /// A duplicate or impossible lifecycle transition desynchronized state.
    LifecycleRejected(LifecycleError),
    /// A valid lifecycle event was observed while state remained desynchronized.
    LifecycleSuppressed,
    /// A syntactically valid snapshot failed authority correlation.
    SnapshotRejected(SnapshotRejection),
    /// A duplicate, older, or wrong-epoch frame desynchronized state.
    StreamOrderRejected {
        /// Position rejected by the reducer.
        received: FramePosition,
        /// Most recent position accepted in the active epoch, if any.
        last_accepted: Option<FramePosition>,
    },
}

/// Ordered session reducer that owns synchronization and command attribution.
#[derive(Eq, PartialEq)]
// These flags record independent protocol facts rather than alternate states.
#[allow(clippy::struct_excessive_bools)]
pub struct ShellSessionState {
    epoch: StreamEpoch,
    last_position: Option<FramePosition>,
    order_faulted: bool,
    synchronization: SynchronizationState,
    foreground: ForegroundCommandState,
    prompt_ready: bool,
    capability: BufferSyncCapability,
    last_probe_nonce: Option<SnapshotNonce>,
    pending_probe: Option<(SnapshotNonce, InputGeneration)>,
    buffer: Option<BufferSnapshot>,
    input_generation: InputGeneration,
    input_unacknowledged: bool,
    buffer_generation: Option<InputGeneration>,
    buffer_observed_since_prompt: bool,
    attribution: Option<(SubmittedCommand, AttributionSource)>,
    last_status: Option<ShellExitStatus>,
}

impl fmt::Debug for ShellSessionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let attribution_source = self.attribution.as_ref().map(|(_, source)| *source);
        formatter
            .debug_struct("ShellSessionState")
            .field("epoch", &self.epoch)
            .field("last_position", &self.last_position)
            .field("order_faulted", &self.order_faulted)
            .field("synchronization", &self.synchronization)
            .field("foreground", &self.foreground)
            .field("prompt_ready", &self.prompt_ready)
            .field("capability", &self.capability)
            .field("last_probe_nonce", &self.last_probe_nonce)
            .field(
                "pending_probe_nonce",
                &self.pending_probe.map(|(nonce, _)| nonce),
            )
            .field("buffer", &self.buffer)
            .field("input_generation", &self.input_generation)
            .field("input_unacknowledged", &self.input_unacknowledged)
            .field("buffer_generation", &self.buffer_generation)
            .field(
                "buffer_observed_since_prompt",
                &self.buffer_observed_since_prompt,
            )
            .field("attribution_source", &attribution_source)
            .field("last_status", &self.last_status)
            .finish()
    }
}

impl ShellSessionState {
    /// Creates desynchronized state for an explicit decoder epoch.
    #[must_use]
    pub const fn new(epoch: StreamEpoch) -> Self {
        Self {
            epoch,
            last_position: None,
            order_faulted: false,
            synchronization: SynchronizationState::Desynchronized,
            foreground: ForegroundCommandState::Unknown,
            prompt_ready: false,
            capability: BufferSyncCapability::Unknown,
            last_probe_nonce: None,
            pending_probe: None,
            buffer: None,
            input_generation: InputGeneration { epoch, sequence: 0 },
            input_unacknowledged: false,
            buffer_generation: None,
            buffer_observed_since_prompt: false,
            attribution: None,
            last_status: None,
        }
    }

    /// Returns the active stream epoch.
    #[must_use]
    pub const fn epoch(&self) -> StreamEpoch {
        self.epoch
    }

    /// Returns the latest authoritative editing snapshot, if synchronized.
    #[must_use]
    pub const fn buffer(&self) -> Option<&BufferSnapshot> {
        self.buffer.as_ref()
    }

    /// Returns the current local-input authority generation.
    #[must_use]
    pub const fn input_generation(&self) -> InputGeneration {
        self.input_generation
    }

    /// Returns the local-input generation of the accepted buffer snapshot.
    #[must_use]
    pub const fn buffer_generation(&self) -> Option<InputGeneration> {
        self.buffer_generation
    }

    /// Returns whether session state is authoritative.
    #[must_use]
    pub const fn synchronization(&self) -> SynchronizationState {
        self.synchronization
    }

    /// Returns foreground command state.
    #[must_use]
    pub const fn foreground(&self) -> ForegroundCommandState {
        self.foreground
    }

    /// Returns the runtime adapter capability.
    #[must_use]
    pub const fn capability(&self) -> BufferSyncCapability {
        self.capability
    }

    /// Returns the nonce of the one outstanding probe request, if any.
    #[must_use]
    pub const fn pending_probe_nonce(&self) -> Option<SnapshotNonce> {
        match self.pending_probe {
            Some((nonce, _)) => Some(nonce),
            None => None,
        }
    }

    /// Returns the most recent safely applied frame position.
    #[must_use]
    pub const fn last_position(&self) -> Option<FramePosition> {
        self.last_position
    }

    /// Returns the most recently completed command status.
    #[must_use]
    pub const fn last_status(&self) -> Option<ShellExitStatus> {
        self.last_status
    }

    /// Returns whether suggestions may be computed and rendered safely.
    #[must_use]
    pub fn suggestions_allowed(&self) -> bool {
        self.synchronization == SynchronizationState::Synchronized
            && self.foreground == ForegroundCommandState::Idle
            && self.prompt_ready
            && self.capability.supports_live_snapshots()
    }

    /// Invalidates the current snapshot before locally forwarded editing input.
    ///
    /// Callers use the returned generation to correlate downstream query work.
    /// A later authoritative snapshot is tagged with this generation; work for
    /// an older generation must be discarded.
    ///
    /// # Errors
    ///
    /// Returns [`InputGenerationError::Exhausted`] rather than wrapping and
    /// resurrecting old snapshot authority.
    pub fn observe_local_input(&mut self) -> Result<InputGeneration, InputGenerationError> {
        let Some(sequence) = self.input_generation.sequence.checked_add(1) else {
            self.invalidate_authority();
            return Err(InputGenerationError::Exhausted);
        };
        self.input_generation = InputGeneration {
            epoch: self.epoch,
            sequence,
        };
        self.input_unacknowledged = true;
        self.synchronization = SynchronizationState::Desynchronized;
        self.buffer = None;
        self.buffer_generation = None;
        self.buffer_observed_since_prompt = false;
        self.attribution = None;
        Ok(self.input_generation)
    }

    /// Reserves the next nonce before the caller injects [`SYNC_PROBE_SEQUENCE`].
    ///
    /// Exactly one request may be outstanding. Its response is accepted only if
    /// it echoes this nonce and no newer local-input generation has been issued.
    ///
    /// # Errors
    ///
    /// Returns an error when probing is unsupported, unsafe in the current
    /// lifecycle state, already outstanding, or numerically exhausted.
    pub fn begin_sync_probe(&mut self) -> Result<SnapshotNonce, ProbeRequestError> {
        if self.capability != BufferSyncCapability::Probe {
            return Err(ProbeRequestError::CapabilityUnavailable);
        }
        if self.foreground != ForegroundCommandState::Idle || !self.prompt_ready {
            return Err(ProbeRequestError::NotAtEditablePrompt);
        }
        if self.pending_probe.is_some() {
            return Err(ProbeRequestError::AlreadyPending);
        }
        let Some(last_nonce) = self.last_probe_nonce else {
            self.invalidate_editing_authority();
            return Err(ProbeRequestError::CapabilityUnavailable);
        };
        let Some(value) = last_nonce.0.checked_add(1) else {
            self.invalidate_editing_authority();
            return Err(ProbeRequestError::NonceExhausted);
        };
        let nonce = SnapshotNonce(value);
        self.last_probe_nonce = Some(nonce);
        self.pending_probe = Some((nonce, self.input_generation));
        Ok(nonce)
    }

    /// Resets state for a newer decoder epoch and requires new authority.
    ///
    /// # Errors
    ///
    /// Returns [`StreamResetError::NonIncreasingEpoch`] for the active or an
    /// older epoch.
    pub fn reset_stream(&mut self, epoch: StreamEpoch) -> Result<(), StreamResetError> {
        if epoch <= self.epoch {
            return Err(StreamResetError::NonIncreasingEpoch);
        }
        self.epoch = epoch;
        self.last_position = None;
        self.order_faulted = false;
        self.capability = BufferSyncCapability::Unknown;
        self.last_probe_nonce = None;
        self.pending_probe = None;
        self.input_generation = InputGeneration { epoch, sequence: 0 };
        self.input_unacknowledged = false;
        self.buffer_generation = None;
        self.invalidate_authority();
        Ok(())
    }

    /// Consumes every valid or rejected decoded frame in stream order.
    pub fn apply(&mut self, frame: DecodedFrame) -> StateUpdate {
        let received = frame.position();
        let expected_sequence = self
            .last_position
            .map_or(Some(0), |last| last.sequence.checked_add(1));
        if self.order_faulted
            || received.epoch != self.epoch
            || expected_sequence != Some(received.sequence)
        {
            let last_accepted = self.last_position;
            self.order_faulted = true;
            self.invalidate_authority();
            return StateUpdate::StreamOrderRejected {
                received,
                last_accepted,
            };
        }
        self.last_position = Some(received);

        match frame {
            DecodedFrame::Rejected(frame) => {
                self.invalidate_authority();
                StateUpdate::FrameRejected(frame.error)
            }
            DecodedFrame::Event(event) => self.apply_event(event.event),
        }
    }

    fn apply_event(&mut self, event: ShellEvent) -> StateUpdate {
        match event {
            ShellEvent::Buffer(buffer) => self.synchronize_buffer(buffer),
            ShellEvent::PromptReady => self.synchronize_prompt(),
            ShellEvent::Capability(announcement) => self.announce_capability(announcement),
            ShellEvent::CommandStart(command) => self.start_command(Some(command)),
            ShellEvent::CommandStartUnknown => self.start_command(None),
            ShellEvent::CommandStop(status) => self.stop_command(status),
            ShellEvent::WorkingDirectory(directory) => {
                StateUpdate::WorkingDirectoryChanged(directory)
            }
            ShellEvent::ReloadRequest(request) => StateUpdate::ReloadRequested(request),
        }
    }

    fn synchronize_buffer(&mut self, buffer: BufferSnapshot) -> StateUpdate {
        if self.foreground == ForegroundCommandState::Running {
            self.invalidate_authority();
            return StateUpdate::LifecycleRejected(LifecycleError::BufferWhileCommandRunning);
        }
        if let Err(rejection) = self.correlate_snapshot(&buffer) {
            self.invalidate_editing_authority();
            return StateUpdate::SnapshotRejected(rejection);
        }
        if self.foreground != ForegroundCommandState::Idle
            || !self.prompt_ready
            || !self.capability.supports_live_snapshots()
        {
            self.invalidate_editing_authority();
            return StateUpdate::LifecycleSuppressed;
        }

        let recovered = self.synchronization == SynchronizationState::Desynchronized;
        self.synchronization = SynchronizationState::Synchronized;
        self.buffer = Some(buffer);
        self.buffer_generation = Some(self.input_generation);
        self.input_unacknowledged = false;
        self.buffer_observed_since_prompt = true;
        self.attribution = None;
        StateUpdate::BufferSynchronized { recovered }
    }

    fn synchronize_prompt(&mut self) -> StateUpdate {
        if self.foreground == ForegroundCommandState::Running {
            self.invalidate_authority();
            return StateUpdate::LifecycleRejected(LifecycleError::PromptWhileCommandRunning);
        }
        if self.capability == BufferSyncCapability::Unknown {
            self.invalidate_authority();
            return StateUpdate::LifecycleSuppressed;
        }
        if self.input_unacknowledged {
            return StateUpdate::LifecycleSuppressed;
        }

        let recovered = self.synchronization == SynchronizationState::Desynchronized;
        self.synchronization = SynchronizationState::Synchronized;
        self.foreground = ForegroundCommandState::Idle;
        self.prompt_ready = true;
        self.buffer = Some(BufferSnapshot::empty());
        self.buffer_generation = Some(self.input_generation);
        self.buffer_observed_since_prompt = false;
        self.pending_probe = None;
        self.input_unacknowledged = false;
        self.attribution = None;
        StateUpdate::PromptReady { recovered }
    }

    fn announce_capability(&mut self, announcement: CapabilityAnnouncement) -> StateUpdate {
        self.invalidate_editing_authority();
        self.pending_probe = None;
        self.capability = announcement.capability;
        self.last_probe_nonce = announcement.last_probe_nonce;
        StateUpdate::CapabilityChanged(announcement.capability)
    }

    fn correlate_snapshot(&mut self, buffer: &BufferSnapshot) -> Result<(), SnapshotRejection> {
        match (self.capability, buffer.probe_nonce) {
            (BufferSyncCapability::Probe, None) => Err(SnapshotRejection::MissingProbeNonce),
            (BufferSyncCapability::Probe, Some(received)) => {
                let Some((expected, requested_generation)) = self.pending_probe else {
                    return Err(SnapshotRejection::ProbeNonceMismatch {
                        expected: None,
                        received,
                    });
                };
                if received != expected {
                    return Err(SnapshotRejection::ProbeNonceMismatch {
                        expected: Some(expected),
                        received,
                    });
                }
                self.pending_probe = None;
                if requested_generation != self.input_generation {
                    return Err(SnapshotRejection::StaleProbeGeneration {
                        requested: requested_generation,
                        current: self.input_generation,
                    });
                }
                Ok(())
            }
            (BufferSyncCapability::Native, Some(nonce)) => {
                Err(SnapshotRejection::UnexpectedProbeNonce(nonce))
            }
            (BufferSyncCapability::Native, None) if self.input_unacknowledged => {
                Err(SnapshotRejection::UncorrelatedNativeSnapshot {
                    current: self.input_generation,
                })
            }
            (BufferSyncCapability::Native, None)
            | (BufferSyncCapability::Unknown | BufferSyncCapability::Unavailable, _) => Ok(()),
        }
    }

    fn start_command(&mut self, preexec: Option<SubmittedCommand>) -> StateUpdate {
        if self.foreground == ForegroundCommandState::Running {
            self.invalidate_authority();
            return StateUpdate::LifecycleRejected(LifecycleError::DuplicateCommandStart);
        }
        self.input_unacknowledged = false;
        if self.foreground == ForegroundCommandState::Unknown || !self.prompt_ready {
            self.foreground = ForegroundCommandState::Running;
            self.prompt_ready = false;
            self.buffer = None;
            self.buffer_observed_since_prompt = false;
            self.attribution = None;
            return StateUpdate::LifecycleSuppressed;
        }

        let snapshot = if self.buffer_observed_since_prompt {
            self.buffer
                .take()
                .map(|buffer| SubmittedCommand(buffer.bytes))
        } else {
            None
        };
        let preexec_matches_snapshot = snapshot
            .as_ref()
            .zip(preexec.as_ref())
            .map(|(snapshot, preexec)| snapshot == preexec);
        let attribution = preexec.map(|command| (command, AttributionSource::LifecycleFrame));
        let source = attribution.as_ref().map(|(_, source)| *source);

        self.foreground = ForegroundCommandState::Running;
        self.prompt_ready = false;
        self.buffer = None;
        self.buffer_generation = None;
        self.buffer_observed_since_prompt = false;
        self.pending_probe = None;
        self.attribution = attribution;
        StateUpdate::CommandStarted {
            source,
            preexec_matches_snapshot,
        }
    }

    fn stop_command(&mut self, status: ShellExitStatus) -> StateUpdate {
        if self.foreground == ForegroundCommandState::Idle {
            self.invalidate_authority();
            return StateUpdate::LifecycleRejected(LifecycleError::CommandStopWithoutStart);
        }
        if self.foreground == ForegroundCommandState::Unknown {
            self.foreground = ForegroundCommandState::Idle;
            self.prompt_ready = false;
            self.attribution = None;
            return StateUpdate::LifecycleSuppressed;
        }

        self.foreground = ForegroundCommandState::Idle;
        self.prompt_ready = false;
        self.buffer = None;
        self.buffer_generation = None;
        self.last_status = Some(status);
        let Some((command, source)) = self.attribution.take() else {
            return StateUpdate::CommandStoppedWithoutAttribution(status);
        };
        StateUpdate::CommandStopped(CompletedCommand {
            command,
            status,
            source,
        })
    }

    fn invalidate_authority(&mut self) {
        self.synchronization = SynchronizationState::Desynchronized;
        self.foreground = ForegroundCommandState::Unknown;
        self.prompt_ready = false;
        self.buffer = None;
        self.buffer_generation = None;
        self.buffer_observed_since_prompt = false;
        self.pending_probe = None;
        self.attribution = None;
    }

    fn invalidate_editing_authority(&mut self) {
        self.synchronization = SynchronizationState::Desynchronized;
        self.buffer = None;
        self.buffer_generation = None;
        self.buffer_observed_since_prompt = false;
        self.attribution = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder() -> ShellEventDecoder {
        ShellEventDecoder::new(StreamEpoch::INITIAL)
    }

    fn decode(decoder: &mut ShellEventDecoder, bytes: &[u8]) -> Vec<DecodedFrame> {
        let mut frames = Vec::new();
        decoder.push(bytes, |frame| frames.push(frame));
        frames
    }

    fn event(frame: &DecodedFrame) -> &ShellEvent {
        let DecodedFrame::Event(event) = frame else {
            panic!("expected event, got {frame:?}");
        };
        event.event()
    }

    fn error(frame: &DecodedFrame) -> &FrameError {
        let DecodedFrame::Rejected(frame) = frame else {
            panic!("expected rejection, got {frame:?}");
        };
        frame.error()
    }

    #[test]
    fn decodes_chunk_boundaries_and_multiple_protocol_events() {
        let bytes = b"capability:sync-probe:40\0prompt-ready\0probe-buffer:b:41:3:git status\0\
          command-start:git status\0command-stop:17\0";
        let mut decoder = decoder();
        let mut frames = Vec::new();
        for byte in bytes {
            decoder.push(std::slice::from_ref(byte), |frame| frames.push(frame));
            assert!(decoder.pending_len() <= decoder.frame_limit());
        }

        assert_eq!(frames.len(), 5);
        assert_eq!(frames[0].position().sequence(), 0);
        assert_eq!(frames[4].position().sequence(), 4);
        let ShellEvent::Capability(announcement) = event(&frames[0]) else {
            panic!("expected capability announcement");
        };
        assert_eq!(announcement.capability(), BufferSyncCapability::Probe);
        assert_eq!(announcement.last_probe_nonce(), Some(SnapshotNonce(40)));
        assert_eq!(event(&frames[1]), &ShellEvent::PromptReady);
        assert!(matches!(event(&frames[2]), ShellEvent::Buffer(_)));
        let ShellEvent::Buffer(snapshot) = event(&frames[2]) else {
            unreachable!();
        };
        assert_eq!(snapshot.probe_nonce(), Some(SnapshotNonce(41)));
        assert!(matches!(event(&frames[3]), ShellEvent::CommandStart(_)));
        assert_eq!(
            event(&frames[4]),
            &ShellEvent::CommandStop(ShellExitStatus(17))
        );
    }

    #[test]
    fn decodes_bounded_cwd_and_correlated_reload_control_events() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"cwd:/tmp/Greendale Community College\0reload-request:42\0",
        );
        let ShellEvent::WorkingDirectory(directory) = event(&frames[0]) else {
            panic!("expected working directory");
        };
        assert_eq!(
            directory.as_path(),
            Path::new("/tmp/Greendale Community College")
        );
        assert_eq!(
            event(&frames[1]),
            &ShellEvent::ReloadRequest(ReloadRequest(42))
        );

        let rejected = decode(
            &mut decoder,
            b"cwd:relative\0cwd:\0reload-request:\0reload-request:01\0reload-request:4294967296\0",
        );
        assert_eq!(error(&rejected[0]), &FrameError::InvalidWorkingDirectory);
        assert_eq!(error(&rejected[1]), &FrameError::InvalidWorkingDirectory);
        assert!(
            rejected[2..]
                .iter()
                .all(|frame| error(frame) == &FrameError::InvalidReloadRequest)
        );
    }

    #[test]
    fn validates_byte_cursor_range_and_utf8_boundaries() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            "buffer:b:2:éx\0buffer:b:1:éx\0buffer:b:4:éx\0".as_bytes(),
        );

        let ShellEvent::Buffer(snapshot) = event(&frames[0]) else {
            panic!("expected buffer");
        };
        assert_eq!(snapshot.cursor(), 2);
        assert_eq!(snapshot.before_cursor(), "é".as_bytes());
        assert_eq!(snapshot.after_cursor(), b"x");
        assert_eq!(error(&frames[1]), &FrameError::CursorNotUtf8Boundary);
        assert_eq!(error(&frames[2]), &FrameError::CursorOutOfRange);
    }

    #[test]
    fn converts_character_cursor_to_a_valid_byte_boundary() {
        let mut decoder = decoder();
        let frames = decode(&mut decoder, "buffer:c:2:a☃b\0".as_bytes());
        let ShellEvent::Buffer(snapshot) = event(&frames[0]) else {
            panic!("expected buffer");
        };
        assert_eq!(snapshot.cursor(), 4);
        assert_eq!(snapshot.before_cursor(), "a☃".as_bytes());
        assert_eq!(snapshot.as_str(), Some("a☃b"));
    }

    #[test]
    fn fish_probe_preserves_trailing_newlines_and_removes_only_print_terminator() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"probe-buffer:f:9:5:echo\n\n\0probe-buffer:f:10:0:\0",
        );
        let ShellEvent::Buffer(snapshot) = event(&frames[0]) else {
            panic!("expected Fish buffer");
        };
        assert_eq!(snapshot.as_bytes(), b"echo\n");
        assert_eq!(snapshot.cursor(), 5);
        assert_eq!(snapshot.probe_nonce(), Some(SnapshotNonce(9)));
        assert_eq!(error(&frames[1]), &FrameError::MissingFishPrintTerminator);
    }

    #[test]
    fn rejects_missing_malformed_and_overflowing_probe_nonces() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:sync-probe:\0probe-buffer:b:x:0:x\0\
              capability:sync-probe:18446744073709551616\0",
        );
        assert_eq!(error(&frames[0]), &FrameError::MissingProbeNonce);
        assert_eq!(error(&frames[1]), &FrameError::NonDecimalProbeNonce);
        assert_eq!(error(&frames[2]), &FrameError::ProbeNonceOutOfRange);
    }

    #[test]
    fn rejects_invalid_utf8_and_cursor_syntax_without_losing_later_frames() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"buffer:b:0:\xff\0buffer:b::x\0buffer:b:x:x\0buffer:b:0:ok\0",
        );
        assert_eq!(error(&frames[0]), &FrameError::InvalidBufferUtf8);
        assert_eq!(error(&frames[1]), &FrameError::MissingCursor);
        assert_eq!(error(&frames[2]), &FrameError::NonDecimalCursor);
        assert!(matches!(event(&frames[3]), ShellEvent::Buffer(_)));
    }

    #[test]
    fn submitted_command_preserves_non_utf8_bytes() {
        let mut decoder = decoder();
        let frames = decode(&mut decoder, b"command-start:a\xffb\0");
        let ShellEvent::CommandStart(command) = event(&frames[0]) else {
            panic!("expected command start");
        };
        assert_eq!(command.as_bytes(), b"a\xffb");
        assert_eq!(command.as_str(), None);
    }

    #[test]
    fn validates_exit_status_and_rejects_empty_submission() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"command-stop:0\0command-stop:255\0command-stop:256\0\
              command-stop:-1\0command-stop:\0command-start:\0",
        );
        assert!(matches!(
            event(&frames[0]),
            ShellEvent::CommandStop(status) if status.success()
        ));
        assert!(matches!(
            event(&frames[1]),
            ShellEvent::CommandStop(status) if status.get() == 255
        ));
        assert_eq!(error(&frames[2]), &FrameError::ExitStatusOutOfRange);
        assert_eq!(error(&frames[3]), &FrameError::NonDecimalExitStatus);
        assert_eq!(error(&frames[4]), &FrameError::MissingExitStatus);
        assert_eq!(error(&frames[5]), &FrameError::EmptySubmittedCommand);
    }

    #[test]
    fn oversized_frame_is_bounded_and_following_frame_recovers() {
        let mut decoder = ShellEventDecoder::with_frame_limit(StreamEpoch::INITIAL, 12);
        decoder.push(b"0123456789abcdef", |_| panic!("unterminated"));
        assert_eq!(decoder.pending_len(), 0);
        assert!(decoder.is_discarding());
        let frames = decode(&mut decoder, b"\0prompt-ready\0");
        assert_eq!(
            error(&frames[0]),
            &FrameError::FrameTooLarge {
                observed_bytes: 16,
                limit: 12,
            }
        );
        assert_eq!(event(&frames[1]), &ShellEvent::PromptReady);
    }

    #[test]
    fn streaming_callback_retains_no_unbounded_result_batch() {
        let mut decoder = ShellEventDecoder::with_frame_limit(StreamEpoch::INITIAL, 8);
        let input = vec![0_u8; 100_000];
        let mut emitted = 0_usize;
        decoder.push(&input, |_| emitted = emitted.saturating_add(1));
        assert_eq!(emitted, input.len());
        assert_eq!(decoder.pending_len(), 0);
    }

    #[test]
    fn rejected_frame_desynchronizes_and_prompt_recovers() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:native-buffer\0prompt-ready\0wat\0prompt-ready\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        assert!(matches!(
            state.apply(frames[0].clone()),
            StateUpdate::CapabilityChanged(BufferSyncCapability::Native)
        ));
        assert!(matches!(
            state.apply(frames[1].clone()),
            StateUpdate::PromptReady { recovered: true }
        ));
        assert!(state.suggestions_allowed());
        assert_eq!(
            state.apply(frames[2].clone()),
            StateUpdate::FrameRejected(FrameError::UnknownEvent)
        );
        assert_eq!(
            state.synchronization(),
            SynchronizationState::Desynchronized
        );
        assert!(!state.suggestions_allowed());
        assert_eq!(state.last_position(), Some(frames[2].position()));
        assert!(matches!(
            state.apply(frames[3].clone()),
            StateUpdate::PromptReady { recovered: true }
        ));
        assert!(state.suggestions_allowed());
    }

    #[test]
    fn rejection_during_command_invalidates_attribution_until_snapshot() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:native-buffer\0prompt-ready\0buffer:b:7:echo hi\0\
              command-start:echo hi\0wat\0command-stop:0\0prompt-ready\0buffer:b:1:x\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        let updates = frames
            .into_iter()
            .map(|frame| state.apply(frame))
            .collect::<Vec<_>>();

        assert_eq!(
            updates[4],
            StateUpdate::FrameRejected(FrameError::UnknownEvent)
        );
        assert_eq!(updates[5], StateUpdate::LifecycleSuppressed);
        assert!(
            !updates
                .iter()
                .any(|update| matches!(update, StateUpdate::CommandStopped(_)))
        );
        assert_eq!(
            updates[7],
            StateUpdate::BufferSynchronized { recovered: false }
        );
        assert_eq!(state.buffer().unwrap().as_bytes(), b"x");
        assert!(state.suggestions_allowed());
    }

    #[test]
    fn exact_preexec_supersedes_a_mismatching_snapshot() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:native-buffer\0prompt-ready\0buffer:b:3:git status\0\
              command-start:git diff\0command-stop:1\0prompt-ready\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        let mut frames = frames.into_iter();
        for frame in frames.by_ref().take(3) {
            state.apply(frame);
        }
        assert_eq!(
            state.apply(frames.next().expect("start")),
            StateUpdate::CommandStarted {
                source: Some(AttributionSource::LifecycleFrame),
                preexec_matches_snapshot: Some(false),
            }
        );
        let StateUpdate::CommandStopped(completed) = state.apply(frames.next().expect("stop"))
        else {
            panic!("expected completed command");
        };
        assert_eq!(completed.command().as_bytes(), b"git diff");
        assert_eq!(completed.source(), AttributionSource::LifecycleFrame);
        assert_eq!(completed.status().get(), 1);
    }

    #[test]
    fn unknown_start_never_attributes_even_with_a_fresh_snapshot() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:native-buffer\0prompt-ready\0buffer:b:10:git status\0\
              command-start-unknown\0command-stop:1\0prompt-ready\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        let mut frames = frames.into_iter();
        for frame in frames.by_ref().take(3) {
            state.apply(frame);
        }
        assert!(matches!(
            state.apply(frames.next().expect("start")),
            StateUpdate::CommandStarted {
                source: None,
                preexec_matches_snapshot: None,
            }
        ));
        assert_eq!(
            state.apply(frames.next().expect("stop")),
            StateUpdate::CommandStoppedWithoutAttribution(ShellExitStatus(1))
        );
        assert!(!state.suggestions_allowed());
        assert!(matches!(
            state.apply(frames.next().expect("prompt")),
            StateUpdate::PromptReady { recovered: false }
        ));
    }

    #[test]
    fn unknown_start_without_probe_never_emits_false_completion() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:unavailable\0prompt-ready\0command-start-unknown\0\
              command-stop:0\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        let updates = frames
            .into_iter()
            .map(|frame| state.apply(frame))
            .collect::<Vec<_>>();
        assert!(matches!(
            updates[3],
            StateUpdate::CommandStoppedWithoutAttribution(status) if status.success()
        ));
        assert!(
            !updates
                .iter()
                .any(|update| matches!(update, StateUpdate::CommandStopped(_)))
        );
        assert!(!state.suggestions_allowed());
    }

    #[test]
    fn exact_preexec_is_safe_fallback_without_an_edit_snapshot() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:native-buffer\0prompt-ready\0command-start:echo hi\0\
              command-stop:0\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        let updates = frames
            .into_iter()
            .map(|frame| state.apply(frame))
            .collect::<Vec<_>>();
        let StateUpdate::CommandStopped(completed) = &updates[3] else {
            panic!("expected completed command");
        };
        assert_eq!(completed.command().as_bytes(), b"echo hi");
        assert_eq!(completed.source(), AttributionSource::LifecycleFrame);
    }

    #[test]
    fn duplicate_and_impossible_lifecycle_never_complete() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:native-buffer\0prompt-ready\0command-start:echo hi\0command-start:echo bye\0\
              command-stop:0\0prompt-ready\0command-stop:0\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        let updates = frames
            .into_iter()
            .map(|frame| state.apply(frame))
            .collect::<Vec<_>>();
        assert_eq!(
            updates[3],
            StateUpdate::LifecycleRejected(LifecycleError::DuplicateCommandStart)
        );
        assert_eq!(updates[4], StateUpdate::LifecycleSuppressed);
        assert_eq!(
            updates[6],
            StateUpdate::LifecycleRejected(LifecycleError::CommandStopWithoutStart)
        );
        assert!(
            !updates
                .iter()
                .any(|update| matches!(update, StateUpdate::CommandStopped(_)))
        );
    }

    #[test]
    fn stale_frame_desynchronizes_instead_of_resurrecting_buffer() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:native-buffer\0prompt-ready\0buffer:b:3:old\0\
              buffer:b:3:new\0prompt-ready\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        state.apply(frames[0].clone());
        state.apply(frames[1].clone());
        assert!(matches!(
            state.apply(frames[3].clone()),
            StateUpdate::StreamOrderRejected { .. }
        ));
        assert!(state.buffer().is_none());
        assert_eq!(
            state.synchronization(),
            SynchronizationState::Desynchronized
        );
        assert!(matches!(
            state.apply(frames[2].clone()),
            StateUpdate::StreamOrderRejected { .. }
        ));
        assert!(matches!(
            state.apply(frames[4].clone()),
            StateUpdate::StreamOrderRejected { .. }
        ));
    }

    #[test]
    fn explicit_epoch_reset_prevents_decoder_recreation_staleness() {
        let mut decoder = decoder();
        let initial = decode(&mut decoder, b"capability:native-buffer\0prompt-ready\0");
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        state.apply(initial[0].clone());
        state.apply(initial[1].clone());

        let epoch = decoder.reset_stream().expect("epoch advances");
        state.reset_stream(epoch).expect("state epoch advances");
        assert_eq!(state.capability(), BufferSyncCapability::Unknown);
        assert!(!state.suggestions_allowed());
        let next = decode(
            &mut decoder,
            b"prompt-ready\0capability:native-buffer\0prompt-ready\0buffer:b:1:x\0",
        );
        assert_eq!(next[0].position().epoch(), epoch);
        assert_eq!(
            state.apply(next[0].clone()),
            StateUpdate::LifecycleSuppressed
        );
        state.apply(next[1].clone());
        state.apply(next[2].clone());
        assert!(matches!(
            state.apply(next[3].clone()),
            StateUpdate::BufferSynchronized { recovered: false }
        ));
        assert_eq!(state.buffer().unwrap().as_bytes(), b"x");
    }

    #[test]
    fn higher_epoch_frames_require_an_explicit_matching_reset() {
        let mut decoder = decoder();
        let epoch = decoder.reset_stream().expect("epoch advances");
        let frame = decode(&mut decoder, b"capability:unavailable\0").remove(0);
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);

        assert!(matches!(
            state.apply(frame.clone()),
            StateUpdate::StreamOrderRejected { .. }
        ));
        assert_eq!(state.epoch(), StreamEpoch::INITIAL);
        state.reset_stream(epoch).expect("explicit reset");
        assert_eq!(
            state.apply(frame),
            StateUpdate::CapabilityChanged(BufferSyncCapability::Unavailable)
        );
    }

    #[test]
    fn stale_probe_response_after_newer_local_input_fails_closed() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:sync-probe:0\0prompt-ready\0\
              probe-buffer:b:1:1:x\0probe-buffer:b:2:1:y\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        state.apply(frames[0].clone());
        state.apply(frames[1].clone());

        let requested = state.observe_local_input().expect("generation");
        assert_eq!(state.begin_sync_probe(), Ok(SnapshotNonce(1)));
        let current = state.observe_local_input().expect("newer generation");
        assert_eq!(
            state.apply(frames[2].clone()),
            StateUpdate::SnapshotRejected(SnapshotRejection::StaleProbeGeneration {
                requested,
                current,
            })
        );
        assert!(state.buffer().is_none());
        assert!(!state.suggestions_allowed());

        assert_eq!(state.begin_sync_probe(), Ok(SnapshotNonce(2)));
        assert_eq!(
            state.apply(frames[3].clone()),
            StateUpdate::BufferSynchronized { recovered: true }
        );
        assert_eq!(state.buffer().unwrap().as_bytes(), b"y");
        assert_eq!(state.buffer_generation(), Some(current));
    }

    #[test]
    fn delayed_prompt_cannot_overwrite_newer_local_input_authority() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:sync-probe:0\0prompt-ready\0prompt-ready\0\
              probe-buffer:b:1:1:x\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        state.apply(frames[0].clone());
        state.apply(frames[1].clone());

        let generation = state.observe_local_input().expect("generation");
        assert_eq!(state.begin_sync_probe(), Ok(SnapshotNonce(1)));
        assert_eq!(
            state.apply(frames[2].clone()),
            StateUpdate::LifecycleSuppressed
        );
        assert_eq!(state.input_generation(), generation);
        assert!(state.buffer().is_none());
        assert_eq!(state.buffer_generation(), None);
        assert_eq!(state.pending_probe_nonce(), Some(SnapshotNonce(1)));
        assert!(!state.suggestions_allowed());

        assert_eq!(
            state.apply(frames[3].clone()),
            StateUpdate::BufferSynchronized { recovered: true }
        );
        assert_eq!(state.buffer().unwrap().as_bytes(), b"x");
        assert_eq!(state.buffer_generation(), Some(generation));
    }

    #[test]
    fn unmatched_probe_nonce_cannot_resurrect_a_snapshot() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:sync-probe:10\0prompt-ready\0probe-buffer:b:12:1:x\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        state.apply(frames[0].clone());
        state.apply(frames[1].clone());
        state.observe_local_input().expect("generation");
        assert_eq!(state.begin_sync_probe(), Ok(SnapshotNonce(11)));
        assert_eq!(
            state.apply(frames[2].clone()),
            StateUpdate::SnapshotRejected(SnapshotRejection::ProbeNonceMismatch {
                expected: Some(SnapshotNonce(11)),
                received: SnapshotNonce(12),
            })
        );
        assert_eq!(state.pending_probe_nonce(), Some(SnapshotNonce(11)));
        assert!(state.buffer().is_none());
    }

    #[test]
    fn delayed_native_snapshot_after_two_forwarded_inputs_fails_closed() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:native-buffer\0prompt-ready\0buffer:b:1:x\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        state.apply(frames[0].clone());
        state.apply(frames[1].clone());
        state.observe_local_input().expect("first generation");
        let current = state.observe_local_input().expect("second generation");

        assert_eq!(
            state.apply(frames[2].clone()),
            StateUpdate::SnapshotRejected(SnapshotRejection::UncorrelatedNativeSnapshot {
                current,
            })
        );
        assert!(state.buffer().is_none());
        assert!(!state.suggestions_allowed());
    }

    #[test]
    fn prompt_and_buffer_frames_while_running_are_rejected() {
        for tail in [b"buffer:b:1:x\0".as_slice(), b"prompt-ready\0".as_slice()] {
            let mut decoder = decoder();
            let mut bytes = b"capability:native-buffer\0prompt-ready\0\
                command-start:echo hi\0"
                .to_vec();
            bytes.extend_from_slice(tail);
            let frames = decode(&mut decoder, &bytes);
            let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
            for frame in &frames[..3] {
                state.apply(frame.clone());
            }
            let update = state.apply(frames[3].clone());
            assert!(matches!(
                update,
                StateUpdate::LifecycleRejected(
                    LifecycleError::BufferWhileCommandRunning
                        | LifecycleError::PromptWhileCommandRunning
                )
            ));
            assert!(state.buffer().is_none());
            assert!(!state.suggestions_allowed());
        }
    }

    #[test]
    fn exact_preexec_remains_authoritative_after_local_input_invalidates_snapshot() {
        let mut decoder = decoder();
        let frames = decode(
            &mut decoder,
            b"capability:native-buffer\0prompt-ready\0command-start:echo hi\0\
              command-stop:0\0",
        );
        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        state.apply(frames[0].clone());
        state.apply(frames[1].clone());
        state.observe_local_input().expect("generation");
        assert_eq!(
            state.apply(frames[2].clone()),
            StateUpdate::CommandStarted {
                source: Some(AttributionSource::LifecycleFrame),
                preexec_matches_snapshot: None,
            }
        );
        let StateUpdate::CommandStopped(completed) = state.apply(frames[3].clone()) else {
            panic!("expected completed command");
        };
        assert_eq!(completed.command().as_bytes(), b"echo hi");
    }

    #[test]
    fn debug_output_redacts_protocol_payloads() {
        let secret = "Dean Pelton's secret command";
        let mut decoder = decoder();
        decoder.push(format!("buffer:b:0:{secret}").as_bytes(), |_| {});
        assert!(!format!("{decoder:?}").contains(secret));

        let frame = decode(
            &mut ShellEventDecoder::new(StreamEpoch::INITIAL),
            format!("command-start:{secret}\0").as_bytes(),
        )
        .remove(0);
        assert!(!format!("{frame:?}").contains(secret));

        let mut state = ShellSessionState::new(StreamEpoch::INITIAL);
        let mut state_decoder = ShellEventDecoder::new(StreamEpoch::INITIAL);
        let frames = decode(
            &mut state_decoder,
            format!("capability:native-buffer\0prompt-ready\0buffer:b:0:{secret}\0").as_bytes(),
        );
        for frame in frames {
            state.apply(frame);
        }
        assert!(!format!("{state:?}").contains(secret));
    }

    #[test]
    fn finish_rejects_truncated_input_and_advances_authority() {
        let mut decoder = decoder();
        decoder.push(b"prompt", |_| panic!("unterminated"));
        let frame = decoder.finish().expect("rejection");
        assert_eq!(
            error(&frame),
            &FrameError::TruncatedFrame { observed_bytes: 6 }
        );
        assert_eq!(frame.position().sequence(), 0);
        assert!(decoder.finish().is_none());
    }
}
