//! A temporary renderer must not outlive its owner, even on process::exit or
//! a crash. Create it inside a non-inheritable kill-on-close Windows job; never
//! attach after spawn, when the debugger (or a descendant) could already run.
#[cfg(windows)]
mod platform {
    use std::ffi::{OsStr, OsString};
    use std::io;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
        UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW,
        EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
        PROC_THREAD_ATTRIBUTE_JOB_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    };

    struct Attributes(Vec<usize>);

    impl Attributes {
        fn new() -> io::Result<Self> {
            let mut bytes = 0;
            // SAFETY: the null-buffer call asks Windows for the required size.
            unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes) };
            if bytes == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buffer = vec![0usize; bytes.div_ceil(size_of::<usize>())];
            // SAFETY: this buffer is word-aligned and at least `bytes` long.
            if unsafe {
                InitializeProcThreadAttributeList(buffer.as_mut_ptr().cast(), 1, 0, &mut bytes)
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(buffer))
        }

        fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            self.0.as_mut_ptr().cast()
        }
    }

    impl Drop for Attributes {
        fn drop(&mut self) {
            // SAFETY: new() returns only an initialized list; storage lives on.
            unsafe { DeleteProcThreadAttributeList(self.pointer()) };
        }
    }

    fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
        let mut result: Vec<_> = value.encode_wide().collect();
        if result.contains(&0) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        result.push(0);
        Ok(result)
    }

    // Windows CRT quoting, including trailing backslashes and embedded quotes.
    // Keep paths as UTF-16 rather than round-tripping through lossy UTF-8.
    fn command_line(path: &Path, args: &[OsString]) -> io::Result<Vec<u16>> {
        let mut result = Vec::new();
        for arg in std::iter::once(path.as_os_str()).chain(args.iter().map(OsString::as_os_str)) {
            if !result.is_empty() {
                result.push(b' ' as u16);
            }
            result.push(b'"' as u16);
            let mut slashes = 0;
            for unit in arg.encode_wide() {
                if unit == 0 {
                    return Err(io::Error::from(io::ErrorKind::InvalidInput));
                }
                if unit == b'\\' as u16 {
                    slashes += 1;
                    continue;
                }
                if unit == b'"' as u16 {
                    result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
                } else {
                    result.extend(std::iter::repeat_n(b'\\' as u16, slashes));
                }
                slashes = 0;
                result.push(unit);
            }
            result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
            result.push(b'"' as u16);
        }
        result.push(0);
        Ok(result)
    }

    pub struct OwnedProcess {
        job: Option<OwnedHandle>,
        process: OwnedHandle,
    }

    impl OwnedProcess {
        pub fn spawn(path: &Path, args: &[OsString]) -> io::Result<Self> {
            let executable = wide(path.as_os_str())?;
            let mut line = command_line(path, args)?;
            // SAFETY: null security attributes create a non-inheritable handle.
            let raw_job = unsafe { CreateJobObjectW(null(), null()) };
            if raw_job.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: Windows returned a new, uniquely owned job handle.
            let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };
            // SAFETY: all-zero is a valid initial limits structure.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: structure and size agree; `job` stays open throughout.
            if unsafe {
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            // The handle array must outlive the attribute list that points to it.
            let mut jobs = [job.as_raw_handle()];
            let mut attributes = Attributes::new()?;
            // SAFETY: the initialized list holds this live array until deletion.
            if unsafe {
                UpdateProcThreadAttribute(
                    attributes.pointer(),
                    0,
                    PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                    jobs.as_mut_ptr().cast(),
                    size_of::<std::os::windows::raw::HANDLE>(),
                    null_mut(),
                    null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: these Win32 structs accept zero initialization.
            let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
            let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            // Deliberately give the renderer no terminal or inherited handles.
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.lpAttributeList = attributes.pointer();
            // SAFETY: UTF-16 buffers are terminated and the command line is
            // writable. Windows assigns the job as part of process creation,
            // before any renderer code can execute. No handles are inherited.
            let created = unsafe {
                CreateProcessW(
                    executable.as_ptr(),
                    line.as_mut_ptr(),
                    null(),
                    null(),
                    0,
                    CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
                    null(),
                    null(),
                    &startup.StartupInfo,
                    &mut process,
                )
            };
            if created == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful CreateProcessW returns two owned handles.
            let handle = unsafe { OwnedHandle::from_raw_handle(process.hProcess) };
            let thread = unsafe { OwnedHandle::from_raw_handle(process.hThread) };
            drop(thread);
            Ok(Self {
                job: Some(job),
                process: handle,
            })
        }

        pub fn has_exited(&self) -> io::Result<bool> {
            // SAFETY: this is a live process handle and a nonblocking wait.
            match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => Ok(true),
                WAIT_TIMEOUT => Ok(false),
                _ => Err(io::Error::last_os_error()),
            }
        }

        pub fn close(&mut self) -> io::Result<()> {
            if let Some(job) = self.job.take() {
                // Also terminates renderer descendants, even if the root exited.
                drop(job);
                // SAFETY: the process handle is still owned while waiting.
                if unsafe { WaitForSingleObject(self.process.as_raw_handle(), 5000) } != WAIT_OBJECT_0 {
                    return Err(io::Error::from(io::ErrorKind::TimedOut));
                }
            }
            Ok(())
        }
    }

    impl Drop for OwnedProcess {
        fn drop(&mut self) {
            let _ = self.close();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::{Command, Stdio};
        use std::time::Duration;
        use windows_sys::Win32::System::Threading::{
            GetProcessId, OpenProcess, PROCESS_SYNCHRONIZE,
        };

        fn probe_args(value: OsString) -> Vec<OsString> {
            // libtest names omit the crate name from module_path!(). --skip
            // carries a private marker without adding a custom CLI option.
            let name = format!(
                "{}::subprocess_probe",
                module_path!().split_once("::").unwrap().1,
            );
            let mut marker = OsString::from("leagueaccounts-job-probe=");
            marker.push(value);
            vec!["--exact".into(), name.into(), "--skip".into(), marker]
        }

        #[test]
        fn subprocess_probe() {
            let marker = std::env::args().find_map(|arg| {
                arg.strip_prefix("leagueaccounts-job-probe=").map(str::to_owned)
            });
            let Some(marker) = marker else { return };
            if marker == "sleep" {
                std::thread::sleep(Duration::from_secs(30));
                return;
            }
            let child = OwnedProcess::spawn(
                &std::env::current_exe().unwrap(),
                &probe_args("sleep".into()),
            )
            .unwrap();
            // SAFETY: the child process handle is owned and valid.
            let pid = unsafe { GetProcessId(child.process.as_raw_handle()) };
            std::fs::write(marker, pid.to_string()).unwrap();
            // Deliberately bypass every Rust destructor: Windows must close
            // the owner's job handle and kill its child anyway.
            std::mem::forget(child);
            std::process::exit(0);
        }

        #[test]
        fn dropping_owner_terminates_the_native_child() {
            let child = OwnedProcess::spawn(
                &std::env::current_exe().unwrap(),
                &probe_args("sleep".into()),
            )
            .unwrap();
            let handle = child.process.try_clone().unwrap();
            assert!(!child.has_exited().unwrap());
            drop(child);
            // SAFETY: the cloned process handle outlives the owner.
            assert_eq!(
                unsafe { WaitForSingleObject(handle.as_raw_handle(), 5000) },
                WAIT_OBJECT_0,
            );
        }

        #[test]
        fn abrupt_owner_exit_terminates_the_native_child_without_drop() {
            let directory = tempfile::tempdir().unwrap();
            let marker = directory.path().join("child pid.txt");
            let status = Command::new(std::env::current_exe().unwrap())
                .args(probe_args(marker.as_os_str().to_owned()))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
            let pid: u32 = std::fs::read_to_string(marker).unwrap().parse().unwrap();
            // SAFETY: a read-only wait handle; a terminated PID may already be gone.
            let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            if raw.is_null() {
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(87));
            } else {
                // SAFETY: OpenProcess returned a new owned handle.
                let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
                assert_eq!(
                    unsafe { WaitForSingleObject(handle.as_raw_handle(), 5000) },
                    WAIT_OBJECT_0,
                );
            }
        }

        #[test]
        fn command_line_preserves_spaces_quotes_and_trailing_backslashes() {
            let line = command_line(
                Path::new(r"C:\Riot Games\Riot Client.exe"),
                &["".into(), "a\"b".into(), "C:\\space path\\".into()],
            )
            .unwrap();
            let text = String::from_utf16(&line[..line.len() - 1]).unwrap();
            assert_eq!(text, "\"C:\\Riot Games\\Riot Client.exe\" \"\" \"a\\\"b\" \"C:\\space path\\\\\"");
            assert!(command_line(Path::new("client.exe"), &["bad\0arg".into()]).is_err());
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::{ffi::OsString, io, path::Path};

    pub struct OwnedProcess;

    impl OwnedProcess {
        pub fn spawn(_: &Path, _: &[OsString]) -> io::Result<Self> {
            // Never enable a debugger without native process-lifetime ownership.
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }

        pub fn has_exited(&self) -> io::Result<bool> {
            Ok(true)
        }

        pub fn close(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}

pub(super) use platform::OwnedProcess;
