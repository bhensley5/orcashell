use crate::IpcEndpoint;
use std::cell::Cell;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::SystemServices::SE_GROUP_LOGON_ID;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a Rust string to a null-terminated wide (UTF-16) string.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Retrieve the SID string of the current process logon session.
pub(crate) fn get_current_logon_sid() -> io::Result<String> {
    unsafe {
        let process = GetCurrentProcess();
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }

        let result = token_logon_sid(token);
        CloseHandle(token);
        result
    }
}

fn token_logon_sid(token: HANDLE) -> io::Result<String> {
    unsafe {
        let mut needed: u32 = 0;
        let _ = GetTokenInformation(token, TokenGroups, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut buf = vec![0u8; needed as usize];
        if GetTokenInformation(
            token,
            TokenGroups,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }

        let token_groups = &*(buf.as_ptr() as *const TOKEN_GROUPS);
        for index in 0..token_groups.GroupCount as usize {
            let group = *token_groups.Groups.as_ptr().add(index);
            if group.Attributes & (SE_GROUP_LOGON_ID as u32) != 0 {
                return sid_to_string(group.Sid);
            }
        }

        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "token has no logon SID",
        ))
    }
}

unsafe fn sid_to_string(sid: PSID) -> io::Result<String> {
    let mut sid_str: *mut u16 = std::ptr::null_mut();
    if ConvertSidToStringSidW(sid, &mut sid_str) == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut len = 0usize;
    while *sid_str.add(len) != 0 {
        len += 1;
    }
    let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, len));
    LocalFree(sid_str.cast());
    Ok(sid)
}

fn client_logon_sid(pipe: HANDLE) -> io::Result<String> {
    unsafe {
        if ImpersonateNamedPipeClient(pipe) == 0 {
            return Err(io::Error::last_os_error());
        }
        let _guard = ImpersonationGuard;

        let mut token: HANDLE = std::ptr::null_mut();
        if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, TRUE, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }

        let result = token_logon_sid(token);
        CloseHandle(token);
        result
    }
}

struct ImpersonationGuard;

impl Drop for ImpersonationGuard {
    fn drop(&mut self) {
        unsafe {
            RevertToSelf();
        }
    }
}

fn verify_client_logon(pipe: HANDLE, expected_logon_sid: &str) -> io::Result<()> {
    let actual = client_logon_sid(pipe)?;
    if actual == expected_logon_sid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "rejected IPC client from another logon session",
        ))
    }
}

// ---------------------------------------------------------------------------
// Security descriptor
// ---------------------------------------------------------------------------

/// RAII wrapper around a security descriptor allocated by
/// `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
struct SecurityDesc {
    ptr: PSECURITY_DESCRIPTOR,
}

impl SecurityDesc {
    fn for_logon_session(logon_sid: &str) -> io::Result<Self> {
        let sddl = format!("D:P(A;;GA;;;{logon_sid})");
        let sddl_wide = to_wide(&sddl);
        let mut ptr: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        unsafe {
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl_wide.as_ptr(),
                1, // SDDL_REVISION_1
                &mut ptr,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self { ptr })
    }

    fn as_security_attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.ptr,
            bInheritHandle: FALSE,
        }
    }
}

impl Drop for SecurityDesc {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                LocalFree(self.ptr.cast());
            }
        }
    }
}

// Safety: the descriptor is created once during bind() and never mutated.
unsafe impl Send for SecurityDesc {}
unsafe impl Sync for SecurityDesc {}

// ---------------------------------------------------------------------------
// IpcListener
// ---------------------------------------------------------------------------

const PIPE_BUFFER_SIZE: u32 = 4096;
const PIPE_DEFAULT_TIMEOUT_MS: u32 = 5000;
// Keep pipe capacity above the daemon handler cap. The listener always tries to
// keep one pending instance ready while accepted streams remain open.
pub const PIPE_MAX_INSTANCES: u32 = 32;
const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_secs(5);

