//! Parent-terminal validation, raw mode, and serialized output restoration.
//!
//! This module deliberately owns terminal modes but not session policy.  Its
//! guard restores every mode it changed on explicit shutdown and during
//! unwinding.  `Drop` is necessarily best-effort; callers that can report an
//! error should call [`TerminalGuard::restore`] first.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::ffi::OsStrExt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::unistd::isatty;
use rustix::termios::{Winsize, tcgetwinsize, tcsetwinsize};

use crate::pty::PtySize;

/// Largest atomic write accepted by the shared parent-output boundary.
///
/// Event loops should split larger shell reads into chunks no larger than this
/// value.  The limit prevents a renderer or producer from monopolizing the
/// terminal-output lock with an accidentally unbounded frame.
pub const MAX_SERIALIZED_WRITE: usize = 64 * 1024;

/// Largest inert ANSI cleanup program retained by a terminal guard.
pub const MAX_OVERLAY_CLEANUP: usize = MAX_SERIALIZED_WRITE - 32;

/// Maximum wait for the write boundary while restoring terminal state.
const RESTORE_OUTPUT_TIMEOUT: Duration = Duration::from_secs(2);
const OUTPUT_LOCK_POLL: Duration = Duration::from_millis(1);

const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const ENABLE_WRAP: &[u8] = b"\x1b[?7h";
const DISABLE_WRAP: &[u8] = b"\x1b[?7l";
const RESET_STYLE: &[u8] = b"\x1b[0m";

/// Where a parent-terminal dimension snapshot came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DimensionSource {
    /// A kernel query or platform adapter obtained the terminal dimensions.
    Kernel,
    /// Both `COLUMNS` and `LINES` were present and valid in the environment.
    Environment,
    /// The caller deliberately selected a compatibility fallback.
    Fallback,
}

/// A validated terminal size and its acquisition capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalDimensions {
    size: PtySize,
    source: DimensionSource,
}

impl TerminalDimensions {
    /// Captures the kernel dimensions of a terminal descriptor.
    ///
    /// Cell and pixel dimensions are preserved exactly.  A terminal reporting
    /// zero rows or columns is rejected rather than silently replaced with an
    /// environment or compatibility value.
    ///
    /// # Errors
    ///
    /// Returns [`DimensionError`] when the kernel query fails or reports zero
    /// rows or columns.
    pub fn from_terminal(terminal: &impl AsFd) -> Result<Self, DimensionError> {
        let size = tcgetwinsize(terminal)
            .map_err(|error| DimensionError::KernelQuery(io::Error::from(error).kind()))?;
        Self::from_kernel(PtySize {
            rows: size.ws_row,
            cols: size.ws_col,
            pixel_width: size.ws_xpixel,
            pixel_height: size.ws_ypixel,
        })
    }

    /// Validates a size supplied by a kernel terminal query or platform adapter.
    ///
    /// # Errors
    ///
    /// Returns [`DimensionError`] when rows or columns are zero.
    pub fn from_kernel(size: PtySize) -> Result<Self, DimensionError> {
        Self::new(size, DimensionSource::Kernel)
    }

    /// Reads `COLUMNS` and `LINES` through a bounded caller-supplied lookup.
    ///
    /// This is an explicitly weaker capability than [`Self::from_terminal`].
    ///
    /// # Errors
    ///
    /// Returns [`DimensionError`] unless both values are present decimal
    /// integers in the range `1..=u16::MAX`.
    pub fn from_environment(
        mut lookup: impl FnMut(&str) -> Option<OsString>,
    ) -> Result<Self, DimensionError> {
        let columns = parse_dimension(lookup("COLUMNS"), "COLUMNS")?;
        let rows = parse_dimension(lookup("LINES"), "LINES")?;
        Self::new(
            PtySize {
                rows,
                cols: columns,
                pixel_width: 0,
                pixel_height: 0,
            },
            DimensionSource::Environment,
        )
    }

    /// Constructs the conventional 80-by-24 compatibility fallback.
    ///
    /// The source remains visible so the runtime never mistakes this for an
    /// authoritative kernel snapshot.
    #[must_use]
    pub const fn fallback() -> Self {
        Self {
            size: PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
            source: DimensionSource::Fallback,
        }
    }

    /// Returns the validated PTY size.
    #[must_use]
    pub const fn size(self) -> PtySize {
        self.size
    }

    /// Returns how the dimensions were acquired.
    #[must_use]
    pub const fn source(self) -> DimensionSource {
        self.source
    }

    /// Applies these dimensions to a terminal descriptor through the kernel.
    ///
    /// Cell and pixel dimensions are both propagated.
    ///
    /// # Errors
    ///
    /// Returns [`DimensionError::KernelResize`] when the kernel rejects the
    /// resize.
    pub fn apply_to(self, terminal: &impl AsFd) -> Result<(), DimensionError> {
        tcsetwinsize(
            terminal,
            Winsize {
                ws_row: self.size.rows,
                ws_col: self.size.cols,
                ws_xpixel: self.size.pixel_width,
                ws_ypixel: self.size.pixel_height,
            },
        )
        .map_err(|error| DimensionError::KernelResize(io::Error::from(error).kind()))
    }

    fn new(size: PtySize, source: DimensionSource) -> Result<Self, DimensionError> {
        if size.rows == 0 {
            return Err(DimensionError::ZeroRows);
        }
        if size.cols == 0 {
            return Err(DimensionError::ZeroColumns);
        }
        Ok(Self { size, source })
    }
}

/// Invalid or unavailable dimension input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DimensionError {
    /// The kernel window-size query failed.
    KernelQuery(io::ErrorKind),
    /// The kernel rejected a window-size update.
    KernelResize(io::ErrorKind),
    /// A required environment value was absent.
    Missing(&'static str),
    /// A required environment value was malformed or outside the valid range.
    Invalid(&'static str),
    /// Terminal rows were zero.
    ZeroRows,
    /// Terminal columns were zero.
    ZeroColumns,
}

impl fmt::Display for DimensionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KernelQuery(kind) => {
                write!(formatter, "could not query terminal dimensions: {kind}")
            }
            Self::KernelResize(kind) => {
                write!(formatter, "could not resize terminal: {kind}")
            }
            Self::Missing(variable) => write!(formatter, "{variable} is not set"),
            Self::Invalid(variable) => {
                write!(
                    formatter,
                    "{variable} must be an integer from 1 through 65535"
                )
            }
            Self::ZeroRows => formatter.write_str("terminal rows must be greater than zero"),
            Self::ZeroColumns => formatter.write_str("terminal columns must be greater than zero"),
        }
    }
}

