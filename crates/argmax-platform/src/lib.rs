//! Small audited wrappers for Unix interfaces missing from safe dependencies.

#![cfg(unix)]

/// Audited wrappers shared by supported Unix platforms.
pub mod unix {
    use std::io;
    use std::num::NonZeroI32;
    use std::ptr;
    use std::time::{Duration, Instant};

    const MAX_INHERITED_FRAME_BYTES: usize = 4 * 1024;

    /// Reports whether an exact child has exited without reaping it.
    ///
    /// Keeping an observed group leader waitable pins its process identifier
    /// while callers terminate the rest of that process group. This prevents a
    /// recycled identifier from redirecting cleanup at an unrelated process.
    ///
    /// # Errors
    ///
    /// Returns the operating-system error from `waitid`, including
    /// [`io::ErrorKind::NotFound`] when the identifier is not a waitable child.
    pub fn peek_child_exit(pid: NonZeroI32) -> io::Result<bool> {
        // POSIX specifies that a zero `si_pid` distinguishes the WNOHANG case.
        // Starting from all-zero bytes also avoids observing uninitialized
        // padding when the kernel reports no state change.
        // SAFETY: an all-zero `siginfo_t` is a valid writable output buffer;
        // `waitid` initializes it before any nonzero `si_pid` is inspected.
        let mut information = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        // SAFETY: `information` is live writable storage, `P_PID` selects the
        // exact positive child identifier, and WNOWAIT explicitly preserves
        // the waitable status for a later `wait` call.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid.get().unsigned_abs(),
                ptr::from_mut(&mut information),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `waitid` returned success and `si_pid` is the process field
        // selected by the WEXITED event class on Linux and macOS.
        Ok(unsafe { information.si_pid() } != 0)
    }

    /// Exchanges one bounded NUL-framed message over an inherited nonblocking
    /// Unix stream descriptor.
    ///
    /// The descriptor is accepted only when it is an open `AF_UNIX`,
    /// `SOCK_STREAM`, nonblocking socket. `request` must exclude the terminator;
    /// the returned length identifies response bytes before its terminator.
    /// This narrow capability is used by a session child to request work from
    /// its owning wrapper without reconstructing an unsafe borrowed Rust handle.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error for an empty, oversized, or NUL-bearing
    /// request, an invalid-data error for the wrong descriptor type or an
    /// oversized response, a timeout error when the deadline expires, or the
    /// underlying descriptor error.
    pub fn exchange_inherited_unix_frame(
        descriptor: NonZeroI32,
        request: &[u8],
        response: &mut [u8],
        timeout: Duration,
    ) -> io::Result<usize> {
        if request.is_empty()
            || request.len() > MAX_INHERITED_FRAME_BYTES
            || request.contains(&0)
            || response.is_empty()
            || response.len() > MAX_INHERITED_FRAME_BYTES
        {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        validate_inherited_unix_stream(descriptor.get())?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        write_frame(descriptor.get(), request, deadline)?;
        read_frame(descriptor.get(), response, deadline)
    }

    fn validate_inherited_unix_stream(descriptor: i32) -> io::Result<()> {
        // SAFETY: `fcntl` inspects only the numeric descriptor and does not
        // create Rust ownership. A negative return is handled immediately.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::O_NONBLOCK == 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }

        let mut socket_type = 0_i32;
        let mut type_length = libc::socklen_t::try_from(std::mem::size_of::<i32>())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        // SAFETY: both output pointers reference initialized writable storage
        // for the exact `SO_TYPE` value and length.
        let type_result = unsafe {
            libc::getsockopt(
                descriptor,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut socket_type).cast(),
                &raw mut type_length,
            )
        };
        if type_result == -1 {
            return Err(io::Error::last_os_error());
        }
        if socket_type != libc::SOCK_STREAM {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }

        // Starting from zeroed storage makes every byte initialized before the
        // kernel writes the actual address and its returned length.
        // SAFETY: all-zero `sockaddr_storage` is valid writable output storage.
        let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_storage>() };
        let mut address_length = libc::socklen_t::try_from(std::mem::size_of_val(&address))
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        // SAFETY: the descriptor is open, and both pointers reference live
        // writable storage with the supplied capacity.
        let address_result = unsafe {
            libc::getsockname(
                descriptor,
                (&raw mut address).cast(),
                &raw mut address_length,
            )
        };
        if address_result == -1 {
            return Err(io::Error::last_os_error());
        }
        if i32::from(address.ss_family) != libc::AF_UNIX {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        Ok(())
    }