fn client_open_flags() -> u32 {
    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION
}

fn pipe_mode() -> u32 {
    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS
}

fn log_prepare_next_error(error: &io::Error, message: &str) {
    if error.kind() == io::ErrorKind::WouldBlock {
        tracing::debug!(error = %error, "{message}");
    } else {
        tracing::warn!(error = %error, "{message}");
    }
}

fn is_retryable_accept_error(error_code: u32) -> bool {
    matches!(
        error_code,
        ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED | ERROR_OPERATION_ABORTED
    )
}

fn retryable_accept_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "IPC client disconnected during accept",
    )
}

fn pipe_instances_exhausted_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "pipe instances temporarily exhausted",
    )
}

fn client_verification_accept_error(error: io::Error) -> io::Error {
    if error
        .raw_os_error()
        .map(|code| is_retryable_accept_error(code as u32))
        .unwrap_or(false)
    {
        retryable_accept_error()
    } else if error.kind() == io::ErrorKind::PermissionDenied {
        error
    } else {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("rejected IPC client: {error}"),
        )
    }
}

pub struct IpcListener {
    pipe_name: Vec<u16>,
    _security_desc: SecurityDesc,
    security_attrs: SECURITY_ATTRIBUTES,
    logon_sid: String,
    pending_handle: HANDLE,
    overlapped: OVERLAPPED,
    event: HANDLE,
    nonblocking: AtomicBool,
    /// True until the first `ConnectNamedPipe` is issued from `accept()`.
    /// We must NOT call `ConnectNamedPipe` in `bind()` because the
    /// `OVERLAPPED` struct is on the stack there and gets moved into `Self`,
    /// invalidating the pointer Windows holds internally.
    needs_connect: bool,
}

// Safety: the listener is used from a single accept-loop thread. The pipe
// handle and event are valid across thread boundaries.
unsafe impl Send for IpcListener {}