impl Error for DimensionError {}

fn parse_dimension(value: Option<OsString>, variable: &'static str) -> Result<u16, DimensionError> {
    let Some(value) = value else {
        return Err(DimensionError::Missing(variable));
    };
    if value.as_os_str().as_bytes().len() > 5 {
        return Err(DimensionError::Invalid(variable));
    }
    let Some(value) = value.to_str() else {
        return Err(DimensionError::Invalid(variable));
    };
    value
        .parse::<u16>()
        .ok()
        .filter(|dimension| *dimension != 0)
        .ok_or(DimensionError::Invalid(variable))
}

/// A validated one-based terminal cursor position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalCursor {
    row: u16,
    column: u16,
}

impl TerminalCursor {
    /// Constructs a cursor within `dimensions`.
    ///
    /// # Errors
    ///
    /// Returns [`CleanupError::CursorOutOfBounds`] for zero or out-of-range
    /// coordinates.
    pub fn new(
        row: u16,
        column: u16,
        dimensions: TerminalDimensions,
    ) -> Result<Self, CleanupError> {
        let size = dimensions.size();
        if row == 0 || row > size.rows || column == 0 || column > size.cols {
            return Err(CleanupError::CursorOutOfBounds);
        }
        Ok(Self { row, column })
    }

    /// One-based row.
    #[must_use]
    pub const fn row(self) -> u16 {
        self.row
    }

    /// One-based column.
    #[must_use]
    pub const fn column(self) -> u16 {
        self.column
    }
}

/// A bounded sequence of exact, dimension-checked erasures.
///
/// The accepted grammar is one or more `CUP(row,column), ECH(count)` pairs,
/// followed by one final `CUP` matching the caller-supplied cursor exactly.
/// Every cursor and erase extent must fit the captured terminal dimensions,
/// and no terminal cell may be owned by more than one erasure pair.
/// Absolute positioning and exact erasure make a full retry idempotent after
/// any partial output failure. Relative movement, repeated cursor positions,
/// cursor-only programs, broad erasure, text, and other controls are rejected.
#[derive(Clone, Eq, PartialEq)]
pub struct OverlayCleanup {
    bytes: Vec<u8>,
    rows: u16,
    cols: u16,
    final_cursor: TerminalCursor,
}

impl OverlayCleanup {
    /// Validates a complete ANSI cleanup program for captured dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`CleanupError`] when the input is too large, contains anything
    /// outside the approved grammar, crosses a terminal boundary, or does not
    /// end at `final_cursor`.
    pub fn from_ansi(
        bytes: &[u8],
        dimensions: TerminalDimensions,
        final_cursor: TerminalCursor,
    ) -> Result<Self, CleanupError> {
        if bytes.len() > MAX_OVERLAY_CLEANUP {
            return Err(CleanupError::TooLarge {
                actual: bytes.len(),
                limit: MAX_OVERLAY_CLEANUP,
            });
        }
        let size = dimensions.size();
        if final_cursor.row > size.rows || final_cursor.column > size.cols {
            return Err(CleanupError::CursorOutOfBounds);
        }
        if !is_inert_cleanup(bytes, size, final_cursor) {
            return Err(CleanupError::UnsafeSequence);
        }
        Ok(Self {
            bytes: bytes.to_vec(),
            rows: size.rows,
            cols: size.cols,
            final_cursor,
        })
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for OverlayCleanup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OverlayCleanup")
            .field("bytes", &"<validated ANSI>")
            .field("length", &self.bytes.len())
            .field("rows", &self.rows)
            .field("cols", &self.cols)
            .field("final_cursor", &self.final_cursor)
            .finish()
    }
}

fn is_inert_cleanup(bytes: &[u8], size: PtySize, final_cursor: TerminalCursor) -> bool {
    let mut index = 0;
    let mut erasure_pairs = 0_usize;
    let mut owned_ranges = Vec::new();
    loop {
        let Some((cursor, after_cursor)) = parse_cursor_position(bytes, index) else {
            return false;
        };
        if cursor.row > size.rows || cursor.column > size.cols {
            return false;
        }
        if after_cursor == bytes.len() {
            return erasure_pairs != 0 && cursor == final_cursor;
        }

        let Some((count, after_erase)) = parse_exact_erase(bytes, after_cursor) else {
            return false;
        };
        let available = size.cols - cursor.column + 1;
        if count > available {
            return false;
        }
        let Some(end) = count
            .checked_sub(1)
            .and_then(|offset| cursor.column.checked_add(offset))
        else {
            return false;
        };
        let range = OwnedEraseRange {
            row: cursor.row,
            start: cursor.column,
            end,
        };
        if owned_ranges.iter().any(|owned: &OwnedEraseRange| {
            owned.row == range.row && range.start <= owned.end && owned.start <= range.end
        }) {
            return false;
        }
        owned_ranges.push(range);
        erasure_pairs += 1;
        index = after_erase;
    }
}

#[derive(Clone, Copy)]
struct OwnedEraseRange {
    row: u16,
    start: u16,
    end: u16,
}

fn parse_cursor_position(bytes: &[u8], mut index: usize) -> Option<(TerminalCursor, usize)> {
    if bytes.get(index..index.checked_add(2)?)? != b"\x1b[" {
        return None;
    }
    index += 2;
    let row = parse_decimal(bytes, &mut index, b';')?;
    let column = parse_decimal(bytes, &mut index, b'H')?;
    Some((TerminalCursor { row, column }, index))
}

fn parse_exact_erase(bytes: &[u8], mut index: usize) -> Option<(u16, usize)> {
    if bytes.get(index..index.checked_add(2)?)? != b"\x1b[" {
        return None;
    }
    index += 2;
    let count = parse_decimal(bytes, &mut index, b'X')?;
    Some((count, index))
}

fn parse_decimal(bytes: &[u8], index: &mut usize, terminator: u8) -> Option<u16> {
    let start = *index;
    let mut value = 0_u16;
    while let Some(byte) = bytes.get(*index).copied().filter(u8::is_ascii_digit) {
        value = value.checked_mul(10)?.checked_add(u16::from(byte - b'0'))?;
        *index += 1;
    }
    if *index == start || value == 0 || bytes.get(*index) != Some(&terminator) {
        return None;
    }
    *index += 1;
    Some(value)
}

