// SPDX-License-Identifier: GPL-3.0-or-later

//! Host stdout connection checks shared by session supervisors.

use std::io;

/// A closed pipe can be detected on Darwin even when no frame is being sent.
pub fn check_connection() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::{c_int, c_short, c_uint};
        use std::os::fd::AsRawFd;

        // Layout and flags from Darwin <sys/poll.h>. POLLOUT is required for a
        // pipe's write end to report loss of its reader.
        const POLLOUT: c_short = 0x04;
        const POLLERR: c_short = 0x08;
        const POLLHUP: c_short = 0x10;
        const POLLNVAL: c_short = 0x20;

        #[repr(C)]
        struct PollFd {
            fd: c_int,
            events: c_short,
            revents: c_short,
        }
        unsafe extern "C" {
            fn poll(fds: *mut PollFd, count: c_uint, timeout_ms: c_int) -> c_int;
        }

        let mut output = PollFd {
            fd: io::stdout().as_raw_fd(),
            events: POLLOUT,
            revents: 0,
        };
        // SAFETY: output is one writable pollfd and a zero timeout never waits.
        let result = unsafe { poll(&mut output, 1, 0) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        } else if (output.revents & (POLLERR | POLLHUP | POLLNVAL)) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "host output disconnected",
            ));
        }
    }
    Ok(())
}