impl IpcListener {
    pub fn bind(endpoint: &IpcEndpoint) -> io::Result<Self> {
        let pipe_name = to_wide(&endpoint.address);

        // -- Conflict detection: probe-connect as a client --
        let probe = unsafe {
            CreateFileW(
                pipe_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                client_open_flags(),
                std::ptr::null_mut(),
            )
        };
        if probe != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(probe);
            }
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("daemon already running at {}", endpoint.display_name),
            ));
        }

        let probe_err = io::Error::last_os_error();
        match probe_err.raw_os_error().map(|e| e as u32) {
            Some(ERROR_FILE_NOT_FOUND) => { /* no pipe exists. Safe to proceed */ }
            Some(ERROR_PIPE_BUSY) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("daemon already running at {}", endpoint.display_name),
                ));
            }
            Some(ERROR_ACCESS_DENIED) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!(
                        "endpoint occupied by another user at {}",
                        endpoint.display_name,
                    ),
                ));
            }
            _ => { /* no evidence of a live server. Proceed */ }
        }

        // -- Build security descriptor restricted to the current user --
        let logon_sid = get_current_logon_sid()?;
        let security_desc = SecurityDesc::for_logon_session(&logon_sid)?;
        let security_attrs = security_desc.as_security_attributes();

        // -- Create the first pipe instance --
        let handle = unsafe {
            CreateNamedPipeW(
                pipe_name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                pipe_mode(),
                PIPE_MAX_INSTANCES,
                PIPE_BUFFER_SIZE,
                PIPE_BUFFER_SIZE,
                PIPE_DEFAULT_TIMEOUT_MS,
                &security_attrs,
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            let err = io::Error::last_os_error();
            // Another process won the race between our probe and our create.
            if err.raw_os_error().map(|e| e as u32) == Some(ERROR_ACCESS_DENIED) {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!(
                        "endpoint claimed by another process at {}",
                        endpoint.display_name,
                    ),
                ));
            }
            return Err(err);
        }

        // -- Create event for overlapped accept --
        let event = unsafe { CreateEventW(std::ptr::null(), TRUE, FALSE, std::ptr::null()) };
        if event.is_null() {
            let err = io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(err);
        }

        let overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };

        // NOTE: We do NOT call ConnectNamedPipe here. The OVERLAPPED struct
        // lives on the stack and will be moved into Self, invalidating the
        // pointer Windows holds. The first ConnectNamedPipe is deferred to
        // accept(), where &mut self.overlapped is at its final address.

        Ok(Self {
            pipe_name,
            _security_desc: security_desc,
            security_attrs,
            logon_sid,
            pending_handle: handle,
            overlapped,
            event,
            nonblocking: AtomicBool::new(false),
            needs_connect: true,
        })
    }

    pub fn accept(&mut self) -> io::Result<IpcStream> {
        if self.pending_handle == INVALID_HANDLE_VALUE {
            self.prepare_next_pipe_instance()?;
        }

        // Issue the first ConnectNamedPipe now that self.overlapped is at a
        // stable address (deferred from bind() to avoid use-after-move).
        if self.needs_connect {
            self.overlapped = unsafe { std::mem::zeroed() };
            self.overlapped.hEvent = self.event;

            let ok = unsafe { ConnectNamedPipe(self.pending_handle, &mut self.overlapped) };
            if ok != 0 {
                unsafe {
                    SetEvent(self.event);
                }
            } else {
                let err_code = unsafe { GetLastError() };
                match err_code {
                    ERROR_IO_PENDING => { /* normal: waiting for client */ }
                    ERROR_PIPE_CONNECTED => unsafe {
                        SetEvent(self.event);
                    },
                    _ if is_retryable_accept_error(err_code) => {
                        return self.recover_after_transient_accept_error(err_code);
                    }
                    _ => {
                        return Err(io::Error::from_raw_os_error(err_code as i32));
                    }
                }
            }
            self.needs_connect = false;
        }

        let wait_ms = if self.nonblocking.load(Ordering::Acquire) {
            0
        } else {
            INFINITE
        };

        let wait_result = unsafe { WaitForSingleObject(self.event, wait_ms) };
        if wait_result == WAIT_TIMEOUT {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        if wait_result != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }

        // Confirm the connection completed.
        let mut bytes: u32 = 0;
        let ok = unsafe {
            GetOverlappedResult(self.pending_handle, &self.overlapped, &mut bytes, FALSE)
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            // ERROR_PIPE_CONNECTED is not an error. Client connected.
            match err.raw_os_error().map(|e| e as u32) {
                Some(ERROR_PIPE_CONNECTED) => {}
                Some(err_code) if is_retryable_accept_error(err_code) => {
                    return self.recover_after_transient_accept_error(err_code);
                }
                _ => {
                    return Err(err);
                }
            }
        }

        if self.pending_handle == INVALID_HANDLE_VALUE {
            return Err(retryable_accept_error());
        }

        // Move ownership of the connected handle into the accepted stream.
        // From this point on, self.pending_handle must not close this handle.
        let connected_handle = self.pending_handle;
        self.pending_handle = INVALID_HANDLE_VALUE;

        if let Err(e) = verify_client_logon(connected_handle, &self.logon_sid) {
            let accept_error = client_verification_accept_error(e);
            unsafe {
                CloseHandle(connected_handle);
            }
            if let Err(next_err) = self.prepare_next_pipe_instance() {
                log_prepare_next_error(
                    &next_err,
                    "failed to prepare next pipe instance after rejecting client",
                );
            }
            return Err(accept_error);
        }

        // Wrap the connected handle in an IpcStream.
        let stream_event = unsafe { CreateEventW(std::ptr::null(), TRUE, FALSE, std::ptr::null()) };
        if stream_event.is_null() {
            let err = io::Error::last_os_error();
            unsafe {
                CloseHandle(connected_handle);
            }
            if let Err(next_err) = self.prepare_next_pipe_instance() {
                log_prepare_next_error(
                    &next_err,
                    "failed to prepare next pipe instance after stream setup failure",
                );
            }
            return Err(err);
        }
        let stream = IpcStream {
            handle: connected_handle,
            event: stream_event,
            read_timeout: Cell::new(Some(DEFAULT_STREAM_TIMEOUT)),
            write_timeout: Cell::new(Some(DEFAULT_STREAM_TIMEOUT)),
        };

        if let Err(e) = self.prepare_next_pipe_instance() {
            log_prepare_next_error(&e, "failed to create next pipe instance");
        }

        Ok(stream)
    }

    fn recover_after_transient_accept_error(&mut self, error_code: u32) -> io::Result<IpcStream> {
        tracing::debug!(
            error_code,
            "IPC client disconnected before accept completed; resetting pipe instance"
        );
        if self.pending_handle != INVALID_HANDLE_VALUE {
            unsafe {
                CancelIoEx(self.pending_handle, std::ptr::null());
                CloseHandle(self.pending_handle);
            }
            self.pending_handle = INVALID_HANDLE_VALUE;
        }
        self.needs_connect = false;

        match self.prepare_next_pipe_instance() {
            Ok(()) => Err(retryable_accept_error()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(retryable_accept_error()),
            Err(e) => Err(e),
        }
    }

    fn prepare_next_pipe_instance(&mut self) -> io::Result<()> {
        // -- Prepare a new pipe instance for the next client --
        // FILE_FLAG_FIRST_PIPE_INSTANCE is intentionally omitted on subsequent
        // instances: the first instance already established ownership and
        // subsequent instances are just additional connection slots.
        let new_handle = unsafe {
            CreateNamedPipeW(
                self.pipe_name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                pipe_mode(),
                PIPE_MAX_INSTANCES,
                PIPE_BUFFER_SIZE,
                PIPE_BUFFER_SIZE,
                PIPE_DEFAULT_TIMEOUT_MS,
                &self.security_attrs,
            )
        };
        if new_handle == INVALID_HANDLE_VALUE {
            let err = io::Error::last_os_error();
            self.pending_handle = INVALID_HANDLE_VALUE;
            if err.raw_os_error().map(|e| e as u32) == Some(ERROR_PIPE_BUSY) {
                return Err(pipe_instances_exhausted_error());
            }
            return Err(err);
        }

        // Reset event and begin waiting for next client.
        unsafe {
            ResetEvent(self.event);
        }

        self.pending_handle = new_handle;
        self.overlapped = unsafe { std::mem::zeroed() };
        self.overlapped.hEvent = self.event;

        let ok = unsafe { ConnectNamedPipe(new_handle, &mut self.overlapped) };
        if ok != 0 {
            // immediate connect. Signal event so next accept() picks it up
            unsafe {
                SetEvent(self.event);
            }
        } else {
            let err_code = unsafe { GetLastError() };
            match err_code {
                ERROR_IO_PENDING => { /* normal */ }
                ERROR_PIPE_CONNECTED => unsafe {
                    SetEvent(self.event);
                },
                _ => {
                    // ConnectNamedPipe failed for the next instance. Clean up;
                    // transient capacity failures are retried on a later accept().
                    tracing::warn!(
                        error_code = err_code,
                        "ConnectNamedPipe failed for next pipe instance"
                    );
                    unsafe {
                        CloseHandle(new_handle);
                    }
                    self.pending_handle = INVALID_HANDLE_VALUE;
                    if err_code == ERROR_PIPE_BUSY {
                        return Err(pipe_instances_exhausted_error());
                    }
                    if is_retryable_accept_error(err_code) {
                        return Err(retryable_accept_error());
                    }
                    return Err(io::Error::from_raw_os_error(err_code as i32));
                }
            }
        }

        Ok(())
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Ordering::Release);
        Ok(())
    }
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        unsafe {
            if self.pending_handle != INVALID_HANDLE_VALUE {
                // Cancel any pending overlapped ConnectNamedPipe.
                CancelIoEx(self.pending_handle, std::ptr::null());
                CloseHandle(self.pending_handle);
            }
            CloseHandle(self.event);
        }
    }
}