/// Rejected overlay cleanup state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupError {
    /// The cleanup exceeded its in-memory bound.
    TooLarge {
        /// Submitted byte count.
        actual: usize,
        /// Maximum accepted byte count.
        limit: usize,
    },
    /// A cursor coordinate was zero or outside the terminal dimensions.
    CursorOutOfBounds,
    /// Cleanup was built for different cell dimensions than the terminal.
    DimensionMismatch,
    /// The cleanup contained text or a terminal control outside the safe set.
    UnsafeSequence,
}

impl fmt::Display for CleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual, limit } => {
                write!(
                    formatter,
                    "cleanup has {actual} bytes; the limit is {limit}"
                )
            }
            Self::CursorOutOfBounds => {
                formatter.write_str("cleanup cursor is outside the terminal dimensions")
            }
            Self::DimensionMismatch => {
                formatter.write_str("cleanup dimensions do not match the terminal")
            }
            Self::UnsafeSequence => formatter.write_str(
                "cleanup must contain bounded cursor-and-erasure pairs and an exact final cursor",
            ),
        }
    }
}

impl Error for CleanupError {}

/// One serialized, bounded writer shared by shell and overlay producers.
pub struct SerializedOutput<W: Write> {
    writer: Arc<Mutex<W>>,
}

impl<W: Write> SerializedOutput<W> {
    /// Creates an output serialization boundary.
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self {
            writer: Arc::new(Mutex::new(writer)),
        }
    }

    /// Writes one shell-output chunk without interleaving an overlay frame.
    ///
    /// # Errors
    ///
    /// Returns [`OutputError::TooLarge`] when the chunk exceeds
    /// [`MAX_SERIALIZED_WRITE`], or [`OutputError::Io`] when writing or
    /// flushing the parent terminal fails.
    pub fn write_shell(&self, bytes: &[u8]) -> Result<(), OutputError> {
        self.write_frame(bytes)
    }

    /// Writes one complete overlay transaction without shell-byte interleaving.
    ///
    /// # Errors
    ///
    /// Returns [`OutputError::TooLarge`] when the frame exceeds
    /// [`MAX_SERIALIZED_WRITE`], or [`OutputError::Io`] when writing or
    /// flushing the parent terminal fails.
    pub fn write_overlay(&self, bytes: &[u8]) -> Result<(), OutputError> {
        self.write_frame(bytes)
    }

    /// Writes one frame, giving up when the boundary stays contended.
    ///
    /// Only restoration uses this. Another thread can hold the write boundary
    /// indefinitely while blocked on a flow-controlled terminal, and process
    /// exit must not depend on that thread ever making progress, so a
    /// persistently contended boundary becomes a reported failure.
    ///
    /// # Errors
    ///
    /// Returns [`OutputError::TooLarge`] for an oversized frame, and an I/O
    /// error of kind [`io::ErrorKind::WouldBlock`] when the boundary stayed
    /// held for the whole timeout.
    pub fn write_overlay_within(&self, bytes: &[u8], timeout: Duration) -> Result<(), OutputError> {
        if bytes.len() > MAX_SERIALIZED_WRITE {
            return Err(OutputError::TooLarge {
                actual: bytes.len(),
                limit: MAX_SERIALIZED_WRITE,
            });
        }
        let deadline = Instant::now() + timeout;
        loop {
            match self.writer.try_lock() {
                Ok(mut writer) => {
                    return writer
                        .write_all(bytes)
                        .and_then(|()| writer.flush())
                        .map_err(OutputError::from);
                }
                Err(TryLockError::Poisoned(poisoned)) => {
                    let mut writer = poisoned.into_inner();
                    return writer
                        .write_all(bytes)
                        .and_then(|()| writer.flush())
                        .map_err(OutputError::from);
                }
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(OutputError::Io(io::ErrorKind::WouldBlock));
                    }
                    thread::sleep(OUTPUT_LOCK_POLL);
                }
            }
        }
    }

    fn write_frame(&self, bytes: &[u8]) -> Result<(), OutputError> {
        if bytes.len() > MAX_SERIALIZED_WRITE {
            return Err(OutputError::TooLarge {
                actual: bytes.len(),
                limit: MAX_SERIALIZED_WRITE,
            });
        }
        let mut writer = recover_lock(&self.writer);
        writer
            .write_all(bytes)
            .and_then(|()| writer.flush())
            .map_err(OutputError::from)
    }
}

impl<W: Write> Clone for SerializedOutput<W> {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
        }
    }
}

impl<W: Write> fmt::Debug for SerializedOutput<W> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerializedOutput")
            .field("writer", &"<redacted>")
            .field("maximum_frame", &MAX_SERIALIZED_WRITE)
            .finish()
    }
}

fn recover_lock<W>(mutex: &Mutex<W>) -> MutexGuard<'_, W> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A bounded output write failure without retained output bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputError {
    /// A caller attempted an oversized atomic write.
    TooLarge {
        /// Submitted byte count.
        actual: usize,
        /// Maximum accepted byte count.
        limit: usize,
    },
    /// The underlying writer failed.
    Io(io::ErrorKind),
}

impl From<io::Error> for OutputError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

impl fmt::Display for OutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual, limit } => {
                write!(
                    formatter,
                    "output frame has {actual} bytes; the limit is {limit}"
                )
            }
            Self::Io(kind) => write!(formatter, "terminal output failed: {kind}"),
        }
    }
}

impl Error for OutputError {}

/// Failure to replace a guard's validated terminal dimensions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DimensionUpdateError {
    /// Old-grid overlay cells have not been acknowledged as cleared.
    OverlayCleanupPending,
    /// Serializing the retained old-grid cleanup failed.
    Clear(OutputError),
}

impl fmt::Display for DimensionUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OverlayCleanupPending => {
                formatter.write_str("old terminal dimensions still own overlay cells")
            }
            Self::Clear(error) => {
                write!(formatter, "could not clear the old terminal grid: {error}")
            }
        }
    }
}

impl Error for DimensionUpdateError {}

/// Parent terminal initialization failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalError {
    /// Parent standard input is not a usable terminal.
    InputNotTty,
    /// Parent standard output is not a usable terminal.
    OutputNotTty,
    /// A terminal descriptor could not be inspected.
    Inspect(io::ErrorKind),
    /// The original terminal attributes could not be captured.
    CaptureAttributes,
    /// Raw mode could not be enabled.
    EnableRawMode,
    /// Raw mode failed and the immediate restoration attempt also failed.
    EnableRawModeAndRestoreFailed,
}