    fn write_frame(descriptor: i32, request: &[u8], deadline: Instant) -> io::Result<()> {
        let mut written = 0_usize;
        let terminator = 0_u8;
        while written <= request.len() {
            wait_descriptor(descriptor, libc::POLLOUT, deadline)?;
            let (pointer, length) = if written == request.len() {
                ((&raw const terminator).cast(), 1_usize)
            } else {
                (request[written..].as_ptr().cast(), request.len() - written)
            };
            // SAFETY: `pointer` names `length` readable bytes, and the
            // descriptor was validated as nonblocking before this loop.
            let result = unsafe { libc::write(descriptor, pointer, length) };
            if result > 0 {
                let count = usize::try_from(result)
                    .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
                if written == request.len() {
                    return (count == 1)
                        .then_some(())
                        .ok_or_else(|| io::Error::from(io::ErrorKind::WriteZero));
                }
                written = written
                    .checked_add(count)
                    .filter(|total| *total <= request.len())
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
                continue;
            }
            if result == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(error);
        }
        Err(io::Error::from(io::ErrorKind::WriteZero))
    }

    fn read_frame(descriptor: i32, response: &mut [u8], deadline: Instant) -> io::Result<usize> {
        let mut length = 0_usize;
        loop {
            wait_descriptor(descriptor, libc::POLLIN, deadline)?;
            let mut byte = 0_u8;
            // SAFETY: `byte` is live writable storage for one byte, and the
            // descriptor was validated as nonblocking before this loop.
            let result = unsafe { libc::read(descriptor, (&raw mut byte).cast(), 1) };
            if result == 1 {
                if byte == 0 {
                    return Ok(length);
                }
                let Some(slot) = response.get_mut(length) else {
                    return Err(io::Error::from(io::ErrorKind::InvalidData));
                };
                *slot = byte;
                length += 1;
                continue;
            }
            if result == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(error);
        }
    }