// ---------------------------------------------------------------------------
// IpcStream
// ---------------------------------------------------------------------------

pub struct IpcStream {
    handle: HANDLE,
    event: HANDLE,
    /// Per-stream read timeout, enforced via overlapped I/O + WaitForSingleObject.
    /// Uses `Cell` for safe interior mutability (single-threaded per-stream).
    read_timeout: Cell<Option<Duration>>,
    /// Per-stream write timeout.
    write_timeout: Cell<Option<Duration>>,
}

// Safety: pipe handles in our blocking-with-timeout model are safe to send
// across threads. Cell fields are only accessed from the owning thread.
unsafe impl Send for IpcStream {}

impl IpcStream {
    pub fn connect(endpoint: &IpcEndpoint, timeout: Duration) -> io::Result<Self> {
        let pipe_name = to_wide(&endpoint.address);
        let deadline = std::time::Instant::now() + timeout;

        loop {
            let handle = unsafe {
                CreateFileW(
                    pipe_name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    client_open_flags(),
                    std::ptr::null_mut(),
                )
            };

            if handle != INVALID_HANDLE_VALUE {
                let event =
                    unsafe { CreateEventW(std::ptr::null(), TRUE, FALSE, std::ptr::null()) };
                if event.is_null() {
                    let err = io::Error::last_os_error();
                    unsafe {
                        CloseHandle(handle);
                    }
                    return Err(err);
                }
                return Ok(Self {
                    handle,
                    event,
                    read_timeout: Cell::new(Some(timeout)),
                    write_timeout: Cell::new(Some(timeout)),
                });
            }

            let err_code = unsafe { GetLastError() };
            if err_code == ERROR_PIPE_BUSY {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for pipe",
                    ));
                }
                let wait_ms = remaining.as_millis().min(u32::MAX as u128) as u32;
                if unsafe { WaitNamedPipeW(pipe_name.as_ptr(), wait_ms) } == 0 {
                    let wait_err = unsafe { GetLastError() };
                    // ERROR_SEM_TIMEOUT (121) is the actual timeout error
                    // from WaitNamedPipeW. Other errors should propagate
                    // with their real error code.
                    if wait_err == ERROR_SEM_TIMEOUT {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "timed out waiting for pipe",
                        ));
                    }
                    return Err(io::Error::from_raw_os_error(wait_err as i32));
                }
                continue;
            }

            return Err(io::Error::from_raw_os_error(err_code as i32));
        }
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout.set(timeout);
        Ok(())
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.write_timeout.set(timeout);
        Ok(())
    }

    /// Perform an overlapped I/O operation with timeout enforcement.
    fn io_with_timeout(
        &mut self,
        timeout: Option<Duration>,
        op: impl FnOnce(HANDLE, &mut OVERLAPPED) -> (i32, u32),
    ) -> io::Result<u32> {
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = self.event;
        unsafe {
            ResetEvent(self.event);
        }

        let (ok, mut bytes_transferred) = op(self.handle, &mut overlapped);

        if ok != 0 {
            // Completed synchronously.
            return Ok(bytes_transferred);
        }

        let err_code = unsafe { GetLastError() };
        if err_code == ERROR_BROKEN_PIPE {
            return Ok(0); // EOF
        }
        if err_code != ERROR_IO_PENDING {
            return Err(io::Error::from_raw_os_error(err_code as i32));
        }

        // I/O is pending. Wait with timeout.
        let wait_ms = match timeout {
            Some(d) => d.as_millis().min(u32::MAX as u128) as u32,
            None => INFINITE,
        };
        let wait_result = unsafe { WaitForSingleObject(self.event, wait_ms) };

        match wait_result {
            WAIT_OBJECT_0 => {
                if unsafe {
                    GetOverlappedResult(self.handle, &overlapped, &mut bytes_transferred, FALSE)
                } == 0
                {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error().map(|e| e as u32) == Some(ERROR_BROKEN_PIPE) {
                        return Ok(0); // EOF
                    }
                    return Err(err);
                }
                Ok(bytes_transferred)
            }
            WAIT_TIMEOUT => {
                // Cancel the pending I/O and wait for cancellation to complete.
                unsafe {
                    CancelIoEx(self.handle, &overlapped);
                    GetOverlappedResult(
                        self.handle,
                        &overlapped,
                        &mut bytes_transferred,
                        TRUE, // wait for cancellation
                    );
                }
                Err(io::Error::new(io::ErrorKind::TimedOut, "I/O timed out"))
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let timeout = self.read_timeout.get();
        self.io_with_timeout(timeout, |handle, overlapped| {
            let mut bytes_read: u32 = 0;
            let ok = unsafe {
                ReadFile(
                    handle,
                    buf.as_mut_ptr().cast(),
                    buf.len() as u32,
                    &mut bytes_read,
                    overlapped,
                )
            };
            (ok, bytes_read)
        })
        .map(|n| n as usize)
    }
}

impl Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let timeout = self.write_timeout.get();
        self.io_with_timeout(timeout, |handle, overlapped| {
            let mut bytes_written: u32 = 0;
            let ok = unsafe {
                WriteFile(
                    handle,
                    buf.as_ptr().cast(),
                    buf.len() as u32,
                    &mut bytes_written,
                    overlapped,
                )
            };
            (ok, bytes_written)
        })
        .map(|n| n as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        // FlushFileBuffers is not usable on overlapped-mode pipe handles
        // (returns ERROR_INVALID_PARAMETER). Byte-mode WriteFile already
        // places data in the pipe buffer, and we no longer call
        // DisconnectNamedPipe in Drop, so buffered data survives for the
        // peer to read.
        Ok(())
    }
}