impl fmt::Display for TerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputNotTty => {
                formatter.write_str("argmax requires an interactive terminal on standard input")
            }
            Self::OutputNotTty => {
                formatter.write_str("argmax requires an interactive terminal on standard output")
            }
            Self::Inspect(kind) => {
                write!(formatter, "could not inspect the parent terminal: {kind}")
            }
            Self::CaptureAttributes => {
                formatter.write_str("could not capture the parent terminal settings")
            }
            Self::EnableRawMode => {
                formatter.write_str("could not put the parent terminal into raw mode")
            }
            Self::EnableRawModeAndRestoreFailed => formatter
                .write_str("could not enable raw mode or restore the parent terminal settings"),
        }
    }
}

impl Error for TerminalError {}

/// Which restoration operation failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestoreOperation {
    /// Clearing owned cells and restoring visible terminal modes.
    VisualState,
    /// Restoring the original termios attributes.
    Termios,
}

/// One best-effort restoration failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestoreFailure {
    /// Operation that failed.
    pub operation: RestoreOperation,
    /// Stable I/O category when one is available.
    pub io_kind: Option<io::ErrorKind>,
}

const EMPTY_RESTORE_FAILURE: RestoreFailure = RestoreFailure {
    operation: RestoreOperation::Termios,
    io_kind: None,
};

/// Fixed-capacity aggregate restoration failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreErrors {
    failures: [RestoreFailure; 2],
    length: u8,
}

impl RestoreErrors {
    const fn new() -> Self {
        Self {
            failures: [EMPTY_RESTORE_FAILURE; 2],
            length: 0,
        }
    }

    fn push(&mut self, failure: RestoreFailure) {
        let index = usize::from(self.length);
        if let Some(slot) = self.failures.get_mut(index) {
            *slot = failure;
            self.length += 1;
        }
    }

    const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// All attempted restoration operations that failed.
    #[must_use]
    pub fn failures(&self) -> &[RestoreFailure] {
        &self.failures[..usize::from(self.length)]
    }
}

impl fmt::Display for RestoreErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} terminal restoration operation(s) failed",
            self.length
        )
    }
}

impl Error for RestoreErrors {}

/// RAII ownership of parent raw mode and visual cleanup state.
///
/// `T` is retained for the life of the guard, preventing the descriptor used
/// for restoration from being closed or reused prematurely.
pub struct TerminalGuard<T: AsFd, W: Write> {
    terminal: T,
    output: SerializedOutput<W>,
    dimensions: TerminalDimensions,
    original: Termios,
    raw_active: bool,
    cursor_hidden: bool,
    wrap_disabled: bool,
    cleanup: Option<OverlayCleanup>,
    restore_buffer: Vec<u8>,
}

impl<T: AsFd, W: Write> TerminalGuard<T, W> {
    /// Validates both parent descriptors, captures termios, and enables raw mode.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalError`] if either descriptor is not a TTY or if
    /// capturing or applying terminal attributes fails.
    pub fn enter<O: AsFd>(
        terminal: T,
        output_descriptor: &O,
        output_writer: W,
        dimensions: TerminalDimensions,
    ) -> Result<Self, TerminalError> {
        validate_tty(&terminal, TerminalSide::Input)?;
        validate_tty(output_descriptor, TerminalSide::Output)?;
        let original = tcgetattr(&terminal).map_err(|_| TerminalError::CaptureAttributes)?;
        // Every heap allocation owned by the guard is complete before raw
        // mode is enabled. Restoration reuses this fixed-capacity buffer.
        let output = SerializedOutput::new(output_writer);
        let restore_buffer = Vec::with_capacity(MAX_SERIALIZED_WRITE);
        let mut raw = original.clone();
        cfmakeraw(&mut raw);
        if tcsetattr(&terminal, SetArg::TCSANOW, &raw).is_err() {
            return match tcsetattr(&terminal, SetArg::TCSANOW, &original) {
                Ok(()) => Err(TerminalError::EnableRawMode),
                Err(_) => Err(TerminalError::EnableRawModeAndRestoreFailed),
            };
        }

        Ok(Self {
            terminal,
            output,
            dimensions,
            original,
            raw_active: true,
            cursor_hidden: false,
            wrap_disabled: false,
            cleanup: None,
            restore_buffer,
        })
    }

    /// Returns a clone of the one output serialization boundary.
    #[must_use]
    pub fn output(&self) -> SerializedOutput<W> {
        self.output.clone()
    }

    /// Returns the parent terminal dimensions captured for this session.
    #[must_use]
    pub const fn dimensions(&self) -> TerminalDimensions {
        self.dimensions
    }

    /// Stores new validated dimensions after old overlay cells were cleared.
    ///
    /// Call [`Self::overlay_cleared`] only after a successful serialized clear,
    /// then call this method. The update is rejected while old-grid cleanup is
    /// retained, leaving the prior dimensions unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`DimensionUpdateError::OverlayCleanupPending`] while old-grid
    /// cells remain owned by this guard.
    pub fn update_dimensions(
        &mut self,
        dimensions: TerminalDimensions,
    ) -> Result<(), DimensionUpdateError> {
        if self.cleanup.is_some() {
            return Err(DimensionUpdateError::OverlayCleanupPending);
        }
        self.dimensions = dimensions;
        Ok(())
    }

    /// Atomically clears old-grid overlay cells and stores new dimensions.
    ///
    /// The retained cleanup uses absolute, retry-safe operations validated for
    /// the old grid. If output fails, both that cleanup and the old dimensions
    /// remain intact for retry. New dimensions are stored only after a complete
    /// serialized clear.
    ///
    /// # Errors
    ///
    /// Returns [`DimensionUpdateError::Clear`] when the old cleanup cannot be
    /// written and flushed completely.
    pub fn clear_overlay_and_update_dimensions(
        &mut self,
        dimensions: TerminalDimensions,
    ) -> Result<(), DimensionUpdateError> {
        if let Some(cleanup) = &self.cleanup {
            self.output
                .write_overlay(cleanup.as_bytes())
                .map_err(DimensionUpdateError::Clear)?;
            self.cleanup = None;
        }
        self.dimensions = dimensions;
        Ok(())
    }

    /// Records the only validated ANSI needed to erase currently owned cells.
    ///
    /// # Errors
    ///
    /// Returns [`CleanupError::DimensionMismatch`] if the cleanup was built
    /// for a different cell grid than this guard.
    pub fn set_overlay_cleanup(&mut self, cleanup: OverlayCleanup) -> Result<(), CleanupError> {
        let size = self.dimensions.size();
        if cleanup.rows != size.rows || cleanup.cols != size.cols {
            return Err(CleanupError::DimensionMismatch);
        }
        self.cleanup = Some(cleanup);
        Ok(())
    }

    /// Removes retained overlay cleanup after a renderer cleared its own cells.
    pub fn overlay_cleared(&mut self) {
        self.cleanup = None;
    }

