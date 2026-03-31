use std::path::PathBuf;

/// Query the current working directory of a process by PID.
/// Returns None if the query fails (permissions, process exited, unsupported platform).
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        macos_process_cwd(pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux_process_cwd(pid)
    }
    #[cfg(windows)]
    {
        windows_process_cwd(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_process_cwd(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::mem;

    const PROC_PIDVNODEPATHINFO: libc::c_int = 9;

    // struct vnode_info_path: vnode_info (152 bytes) + path (MAXPATHLEN=1024)
    #[repr(C)]
    struct VnodeInfoPath {
        _vip_vi: [u8; 152], // struct vnode_info
        vip_path: [u8; libc::MAXPATHLEN as usize],
    }

    // struct proc_vnodepathinfo: cdir + rdir
    #[repr(C)]
    struct ProcVnodePathInfo {
        pvi_cdir: VnodeInfoPath,
        _pvi_rdir: VnodeInfoPath,
    }

    // Static size assertion: sizeof(proc_vnodepathinfo) == 2352 on macOS
    const _: () = assert!(mem::size_of::<ProcVnodePathInfo>() == 2352);

    let mut info: ProcVnodePathInfo = unsafe { mem::zeroed() };
    let size = mem::size_of::<ProcVnodePathInfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };

    if ret <= 0 {
        return None;
    }

    let cstr = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr() as *const libc::c_char) };
    let path = PathBuf::from(cstr.to_string_lossy().into_owned());
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

#[cfg(target_os = "linux")]
fn linux_process_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{}/cwd", pid)).ok()
}

/// Windows CWD query via NtQueryInformationProcess.
///
/// Opens the child process, reads its PEB to find ProcessParameters,
/// then reads the CurrentDirectory.DosPath UNICODE_STRING.
///
/// Uses raw FFI because NtQueryInformationProcess is an ntdll function
/// not available through windows-sys. This is the same approach used by
/// Alacritty and WezTerm.
///
/// PEB offsets are x64-specific (target: x86_64-pc-windows-msvc only).
#[cfg(windows)]
fn windows_process_cwd(pid: u32) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::mem;
    use std::os::windows::ffi::OsStringExt;
    use std::ptr;

    const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
    const PROCESS_VM_READ: u32 = 0x0010;
    const PROCESS_BASIC_INFORMATION: u32 = 0;

    #[repr(C)]
    struct ProcessBasicInformation {
        reserved1: usize,
        peb_base_address: usize,
        reserved2: [usize; 2],
        unique_process_id: usize,
        reserved3: usize,
    }

    // UNICODE_STRING layout on x64:
    // Length (u16) + MaximumLength (u16) + alignment padding (u32) + Buffer (usize)
    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        _padding: u32,
        buffer: usize,
    }

    extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, pid: u32) -> isize;
        fn CloseHandle(handle: isize) -> i32;
        fn ReadProcessMemory(
            process: isize,
            base: usize,
            buffer: *mut u8,
            size: usize,
            bytes_read: *mut usize,
        ) -> i32;
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(
            process: isize,
            info_class: u32,
            info: *mut u8,
            info_length: u32,
            return_length: *mut u32,
        ) -> i32; // NTSTATUS
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle == 0 || handle == -1 {
            return None;
        }

        let result = (|| -> Option<PathBuf> {
            // Step 1: Get PEB address via NtQueryInformationProcess
            let mut pbi: ProcessBasicInformation = mem::zeroed();
            let status = NtQueryInformationProcess(
                handle,
                PROCESS_BASIC_INFORMATION,
                &mut pbi as *mut _ as *mut u8,
                mem::size_of::<ProcessBasicInformation>() as u32,
                ptr::null_mut(),
            );
            if status < 0 {
                return None;
            }

            // Step 2: Read ProcessParameters pointer from PEB
            // On x64: PEB + 0x20 = ProcessParameters pointer
            let params_ptr_addr = pbi.peb_base_address + 0x20;
            let mut params_ptr: usize = 0;
            let ok = ReadProcessMemory(
                handle,
                params_ptr_addr,
                &mut params_ptr as *mut _ as *mut u8,
                mem::size_of::<usize>(),
                ptr::null_mut(),
            );
            if ok == 0 || params_ptr == 0 {
                return None;
            }

            // Step 3: Read CurrentDirectory.DosPath (UNICODE_STRING) from
            // RTL_USER_PROCESS_PARAMETERS.
            // On x64: ProcessParameters + 0x38 = CurrentDirectory.DosPath
            let cwd_ustr_addr = params_ptr + 0x38;
            let mut ustr: UnicodeString = mem::zeroed();
            let ok = ReadProcessMemory(
                handle,
                cwd_ustr_addr,
                &mut ustr as *mut _ as *mut u8,
                mem::size_of::<UnicodeString>(),
                ptr::null_mut(),
            );
            if ok == 0 || ustr.length == 0 || ustr.buffer == 0 {
                return None;
            }

            // Step 4: Read the wide-char path string
            let char_count = ustr.length as usize / 2;
            let mut wide_buf = vec![0u16; char_count];
            let ok = ReadProcessMemory(
                handle,
                ustr.buffer,
                wide_buf.as_mut_ptr() as *mut u8,
                ustr.length as usize,
                ptr::null_mut(),
            );
            if ok == 0 {
                return None;
            }

            // Strip trailing backslash ONLY if it's not a drive root (e.g. C:\)
            // or UNC root. Windows CWD paths often end with \ but removing it
            // from "C:\" produces "C:" which is semantically different (relative
            // to the current directory on that drive, not the drive root).
            let len = wide_buf.len();
            if len > 1 && wide_buf[len - 1] == b'\\' as u16 && wide_buf[len - 2] != b':' as u16 {
                wide_buf.pop();
            }

            let path = PathBuf::from(OsString::from_wide(&wide_buf));
            if path.as_os_str().is_empty() {
                None
            } else {
                Some(path)
            }
        })();

        CloseHandle(handle);
        result
    }
}