impl Drop for IpcStream {
    fn drop(&mut self) {
        unsafe {
            // Do NOT call DisconnectNamedPipe here. It discards unread data
            // in the pipe buffer, causing the peer to see UnexpectedEof. We
            // create a new pipe instance per connection (no reuse), so
            // DisconnectNamedPipe is unnecessary. CloseHandle alone puts the
            // pipe into closing-pending state; the peer can still drain
            // buffered data before seeing ERROR_BROKEN_PIPE (EOF).
            CloseHandle(self.event);
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_opens_always_use_sqos_identification() {
        let flags = client_open_flags();
        assert_ne!(flags & FILE_FLAG_OVERLAPPED, 0);
        assert_ne!(flags & SECURITY_SQOS_PRESENT, 0);
        assert_ne!(flags & SECURITY_IDENTIFICATION, 0);
    }

    #[test]
    fn pipe_mode_rejects_remote_clients() {
        let mode = pipe_mode();
        assert_ne!(mode & PIPE_REJECT_REMOTE_CLIENTS, 0);
        assert_eq!(mode & PIPE_TYPE_MESSAGE, 0);
        assert_eq!(mode & PIPE_READMODE_MESSAGE, 0);
    }

    #[test]
    fn pipe_capacity_reserves_pending_instance_headroom() {
        assert!(PIPE_MAX_INSTANCES >= 2);
    }

    #[test]
    fn transient_verification_errors_are_retryable_accept_errors() {
        let err = io::Error::from_raw_os_error(ERROR_NO_DATA as i32);
        let mapped = client_verification_accept_error(err);
        assert_eq!(mapped.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn non_transient_verification_errors_are_rejections() {
        let err = io::Error::other("impersonation failed");
        let mapped = client_verification_accept_error(err);
        assert_eq!(mapped.kind(), io::ErrorKind::PermissionDenied);
    }
}
