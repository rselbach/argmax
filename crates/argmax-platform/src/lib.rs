//! Safe, descriptor-anchored inspection of macOS extended ACLs.

#![cfg(target_os = "macos")]

/// Audited wrappers around macOS-specific descriptor APIs.
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