    /// Hides the cursor and records ownership for eventual restoration.
    ///
    /// # Errors
    ///
    /// Returns [`OutputError`] if the control cannot be written atomically.
    pub fn hide_cursor(&mut self) -> Result<(), OutputError> {
        if !self.cursor_hidden {
            self.cursor_hidden = true;
            self.output.write_overlay(HIDE_CURSOR)?;
        }
        Ok(())
    }

    /// Shows a cursor previously hidden by this guard.
    ///
    /// # Errors
    ///
    /// Returns [`OutputError`] if the control cannot be written atomically.
    pub fn show_cursor(&mut self) -> Result<(), OutputError> {
        if self.cursor_hidden {
            self.output.write_overlay(SHOW_CURSOR)?;
            self.cursor_hidden = false;
        }
        Ok(())
    }

    /// Disables terminal line wrapping for a renderer's critical section.
    ///
    /// # Errors
    ///
    /// Returns [`OutputError`] if the control cannot be written atomically.
    pub fn disable_wrap(&mut self) -> Result<(), OutputError> {
        if !self.wrap_disabled {
            self.wrap_disabled = true;
            self.output.write_overlay(DISABLE_WRAP)?;
        }
        Ok(())
    }

    /// Re-enables terminal line wrapping after a renderer's critical section.
    ///
    /// # Errors
    ///
    /// Returns [`OutputError`] if the control cannot be written atomically.
    pub fn enable_wrap(&mut self) -> Result<(), OutputError> {
        if self.wrap_disabled {
            self.output.write_overlay(ENABLE_WRAP)?;
            self.wrap_disabled = false;
        }
        Ok(())
    }

    /// Restores every mode still owned by the guard and reports all failures.
    ///
    /// Calling this method repeatedly is safe.  Successfully restored state is
    /// not applied again; failed portions remain eligible for a later retry.
    ///
    /// # Errors
    ///
    /// Restores termios before attempting visual output, so a blocked or
    /// contended writer cannot retain raw mode. Returns all termios and visual
    /// restoration failures after attempting both independently.
    pub fn restore(&mut self) -> Result<(), RestoreErrors> {
        let mut failures = RestoreErrors::new();
        if self.raw_active {
            match tcsetattr(&self.terminal, SetArg::TCSANOW, &self.original) {
                Ok(()) => self.raw_active = false,
                Err(_) => failures.push(RestoreFailure {
                    operation: RestoreOperation::Termios,
                    io_kind: None,
                }),
            }
        }

        if self.cleanup.is_some() || self.cursor_hidden || self.wrap_disabled {
            self.restore_buffer.clear();
            if let Some(cleanup) = &self.cleanup {
                self.restore_buffer.extend_from_slice(cleanup.as_bytes());
            }
            self.restore_buffer.extend_from_slice(RESET_STYLE);
            if self.wrap_disabled {
                self.restore_buffer.extend_from_slice(ENABLE_WRAP);
            }
            if self.cursor_hidden {
                self.restore_buffer.extend_from_slice(SHOW_CURSOR);
            }

            match self
                .output
                .write_overlay_within(&self.restore_buffer, RESTORE_OUTPUT_TIMEOUT)
            {
                Ok(()) => {
                    self.cleanup = None;
                    self.cursor_hidden = false;
                    self.wrap_disabled = false;
                }
                Err(error) => failures.push(RestoreFailure {
                    operation: RestoreOperation::VisualState,
                    io_kind: match error {
                        OutputError::Io(kind) => Some(kind),
                        OutputError::TooLarge { .. } => None,
                    },
                }),
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }
}

impl TerminalGuard<io::Stdin, io::Stdout> {
    /// Enters raw mode on the process parent terminal.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalError`] if standard input or output is not a usable
    /// TTY, or if raw mode cannot be established.
    pub fn enter_stdio(dimensions: TerminalDimensions) -> Result<Self, TerminalError> {
        let input = io::stdin();
        let output_descriptor = io::stdout();
        Self::enter(input, &output_descriptor, io::stdout(), dimensions)
    }
}

impl<T: AsFd, W: Write> fmt::Debug for TerminalGuard<T, W> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalGuard")
            .field("dimensions", &self.dimensions)
            .field("raw_active", &self.raw_active)
            .field("cursor_hidden", &self.cursor_hidden)
            .field("wrap_disabled", &self.wrap_disabled)
            .field("has_overlay_cleanup", &self.cleanup.is_some())
            .finish_non_exhaustive()
    }
}

impl<T: AsFd, W: Write> Drop for TerminalGuard<T, W> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[derive(Clone, Copy)]
enum TerminalSide {
    Input,
    Output,
}