    fn wait_descriptor(descriptor: i32, events: i16, deadline: Instant) -> io::Result<()> {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
            let timeout_millis = remaining
                .as_millis()
                .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0))
                .min(i32::MAX as u128);
            let timeout = i32::try_from(timeout_millis).unwrap_or(i32::MAX);
            let mut descriptor_event = libc::pollfd {
                fd: descriptor,
                events,
                revents: 0,
            };
            // SAFETY: `descriptor_event` is one live poll entry and `timeout`
            // is a finite nonnegative millisecond interval.
            let result = unsafe { libc::poll(&raw mut descriptor_event, 1, timeout) };
            if result > 0 {
                // A readiness bit outranks a hangup. A peer that writes a
                // complete response and then closes reports both at once, and
                // the response is still buffered and readable; treating the
                // hangup as fatal here would discard a delivered reply. End of
                // input is recognized by a zero-length read instead.
                if descriptor_event.revents & events != 0 {
                    return Ok(());
                }
                if descriptor_event.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
                {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
                continue;
            }
            if result == 0 {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
    }

    #[cfg(test)]
    mod tests {
        use std::io::{Read as _, Write as _};
        use std::num::NonZeroI32;
        use std::os::fd::AsRawFd as _;
        use std::os::unix::net::UnixStream;
        use std::process::Command;
        use std::thread;
        use std::time::{Duration, Instant};

        use super::*;

        #[test]
        fn observes_without_reaping_the_exact_child() {
            let mut child = Command::new("/bin/sh")
                .args(["-c", "exit 7"])
                .spawn()
                .unwrap();
            let pid = NonZeroI32::new(i32::try_from(child.id()).unwrap()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(1);
            while !peek_child_exit(pid).unwrap() {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(child.wait().unwrap().code(), Some(7));
        }

        #[test]
        fn running_child_is_not_reported_as_exited() {
            let mut child = Command::new("/bin/sh")
                .args(["-c", "sleep 1"])
                .spawn()
                .unwrap();
            let pid = NonZeroI32::new(i32::try_from(child.id()).unwrap()).unwrap();
            assert!(!peek_child_exit(pid).unwrap());
            child.kill().unwrap();
            child.wait().unwrap();
        }

        #[test]
        fn rejects_a_process_that_is_not_a_child() {
            let own_pid = NonZeroI32::new(i32::try_from(std::process::id()).unwrap()).unwrap();
            assert!(peek_child_exit(own_pid).is_err());
        }

        #[test]
        fn exchanges_one_correlated_inherited_stream_frame() {
            let (client, mut server) = UnixStream::pair().unwrap();
            client.set_nonblocking(true).unwrap();
            let responder = thread::spawn(move || {
                let mut request = [0_u8; 64];
                let mut length = 0;
                loop {
                    server.read_exact(&mut request[length..=length]).unwrap();
                    if request[length] == 0 {
                        break;
                    }
                    length += 1;
                }
                assert_eq!(&request[..length], b"reload-request:42");
                server.write_all(b"reload-ack:42:ok\0").unwrap();
            });
            let descriptor = NonZeroI32::new(client.as_raw_fd()).unwrap();
            let mut response = [0_u8; 64];
            let length = exchange_inherited_unix_frame(
                descriptor,
                b"reload-request:42",
                &mut response,
                Duration::from_secs(1),
            )
            .unwrap();
            assert_eq!(&response[..length], b"reload-ack:42:ok");
            responder.join().unwrap();
        }

        #[test]
        fn a_response_buffered_before_the_peer_closed_is_still_read() {
            let (client, server) = UnixStream::pair().unwrap();
            client.set_nonblocking(true).unwrap();
            thread::spawn(move || {
                let mut server = server;
                server.write_all(b"reload-ack:42:ok\0").unwrap();
            })
            .join()
            .unwrap();

            let descriptor = client.as_raw_fd();
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut response = [0_u8; 64];
            let length = read_frame(descriptor, &mut response, deadline).unwrap();
            assert_eq!(&response[..length], b"reload-ack:42:ok");
            assert_eq!(
                read_frame(descriptor, &mut response, deadline)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::UnexpectedEof
            );
        }

        #[test]
        fn inherited_exchange_rejects_regular_and_blocking_descriptors() {
            let temporary = tempfile::tempfile().unwrap();
            let descriptor = NonZeroI32::new(temporary.as_raw_fd()).unwrap();
            let mut response = [0_u8; 8];
            assert_eq!(
                exchange_inherited_unix_frame(
                    descriptor,
                    b"request",
                    &mut response,
                    Duration::from_millis(1),
                )
                .unwrap_err()
                .kind(),
                io::ErrorKind::InvalidData
            );

            let (blocking, _peer) = UnixStream::pair().unwrap();
            let descriptor = NonZeroI32::new(blocking.as_raw_fd()).unwrap();
            assert_eq!(
                exchange_inherited_unix_frame(
                    descriptor,
                    b"request",
                    &mut response,
                    Duration::from_millis(1),
                )
                .unwrap_err()
                .kind(),
                io::ErrorKind::InvalidData
            );
        }
    }
}

/// Audited wrappers around macOS-specific descriptor APIs.
#[cfg(target_os = "macos")]
pub mod macos {
    use std::io;
    use std::os::fd::{AsFd, AsRawFd};
    use std::ptr::{self, NonNull};

    use libc::{c_int, c_void};

    type Acl = *mut c_void;
    type AclEntry = *mut c_void;

    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: c_int = 0;
    const ACL_ENTRY_AVAILABLE: c_int = 0;
    const ACL_ENTRY_ERROR: c_int = -1;

    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> Acl;
        fn acl_get_entry(acl: Acl, entry_id: c_int, entry: *mut AclEntry) -> c_int;
        fn acl_free(object: *mut c_void) -> c_int;
    }

    struct OwnedAcl(NonNull<c_void>);

    impl OwnedAcl {
        const fn as_raw(&self) -> Acl {
            self.0.as_ptr()
        }
    }

    impl Drop for OwnedAcl {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the unique live ACL object returned by
            // `acl_get_fd_np`; this `Drop` runs exactly once and never uses it again.
            let _ = unsafe { acl_free(self.0.as_ptr()) };
        }
    }

    /// Reports whether an open file has at least one macOS extended ACL entry.
    ///
    /// Inspection uses only the borrowed descriptor. No pathname is reconstructed
    /// or disclosed, so concurrent renames cannot redirect the query.
    ///
    /// # Errors
    ///
    /// Returns the operating-system error from ACL retrieval or enumeration, or
    /// [`io::ErrorKind::InvalidData`] for an impossible ABI result.
    pub fn has_extended_acl(file: impl AsFd) -> io::Result<bool> {
        let descriptor = file.as_fd();
        // SAFETY: the borrowed descriptor is live for this synchronous call, and
        // `ACL_TYPE_EXTENDED` is the typed value from macOS `<sys/acl.h>`.
        let acl = unsafe { acl_get_fd_np(descriptor.as_raw_fd(), ACL_TYPE_EXTENDED) };
        let Some(acl) = NonNull::new(acl) else {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ENOENT) {
                Ok(false)
            } else {
                Err(error)
            };
        };
        let acl = OwnedAcl(acl);
        let mut entry = ptr::null_mut();
        // SAFETY: `acl` owns a live ACL object, `ACL_FIRST_ENTRY` starts bounded
        // enumeration, and `entry` points to writable storage for the result.
        let result = unsafe { acl_get_entry(acl.as_raw(), ACL_FIRST_ENTRY, &raw mut entry) };
        match result {
            ACL_ENTRY_AVAILABLE if entry.is_null() => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ACL enumeration returned an empty entry",
            )),
            ACL_ENTRY_AVAILABLE => Ok(true),
            ACL_ENTRY_ERROR => {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINVAL) {
                    Ok(false)
                } else {
                    Err(error)
                }
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ACL enumeration returned an invalid status",
            )),
        }
    }

    #[cfg(test)]
    mod tests {
        use std::fs::{self, File};
        use std::path::Path;
        use std::process::{Command, Stdio};

        use super::*;

        fn add_extended_acl(path: &Path) {
            let identity = Command::new("/usr/bin/id").arg("-un").output().unwrap();
            assert!(identity.status.success());
            let user = String::from_utf8(identity.stdout).unwrap();
            let status = Command::new("/bin/chmod")
                .arg("+a")
                .arg(format!("user:{} allow read", user.trim()))
                .arg(path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
        }

        #[test]
        fn acl_free_file_reports_false() {
            let temporary = tempfile::tempdir().unwrap();
            let path = temporary.path().join("plain");
            fs::write(&path, b"plain").unwrap();
            assert!(!has_extended_acl(File::open(path).unwrap()).unwrap());
        }

        #[test]
        fn extended_acl_file_reports_true() {
            let temporary = tempfile::tempdir().unwrap();
            let path = temporary.path().join("protected");
            fs::write(&path, b"protected").unwrap();
            add_extended_acl(&path);
            assert!(has_extended_acl(File::open(path).unwrap()).unwrap());
        }

        #[test]
        fn descriptor_survives_parent_and_name_substitution() {
            let temporary = tempfile::tempdir().unwrap();
            let parent = temporary.path().join("config");
            let detached = temporary.path().join("detached-config");
            fs::create_dir(&parent).unwrap();
            let path = parent.join(".bashrc");
            fs::write(&path, b"protected").unwrap();
            add_extended_acl(&path);
            let protected = File::open(&path).unwrap();

            fs::rename(&parent, &detached).unwrap();
            fs::create_dir(&parent).unwrap();
            let rebound_victim = parent.join(".bashrc");
            fs::write(&rebound_victim, b"victim").unwrap();
            let detached_name = detached.join("detached-name");
            fs::rename(detached.join(".bashrc"), &detached_name).unwrap();
            fs::write(detached.join(".bashrc"), b"second victim").unwrap();

            assert!(has_extended_acl(&protected).unwrap());
            assert!(!has_extended_acl(File::open(rebound_victim).unwrap()).unwrap());
            assert!(!has_extended_acl(File::open(detached.join(".bashrc")).unwrap()).unwrap());
            assert!(has_extended_acl(File::open(detached_name).unwrap()).unwrap());
        }
    }
}