fn validate_tty(descriptor: &impl AsFd, side: TerminalSide) -> Result<(), TerminalError> {
    match isatty(descriptor.as_fd().as_raw_fd()) {
        Ok(true) => Ok(()),
        Ok(false) => Err(match side {
            TerminalSide::Input => TerminalError::InputNotTty,
            TerminalSide::Output => TerminalError::OutputNotTty,
        }),
        Err(errno) => Err(TerminalError::Inspect(io::Error::from(errno).kind())),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use nix::pty::openpty;
    use nix::sys::termios::LocalFlags;

    use super::*;

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl SharedWriter {
        fn bytes(&self) -> Vec<u8> {
            recover_lock(&self.0).clone()
        }
    }

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            recover_lock(&self.0).extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    struct PrefixFailWriter {
        state: Arc<Mutex<PrefixFailState>>,
    }

    struct PrefixFailState {
        bytes: Vec<u8>,
        remaining_before_failure: usize,
        failed_once: bool,
    }

    impl PrefixFailWriter {
        fn new(prefix: usize) -> (Self, Arc<Mutex<PrefixFailState>>) {
            let state = Arc::new(Mutex::new(PrefixFailState {
                bytes: Vec::new(),
                remaining_before_failure: prefix,
                failed_once: false,
            }));
            (
                Self {
                    state: Arc::clone(&state),
                },
                state,
            )
        }
    }

    impl Write for PrefixFailWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut state = recover_lock(&self.state);
            if state.failed_once {
                state.bytes.extend_from_slice(bytes);
                return Ok(bytes.len());
            }
            if state.remaining_before_failure == 0 {
                state.failed_once = true;
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }

            let accepted = bytes.len().min(state.remaining_before_failure);
            state.bytes.extend_from_slice(&bytes[..accepted]);
            state.remaining_before_failure -= accepted;
            Ok(accepted)
        }

        fn flush(&mut self) -> io::Result<()> {
            let mut state = recover_lock(&self.state);
            if state.failed_once {
                return Ok(());
            }
            state.failed_once = true;
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    struct ContendedWriter {
        block: Arc<AtomicBool>,
        started: SyncSender<()>,
        release: Receiver<()>,
    }

    impl Write for ContendedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.block.load(Ordering::Acquire) {
                let _ = self.started.send(());
                let _ = self.release.recv();
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_permanently_contended_boundary_yields_instead_of_waiting_forever() {
        let output = SerializedOutput::new(Vec::new());
        let held = output.clone();
        let (holding_tx, holding_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = thread::spawn(move || {
            let _guard = recover_lock(&held.writer);
            holding_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        holding_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let started = Instant::now();
        let outcome = output.write_overlay_within(b"restore", Duration::from_millis(50));
        let waited = started.elapsed();

        assert_eq!(outcome, Err(OutputError::Io(io::ErrorKind::WouldBlock)));
        assert!(waited < Duration::from_secs(1), "waited {waited:?}");

        release_tx.send(()).unwrap();
        holder.join().unwrap();
        assert!(
            output
                .write_overlay_within(b"restore", Duration::from_secs(1))
                .is_ok()
        );
    }

    fn dimensions() -> TerminalDimensions {
        TerminalDimensions::from_kernel(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap()
    }

    fn cursor(row: u16, column: u16) -> TerminalCursor {
        TerminalCursor::new(row, column, dimensions()).unwrap()
    }

    fn cleanup(
        bytes: &[u8],
        final_row: u16,
        final_column: u16,
    ) -> Result<OverlayCleanup, CleanupError> {
        OverlayCleanup::from_ansi(bytes, dimensions(), cursor(final_row, final_column))
    }

    fn termios_matches(mut actual: Termios, mut original: Termios) -> bool {
        // macOS may assert PENDIN after tcsetattr even though the requested
        // canonical settings were restored; it is kernel work state, not a
        // persistent user terminal mode.
        actual.local_flags.remove(LocalFlags::PENDIN);
        original.local_flags.remove(LocalFlags::PENDIN);
        actual.input_flags == original.input_flags
            && actual.output_flags == original.output_flags
            && actual.control_flags == original.control_flags
            && actual.local_flags == original.local_flags
            && actual.control_chars == original.control_chars
    }

    fn assert_restored(actual: Termios, original: Termios) {
        assert!(termios_matches(actual, original));
    }

    #[test]
    fn rejects_non_tty_input_without_paths() {
        let input = File::open("/dev/null").unwrap();
        let output = File::open("/dev/null").unwrap();
        let error = TerminalGuard::enter(input, &output, Vec::new(), dimensions()).unwrap_err();
        assert_eq!(error, TerminalError::InputNotTty);
        assert_eq!(
            error.to_string(),
            "argmax requires an interactive terminal on standard input"
        );
    }

    #[test]
    fn rejects_non_tty_output_without_paths() {
        let pair = openpty(None, None).unwrap();
        let output = File::open("/dev/null").unwrap();
        let error =
            TerminalGuard::enter(pair.slave, &output, Vec::new(), dimensions()).unwrap_err();
        assert_eq!(error, TerminalError::OutputNotTty);
        assert!(!error.to_string().contains("/dev"));
    }

    #[test]
    fn raw_mode_is_restored_explicitly_and_idempotently() {
        let pair = openpty(None, None).unwrap();
        let original = tcgetattr(&pair.slave).unwrap();
        let output_descriptor = pair.slave.try_clone().unwrap();
        let mut guard =
            TerminalGuard::enter(pair.slave, &output_descriptor, Vec::new(), dimensions()).unwrap();
        let raw = tcgetattr(&guard.terminal).unwrap();
        assert_ne!(raw, original);
        guard.restore().unwrap();
        guard.restore().unwrap();
        assert_restored(tcgetattr(&guard.terminal).unwrap(), original);
    }

    #[test]
    fn drop_restores_termios_during_unwind() {
        let pair = openpty(None, None).unwrap();
        let observer = pair.slave.try_clone().unwrap();
        let original = tcgetattr(&observer).unwrap();
        let output_descriptor = pair.slave.try_clone().unwrap();

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard =
                TerminalGuard::enter(pair.slave, &output_descriptor, Vec::new(), dimensions())
                    .unwrap();
            panic!("Troy triggered the crash boundary");
        }));

        assert!(result.is_err());
        assert_restored(tcgetattr(&observer).unwrap(), original);
    }

    #[test]
    fn restores_owned_visual_state_and_clears_it_once() {
        let pair = openpty(None, None).unwrap();
        let output_descriptor = pair.slave.try_clone().unwrap();
        let writer = SharedWriter::default();
        let observer = writer.clone();
        let mut guard =
            TerminalGuard::enter(pair.slave, &output_descriptor, writer, dimensions()).unwrap();
        guard.hide_cursor().unwrap();
        guard.disable_wrap().unwrap();
        guard
            .set_overlay_cleanup(cleanup(b"\x1b[24;1H\x1b[12X\x1b[1;1H", 1, 1).unwrap())
            .unwrap();

        guard.restore().unwrap();
        let after_first_restore = observer.bytes();
        guard.restore().unwrap();
        assert_eq!(observer.bytes(), after_first_restore);
        assert!(after_first_restore.ends_with(b"\x1b[0m\x1b[?7h\x1b[?25h"));
        assert!(
            after_first_restore
                .windows(5)
                .any(|bytes| bytes == b"\x1b[12X")
        );
    }

    #[test]
    fn explicit_restore_keeps_termios_restored_after_visual_failure() {
        let pair = openpty(None, None).unwrap();
        let observer = pair.slave.try_clone().unwrap();
        let original = tcgetattr(&observer).unwrap();
        let output_descriptor = pair.slave.try_clone().unwrap();
        let mut guard =
            TerminalGuard::enter(pair.slave, &output_descriptor, FailingWriter, dimensions())
                .unwrap();
        assert_eq!(
            guard.hide_cursor(),
            Err(OutputError::Io(io::ErrorKind::BrokenPipe))
        );

        let errors = guard.restore().unwrap_err();
        assert_eq!(errors.failures().len(), 1);
        assert_eq!(
            errors.failures()[0],
            RestoreFailure {
                operation: RestoreOperation::VisualState,
                io_kind: Some(io::ErrorKind::BrokenPipe),
            }
        );
        assert_restored(tcgetattr(&observer).unwrap(), original);
    }

    #[test]
    fn restore_reuses_the_buffer_allocated_before_raw_mode() {
        let pair = openpty(None, None).unwrap();
        let output_descriptor = pair.slave.try_clone().unwrap();
        let mut guard =
            TerminalGuard::enter(pair.slave, &output_descriptor, FailingWriter, dimensions())
                .unwrap();
        let buffer_pointer = guard.restore_buffer.as_ptr();
        assert_eq!(guard.restore_buffer.capacity(), MAX_SERIALIZED_WRITE);
        assert_eq!(
            guard.hide_cursor(),
            Err(OutputError::Io(io::ErrorKind::BrokenPipe))
        );

        assert!(guard.restore().is_err());
        assert_eq!(guard.restore_buffer.as_ptr(), buffer_pointer);
        assert_eq!(guard.restore_buffer.capacity(), MAX_SERIALIZED_WRITE);
    }

    #[test]
    fn guard_rejects_cleanup_for_a_different_cell_grid() {
        let other_dimensions = TerminalDimensions::from_kernel(PtySize {
            rows: 23,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
        let other_cursor = TerminalCursor::new(1, 1, other_dimensions).unwrap();
        let cleanup = OverlayCleanup::from_ansi(
            b"\x1b[23;1H\x1b[1X\x1b[1;1H",
            other_dimensions,
            other_cursor,
        )
        .unwrap();

        let pair = openpty(None, None).unwrap();
        let output_descriptor = pair.slave.try_clone().unwrap();
        let mut guard =
            TerminalGuard::enter(pair.slave, &output_descriptor, Vec::new(), dimensions()).unwrap();
        assert_eq!(
            guard.set_overlay_cleanup(cleanup),
            Err(CleanupError::DimensionMismatch)
        );
    }

    #[test]
    fn dimension_update_clears_old_grid_before_accepting_new_cleanup() {
        let old_cleanup = cleanup(b"\x1b[24;1H\x1b[4X\x1b[1;1H", 1, 1).unwrap();
        let old_bytes = old_cleanup.as_bytes().to_vec();
        let new_dimensions = TerminalDimensions::from_kernel(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 800,
            pixel_height: 600,
        })
        .unwrap();
        let new_cursor = TerminalCursor::new(1, 1, new_dimensions).unwrap();
        let new_cleanup =
            OverlayCleanup::from_ansi(b"\x1b[30;97H\x1b[4X\x1b[1;1H", new_dimensions, new_cursor)
                .unwrap();
        let stale_cleanup = cleanup(b"\x1b[24;1H\x1b[4X\x1b[1;1H", 1, 1).unwrap();

        let pair = openpty(None, None).unwrap();
        let output_descriptor = pair.slave.try_clone().unwrap();
        let writer = SharedWriter::default();
        let observer = writer.clone();
        let mut guard =
            TerminalGuard::enter(pair.slave, &output_descriptor, writer, dimensions()).unwrap();
        guard.set_overlay_cleanup(old_cleanup).unwrap();

        assert_eq!(
            guard.update_dimensions(new_dimensions),
            Err(DimensionUpdateError::OverlayCleanupPending)
        );
        assert_eq!(guard.dimensions(), dimensions());
        guard
            .clear_overlay_and_update_dimensions(new_dimensions)
            .unwrap();
        assert_eq!(observer.bytes(), old_bytes);
        assert_eq!(guard.dimensions(), new_dimensions);
        guard.set_overlay_cleanup(new_cleanup).unwrap();
        assert_eq!(
            guard.set_overlay_cleanup(stale_cleanup),
            Err(CleanupError::DimensionMismatch)
        );
    }

    #[test]
    fn failed_old_grid_clear_leaves_dimension_update_fully_retryable() {
        let old_cleanup = cleanup(b"\x1b[24;1H\x1b[4X\x1b[1;1H", 1, 1).unwrap();
        let new_dimensions = TerminalDimensions::from_kernel(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 800,
            pixel_height: 600,
        })
        .unwrap();
        let pair = openpty(None, None).unwrap();
        let output_descriptor = pair.slave.try_clone().unwrap();
        let mut guard =
            TerminalGuard::enter(pair.slave, &output_descriptor, FailingWriter, dimensions())
                .unwrap();
        guard.set_overlay_cleanup(old_cleanup).unwrap();

        for _ in 0..2 {
            assert_eq!(
                guard.clear_overlay_and_update_dimensions(new_dimensions),
                Err(DimensionUpdateError::Clear(OutputError::Io(
                    io::ErrorKind::BrokenPipe
                )))
            );
            assert_eq!(guard.dimensions(), dimensions());
            assert!(guard.cleanup.is_some());
        }
    }

    #[test]
    fn drop_restores_termios_before_waiting_for_the_output_lock() {
        let pair = openpty(None, None).unwrap();
        let observer = pair.slave.try_clone().unwrap();
        let original = tcgetattr(&observer).unwrap();
        let output_descriptor = pair.slave.try_clone().unwrap();
        let block = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel();
        let writer = ContendedWriter {
            block: Arc::clone(&block),
            started: started_tx,
            release: release_rx,
        };
        let mut guard =
            TerminalGuard::enter(pair.slave, &output_descriptor, writer, dimensions()).unwrap();
        guard.hide_cursor().unwrap();
        let output = guard.output();
        block.store(true, Ordering::Release);

        let writer_thread = thread::spawn(move || output.write_shell(b"Greendale"));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
        let drop_thread = thread::spawn(move || {
            drop(guard);
            let _ = dropped_tx.send(());
        });

        let deadline = Instant::now() + Duration::from_millis(500);
        while !termios_matches(tcgetattr(&observer).unwrap(), original.clone())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert_restored(tcgetattr(&observer).unwrap(), original);
        assert_eq!(dropped_rx.try_recv(), Err(TryRecvError::Empty));

        block.store(false, Ordering::Release);
        release_tx.send(()).unwrap();
        writer_thread.join().unwrap().unwrap();
        drop_thread.join().unwrap();
        dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn visual_restore_retries_every_accepted_prefix_idempotently() {
        const CLEANUP: &[u8] = b"\x1b[24;1H\x1b[12X\x1b[1;1H";
        let mut full_restore = CLEANUP.to_vec();
        full_restore.extend_from_slice(RESET_STYLE);

        for prefix in 0..=full_restore.len() {
            let pair = openpty(None, None).unwrap();
            let output_descriptor = pair.slave.try_clone().unwrap();
            let (writer, state) = PrefixFailWriter::new(prefix);
            let mut guard =
                TerminalGuard::enter(pair.slave, &output_descriptor, writer, dimensions()).unwrap();
            guard
                .set_overlay_cleanup(cleanup(CLEANUP, 1, 1).unwrap())
                .unwrap();

            assert!(guard.restore().is_err(), "prefix {prefix}");
            guard.restore().unwrap();
            let after_retry = recover_lock(&state).bytes.clone();
            guard.restore().unwrap();
            assert_eq!(recover_lock(&state).bytes, after_retry, "prefix {prefix}");

            let mut want = full_restore[..prefix].to_vec();
            want.extend_from_slice(&full_restore);
            assert_eq!(after_retry, want, "prefix {prefix}");
        }
    }

    #[test]
    fn cleanup_accepts_only_inert_bounded_csi() {
        assert!(cleanup(b"\x1b[3;4H\x1b[12X\x1b[24;1H", 24, 1).is_ok());
        for unsafe_sequence in [
            b"".as_slice(),
            b"\x1b[2K".as_slice(),
            b"\x1b[2J".as_slice(),
            b"\x1b[A".as_slice(),
            b"\x1b[3C".as_slice(),
            b"\x1b[s".as_slice(),
            b"\x1b[u".as_slice(),
            b"\x1b[2S".as_slice(),
            b"\x1b[1;20r".as_slice(),
            b"\x1b[?25h".as_slice(),
            b"\x1b[1;2;3H".as_slice(),
            b"\x1b[12X".as_slice(),
            b"\x1b[0X".as_slice(),
            b"\x1b[65536X".as_slice(),
            b"\x1b[1;1H".as_slice(),
            b"\x1b[1;1H\x1b[2;2H".as_slice(),
            b"\x1b[1;79H\x1b[3X\x1b[1;1H".as_slice(),
            b"\x1b[65536;1H\x1b[1X\x1b[1;1H".as_slice(),
            b"\x1b[25;1H\x1b[1X\x1b[1;1H".as_slice(),
            b"\x1b[3;4H\x1b[1X\x1b[3;4H\x1b[1X\x1b[1;1H".as_slice(),
            b"\x1b[3;4H\x1b[3X\x1b[3;6H\x1b[2X\x1b[1;1H".as_slice(),
        ] {
            assert_eq!(
                cleanup(unsafe_sequence, 1, 1),
                Err(CleanupError::UnsafeSequence)
            );
        }
        assert_eq!(
            cleanup(b"\x1b]0;hunter2\x07", 1, 1),
            Err(CleanupError::UnsafeSequence)
        );
        assert_eq!(
            cleanup(b"rm -rf Greendale", 1, 1),
            Err(CleanupError::UnsafeSequence)
        );
        assert_eq!(
            cleanup(&vec![b'x'; MAX_OVERLAY_CLEANUP + 1], 1, 1),
            Err(CleanupError::TooLarge {
                actual: MAX_OVERLAY_CLEANUP + 1,
                limit: MAX_OVERLAY_CLEANUP,
            })
        );
        assert_eq!(
            cleanup(b"\x1b[3;4H\x1b[12X\x1b[24;1H", 1, 1),
            Err(CleanupError::UnsafeSequence)
        );
        assert_eq!(
            TerminalCursor::new(0, 1, dimensions()),
            Err(CleanupError::CursorOutOfBounds)
        );
        assert!(cleanup(b"\x1b[3;4H\x1b[1X\x1b[3;5H\x1b[2X\x1b[1;1H", 1, 1).is_ok());
    }

    #[test]
    fn cleanup_accepts_the_largest_valid_erase_range() {
        let dimensions = TerminalDimensions::from_kernel(PtySize {
            rows: 1,
            cols: u16::MAX,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
        let final_cursor = TerminalCursor::new(1, 1, dimensions).unwrap();

        assert!(
            OverlayCleanup::from_ansi(b"\x1b[1;1H\x1b[65535X\x1b[1;1H", dimensions, final_cursor,)
                .is_ok()
        );
    }

    #[test]
    fn output_boundary_is_exact_and_bounded() {
        let writer = SharedWriter::default();
        let observer = writer.clone();
        let output = SerializedOutput::new(writer);
        let shell = output.clone();
        shell.write_shell(b"shell\0bytes").unwrap();
        output.write_overlay(b"\x1b[2K").unwrap();
        assert_eq!(observer.bytes(), b"shell\0bytes\x1b[2K");
        assert_eq!(
            output.write_shell(&vec![0; MAX_SERIALIZED_WRITE + 1]),
            Err(OutputError::TooLarge {
                actual: MAX_SERIALIZED_WRITE + 1,
                limit: MAX_SERIALIZED_WRITE,
            })
        );
    }

    #[test]
    fn environment_dimensions_are_typed_and_values_are_not_retained() {
        let dimensions = TerminalDimensions::from_environment(|name| match name {
            "COLUMNS" => Some(OsString::from("132")),
            "LINES" => Some(OsString::from("43")),
            _ => None,
        })
        .unwrap();
        assert_eq!(dimensions.size().cols, 132);
        assert_eq!(dimensions.size().rows, 43);
        assert_eq!(dimensions.source(), DimensionSource::Environment);

        assert_eq!(
            TerminalDimensions::from_environment(|name| {
                (name == "COLUMNS").then(|| OsString::from("hunter2"))
            }),
            Err(DimensionError::Invalid("COLUMNS"))
        );
    }

    #[test]
    fn kernel_dimensions_preserve_cells_and_pixels_across_a_real_pty() {
        let pair = openpty(None, None).unwrap();
        let initial = TerminalDimensions::from_kernel(PtySize {
            rows: 37,
            cols: 113,
            pixel_width: 904,
            pixel_height: 592,
        })
        .unwrap();
        initial.apply_to(&pair.slave).unwrap();
        assert_eq!(
            TerminalDimensions::from_terminal(&pair.master).unwrap(),
            initial
        );

        let resized = TerminalDimensions::from_kernel(PtySize {
            rows: 51,
            cols: 141,
            pixel_width: 1128,
            pixel_height: 816,
        })
        .unwrap();
        resized.apply_to(&pair.master).unwrap();
        assert_eq!(
            TerminalDimensions::from_terminal(&pair.slave).unwrap(),
            resized
        );
    }

    #[test]
    fn debug_output_contains_no_writer_or_cleanup_bytes() {
        let cleanup = cleanup(b"\x1b[1;1H\x1b[2X\x1b[1;1H", 1, 1).unwrap();
        assert!(!format!("{cleanup:?}").contains("1;1H"));
        let output = SerializedOutput::new(Vec::<u8>::new());
        assert!(!format!("{output:?}").contains("Vec"));
    }
}
