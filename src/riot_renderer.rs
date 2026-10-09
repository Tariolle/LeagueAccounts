//! Own the entire Riot login attempt, including startup and renderer cleanup.
#[path = "riot_process.rs"]
mod owned_process;

use super::{
    check_cancelled, running_processes, windows, LocalApi, LoginError, LoginStep, Process, Session,
};
use crate::logging::{self, Event, Reason};
use owned_process::OwnedProcess;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

fn command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

fn path_arg(name: &str, path: &Path) -> OsString {
    let mut arg = OsString::from(name);
    arg.push(path);
    arg
}

fn start_service(path: &Path, headless: bool) -> Result<(), LoginError> {
    let mut process = command(path);
    if headless {
        process.arg("--headless");
    }
    process
        .spawn()
        .map(|_| ())
        .map_err(|_| LoginError::LaunchFailed)
}

fn wait(cancel: &AtomicBool, duration: Duration) -> Result<(), LoginError> {
    let start = Instant::now();
    while start.elapsed() < duration {
        check_cancelled(cancel)?;
        thread::sleep(Duration::from_millis(50));
    }
    check_cancelled(cancel)
}

fn client_processes(root: &Path) -> Result<Vec<Process>, LoginError> {
    let prefix = format!("{}\\", root.to_string_lossy()).to_ascii_lowercase();
    let mut processes = Vec::new();
    for process in running_processes().into_iter().filter(|p| {
        matches!(
            p.name.as_str(),
            "riotclientservices.exe" | "riot client.exe"
        )
    }) {
        let Some(path) = windows::process_path(process.pid) else {
            if running_processes().iter().any(|p| p.pid == process.pid) {
                return Err(LoginError::CloseFailed);
            }
            continue;
        };
        if path
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with(&prefix)
        {
            processes.push(process);
        }
    }
    Ok(processes)
}

fn stop_client(path: &Path, cancel: &AtomicBool) -> Result<(), LoginError> {
    let root = path.parent().ok_or(LoginError::ClientMissing)?;
    for process in client_processes(root)? {
        check_cancelled(cancel)?;
        // Exact processes under this Riot installation only; never game processes.
        let _ = command(Path::new("taskkill.exe"))
            .args(["/F", "/PID", &process.pid.to_string()])
            .status();
    }
    let start = Instant::now();
    while !client_processes(root)?.is_empty() {
        if start.elapsed() > Duration::from_secs(10) {
            return Err(LoginError::CloseFailed);
        }
        wait(cancel, Duration::from_millis(200))?;
    }
    Ok(())
}

pub(super) fn debugger_url(value: &str, port: u16) -> bool {
    port != 0
        && reqwest::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "ws"
                && url.host_str() == Some("127.0.0.1")
                && url.port() == Some(port)
                && url.username().is_empty()
                && url.password().is_none()
                && url.path().starts_with("/devtools/page/")
                && url.query().is_none()
                && url.fragment().is_none()
        })
}

struct Cdp {
    socket: WebSocket<TcpStream>,
    sequence: u64,
}

impl Cdp {
    fn connect(url: &str, port: u16) -> Result<Self, LoginError> {
        if !debugger_url(url, port) {
            return Err(LoginError::RendererFailed);
        }
        let stream =
            TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2))
                .map_err(|_| LoginError::RendererFailed)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| LoginError::RendererFailed)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| LoginError::RendererFailed)?;
        let (socket, _) =
            tungstenite::client(url, stream).map_err(|_| LoginError::RendererFailed)?;
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|_| LoginError::RendererFailed)?;
        Ok(Self {
            socket,
            sequence: 0,
        })
    }

    fn eval(&mut self, expression: &str, cancel: &AtomicBool) -> Result<Value, LoginError> {
        check_cancelled(cancel)?;
        self.sequence += 1;
        // Expressions may contain credentials. Neither requests nor errors are logged.
        self.socket
            .send(Message::Text(
                json!({"id": self.sequence, "method":"Runtime.evaluate",
            "params":{"expression":expression,"returnByValue":true,"awaitPromise":true}})
                .to_string()
                .into(),
            ))
            .map_err(|_| LoginError::RendererFailed)?;
        let start = Instant::now();
        loop {
            check_cancelled(cancel)?;
            if start.elapsed() > Duration::from_secs(15) {
                return Err(LoginError::RendererFailed);
            }
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    let response: Value =
                        serde_json::from_str(&text).map_err(|_| LoginError::RendererFailed)?;
                    if response["id"].as_u64() != Some(self.sequence) {
                        continue;
                    }
                    if response.get("error").is_some()
                        || response["result"].get("exceptionDetails").is_some()
                    {
                        return Err(LoginError::RendererFailed);
                    }
                    return Ok(response["result"]["result"]["value"].clone());
                }
                Ok(Message::Close(_)) => return Err(LoginError::RendererFailed),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                Err(_) => return Err(LoginError::RendererFailed),
            }
        }
    }
}

pub(super) struct Renderer {
    service: PathBuf,
    child: Option<OwnedProcess>,
    cdp: Option<Cdp>,
    normal: Option<(PathBuf, Vec<OsString>, String)>,
    restore_needed: bool,
    // One seam for deterministic lifecycle tests without launching Riot.
    service_start: fn(&Path, bool) -> Result<(), LoginError>,
}

impl Renderer {
    pub(super) fn new(service: &Path) -> Self {
        Self {
            service: service.to_owned(),
            child: None,
            cdp: None,
            normal: None,
            restore_needed: false,
            service_start: start_service,
        }
    }

    pub(super) fn ensure_service(&mut self, cancel: &AtomicBool) -> Result<(), LoginError> {
        check_cancelled(cancel)?;
        let root = self.service.parent().ok_or(LoginError::ClientMissing)?;
        let processes = client_processes(root)?;
        // A parseable lockfile is discovery data, not evidence of a live
        // service. Check executable ownership even when the API is starting.
        self.ensure_service_with(
            processes.iter().any(|p| p.name == "riotclientservices.exe"),
            processes.iter().any(|p| p.name == "riot client.exe"),
            cancel,
        )
    }

    fn ensure_service_with(
        &mut self,
        service_running: bool,
        renderer_running: bool,
        cancel: &AtomicBool,
    ) -> Result<(), LoginError> {
        check_cancelled(cancel)?;
        if !service_running || !renderer_running {
            // Arm before spawning so errors/cancellation during initial auth
            // restoration cannot strand a headless client.
            self.restore_needed = true;
        }
        if !service_running {
            (self.service_start)(&self.service, true)?;
        }
        Ok(())
    }

    pub(super) fn restore(&mut self) -> Result<(), LoginError> {
        if !self.restore_needed {
            return Ok(());
        }
        if let Some(cdp) = self.cdp.as_mut() {
            let _ = cdp.socket.send(Message::Text(
                json!({"id":0,"method":"Browser.close"}).to_string().into(),
            ));
        }
        if let Some(child) = self.child.as_ref() {
            let start = Instant::now();
            while !child.has_exited().unwrap_or(true)
                && start.elapsed() < Duration::from_secs(3)
            {
                thread::sleep(Duration::from_millis(100));
            }
        }
        self.cdp = None;
        if let Some(mut child) = self.child.take() {
            child.close().map_err(|_| LoginError::RendererFailed)?;
        }
        let restored = self.normal.as_ref().is_some_and(|(path, args, endpoint)| {
            LocalApi::read().is_some_and(|api| {
                &api.base == endpoint && api.session() != Session::Unknown(Reason::AuthUnavailable)
            }) && command(path).args(args).spawn().is_ok()
        });
        if !restored {
            (self.service_start)(&self.service, false)?;
        }
        self.restore_needed = false;
        Ok(())
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // Covers the initial WaitAuth loop as well as renderer setup, MFA,
        // cancellation and unwinding. Successful paths restore before launch.
        if self.restore().is_err() {
            logging::record(Event::LoginFailed, Reason::RendererFailed);
        }
    }
}

pub(super) fn auth_result(value: &str) -> Result<(), LoginError> {
    match value {
        "pending" | "success" | "verification" => Ok(()),
        "captcha_rejected" => Err(LoginError::CaptchaRejected),
        "rate_limited" => Err(LoginError::RateLimited),
        "rejected" => Err(LoginError::AuthRejected),
        _ => Err(LoginError::RendererFailed),
    }
}

pub(super) fn sign_in(
    renderer: &mut Renderer,
    username: &str,
    password: &str,
    progress: &mut dyn FnMut(LoginStep),
    cancel: &AtomicBool,
) -> Result<(), LoginError> {
    if password.is_empty() {
        return Err(LoginError::AuthRejected);
    }
    check_cancelled(cancel)?;
    let service = renderer.service.clone();
    renderer.restore_needed = true;
    progress(LoginStep::Connect);
    stop_client(&service, cancel)?;
    (renderer.service_start)(&service, true)?;
    let start = Instant::now();
    let api = loop {
        check_cancelled(cancel)?;
        if start.elapsed() > Duration::from_secs(45) {
            return Err(LoginError::Timeout);
        }
        if let Some(api) = LocalApi::read() {
            match api.session() {
                Session::SignedOut => break api,
                Session::SignedIn => {
                    if api
                        .username()
                        .is_some_and(|name| name.eq_ignore_ascii_case(username))
                    {
                        return Ok(());
                    }
                    return Err(LoginError::AccountMismatch);
                }
                _ => {}
            }
        }
        wait(cancel, Duration::from_millis(300))?;
    };
    let root = service.parent().ok_or(LoginError::ClientMissing)?;
    let ux = root.join("RiotClientElectron").join("Riot Client.exe");
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or(LoginError::RendererFailed)?;
    let data = local.join("Riot Games").join("Riot Client");
    let lock = std::fs::read_to_string(data.join("Config").join("lockfile"))
        .map_err(|_| LoginError::RendererFailed)?;
    let pid = lock
        .split(':')
        .nth(1)
        .and_then(|p| p.parse::<u32>().ok())
        .ok_or(LoginError::RendererFailed)?;
    let machine = api
        .get("/riotclient/machine-id")
        .filter(|(code, _)| *code == 200)
        .and_then(|(_, v)| v.as_str().map(str::to_owned))
        .ok_or(LoginError::RendererFailed)?;
    let session = api
        .get("/product-session/v1/host-session/id")
        .filter(|(code, _)| *code == 200)
        .and_then(|(_, v)| v.as_str().map(str::to_owned))
        .ok_or(LoginError::RendererFailed)?;
    let args: Vec<OsString> = vec![
        format!(
            "--app-port={}",
            api.base
                .rsplit(':')
                .next()
                .ok_or(LoginError::RendererFailed)?
        )
        .into(),
        format!("--remoting-auth-token={}", api.password).into(),
        format!("--app-pid={pid}").into(),
        format!("--machine-id={machine}").into(),
        format!("--session-id={session}").into(),
        path_arg("--app-root=", root),
        path_arg("--user-data-root=", &data),
        path_arg("--log-dir=", &data.join("Logs").join("Riot Client UX Logs")),
        "--crashpad-environment=prod".into(),
        "--enable-hardware-acceleration".into(),
    ];
    renderer.normal = Some((ux.clone(), args.clone(), api.base.clone()));
    // Port 0 in Chromium enables its automation mode; pass a concrete port.
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|_| LoginError::RendererFailed)?;
    let port = listener
        .local_addr()
        .map_err(|_| LoginError::RendererFailed)?
        .port();
    drop(listener);
    check_cancelled(cancel)?;
    let mut debug_args = args;
    debug_args.extend([
        OsString::from("--allow-chrome-dev-tools"),
        OsString::from("--remote-debugging-address=127.0.0.1"),
        OsString::from(format!("--remote-debugging-port={port}")),
    ]);
    renderer.child = Some(
        OwnedProcess::spawn(&ux, &debug_args).map_err(|_| LoginError::RendererFailed)?,
    );
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| LoginError::RendererFailed)?;
    let start = Instant::now();
    let endpoint = loop {
        check_cancelled(cancel)?;
        if start.elapsed() > Duration::from_secs(30) {
            return Err(LoginError::RendererFailed);
        }
        if renderer
            .child
            .as_ref()
            .is_some_and(|child| child.has_exited().unwrap_or(true))
        {
            return Err(LoginError::RendererFailed);
        }
        let targets = http
            .get(format!("http://127.0.0.1:{port}/json/list"))
            .send()
            .ok()
            .and_then(|r| r.text().ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        if let Some(target) = targets
            .as_ref()
            .and_then(Value::as_array)
            .and_then(|targets| {
                targets
                    .iter()
                    .find(|t| t["type"] == "page" && t["title"] == "Riot Client")
            })
        {
            if let Some(url) = target["webSocketDebuggerUrl"]
                .as_str()
                .filter(|url| debugger_url(url, port))
            {
                break url.to_owned();
            }
        }
        wait(cancel, Duration::from_millis(200))?;
    };
    renderer.cdp = Some(Cdp::connect(&endpoint, port)?);
    let cdp = renderer.cdp.as_mut().ok_or(LoginError::RendererFailed)?;
    cdp.eval(include_str!("riot_renderer.js"), cancel)?;
    progress(LoginStep::FindWindow);
    let start = Instant::now();
    while cdp.eval("globalThis.__leagueAccountsLogin.ready()", cancel)? != true {
        if start.elapsed() > Duration::from_secs(30) {
            return Err(LoginError::RendererFailed);
        }
        wait(cancel, Duration::from_millis(200))?;
    }
    // Allow the native authentication initialization to settle before filling.
    wait(cancel, Duration::from_millis(1500))?;
    let current = LocalApi::read().ok_or(LoginError::RendererFailed)?;
    if current.base != api.base || current.session() != Session::SignedOut {
        return Err(LoginError::AccountMismatch);
    }
    progress(LoginStep::Type);
    let mut expression = format!(
        "globalThis.__leagueAccountsLogin.fill(...{})",
        json!([username, password])
    );
    let filled = cdp.eval(&expression, cancel);
    expression.clear();
    if filled? != true {
        return Err(LoginError::RendererFailed);
    }
    if cdp.eval("globalThis.__leagueAccountsLogin.submit()", cancel)? != true {
        return Err(LoginError::RendererFailed);
    }
    progress(LoginStep::Confirm);
    let start = Instant::now();
    let mut verification = false;
    loop {
        check_cancelled(cancel)?;
        if start.elapsed() > Duration::from_secs(300) {
            return Err(LoginError::Timeout);
        }
        let current = LocalApi::read().ok_or(LoginError::RendererFailed)?;
        if current.base != api.base {
            return Err(LoginError::RendererFailed);
        }
        if current.session() == Session::SignedIn {
            if let Some(name) = current.username() {
                return if name.eq_ignore_ascii_case(username) {
                    Ok(())
                } else {
                    Err(LoginError::AccountMismatch)
                };
            }
        }
        let state = cdp.eval("globalThis.__leagueAccountsLogin.status()", cancel)?;
        let state = state.as_str().ok_or(LoginError::RendererFailed)?;
        auth_result(state)?;
        let multifactor = !verification
            && current
                .get("/rso-authenticator/v1/authentication")
                .is_some_and(|(status, body)| status == 200 && body["type"] == "multifactor");
        if (state == "verification" || multifactor) && !verification {
            cdp.eval(
                "globalThis.__leagueAccountsLogin.showVerification()",
                cancel,
            )?;
            progress(LoginStep::Verification);
            verification = true;
        }
        wait(cancel, Duration::from_millis(300))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        static STARTS: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    }

    fn fake_start(_: &Path, headless: bool) -> Result<(), LoginError> {
        STARTS.with(|starts| starts.borrow_mut().push(headless));
        Ok(())
    }

    fn fixture() -> Renderer {
        STARTS.with(|starts| starts.borrow_mut().clear());
        let mut renderer = Renderer::new(Path::new("dummy/RiotClientServices.exe"));
        renderer.service_start = fake_start;
        renderer
    }

    fn starts() -> Vec<bool> {
        STARTS.with(|starts| starts.borrow().clone())
    }

    #[test]
    fn stale_lockfile_does_not_suppress_starting_a_missing_service() {
        let stale = LocalApi::from_lockfile("riot:999999:12345:dummy:https");
        assert!(stale.is_some());
        let mut renderer = fixture();
        renderer.ensure_service_with(false, false, &AtomicBool::new(false)).unwrap();
        assert_eq!(starts(), vec![true]);
        drop(renderer);
        assert_eq!(starts(), vec![true, false]);
    }

    #[test]
    fn restored_account_without_game_launch_gets_a_normal_client_once() {
        let mut renderer = fixture();
        renderer.ensure_service_with(false, false, &AtomicBool::new(false)).unwrap();
        // The already-signed-in path finishes before any debug renderer exists.
        renderer.restore().unwrap();
        renderer.restore().unwrap();
        drop(renderer);
        assert_eq!(starts(), vec![true, false]);
    }

    #[test]
    fn initial_wait_errors_restore_even_before_renderer_setup() {
        for error in [LoginError::Cancelled, LoginError::Timeout, LoginError::AuthRejected] {
            let result = (|| -> Result<(), LoginError> {
                let mut renderer = fixture();
                renderer.ensure_service_with(false, false, &AtomicBool::new(false))?;
                Err(error)
            })();
            assert_eq!(result, Err(error));
            assert_eq!(starts(), vec![true, false]);
        }
    }

    #[test]
    fn startup_unwinding_restores_the_normal_client() {
        let result = std::panic::catch_unwind(|| {
            let mut renderer = fixture();
            renderer.ensure_service_with(false, false, &AtomicBool::new(false)).unwrap();
            panic!("simulated startup failure");
        });
        assert!(result.is_err());
        assert_eq!(starts(), vec![true, false]);
    }

    #[test]
    fn existing_normal_client_is_not_restarted() {
        let mut renderer = fixture();
        renderer.ensure_service_with(true, true, &AtomicBool::new(false)).unwrap();
        drop(renderer);
        assert!(starts().is_empty());
    }

    #[test]
    fn existing_headless_service_gets_a_renderer_without_restarting_auth() {
        let mut renderer = fixture();
        renderer.ensure_service_with(true, false, &AtomicBool::new(false)).unwrap();
        drop(renderer);
        assert_eq!(starts(), vec![false]);
    }

    #[test]
    fn cancellation_before_startup_does_not_open_a_client() {
        let mut renderer = fixture();
        assert_eq!(
            renderer.ensure_service_with(false, false, &AtomicBool::new(true)),
            Err(LoginError::Cancelled),
        );
        drop(renderer);
        assert!(starts().is_empty());
    }

    #[test]
    fn failed_headless_spawn_still_attempts_normal_restoration() {
        fn fail_headless(path: &Path, headless: bool) -> Result<(), LoginError> {
            fake_start(path, headless)?;
            if headless { Err(LoginError::LaunchFailed) } else { Ok(()) }
        }
        let mut renderer = fixture();
        renderer.service_start = fail_headless;
        assert_eq!(
            renderer.ensure_service_with(false, false, &AtomicBool::new(false)),
            Err(LoginError::LaunchFailed),
        );
        drop(renderer);
        assert_eq!(starts(), vec![true, false]);
    }

    #[test]
    fn debugger_endpoint_is_restricted_to_allocated_loopback_port() {
        assert!(debugger_url(
            "ws://127.0.0.1:12345/devtools/page/abc",
            12345
        ));
        for url in [
            "ws://example.com:12345/devtools/page/abc",
            "ws://127.0.0.1:23456/devtools/page/abc",
            "ws://user:secret@127.0.0.1:12345/devtools/page/abc",
            "ws://127.0.0.1:12345/devtools/browser/abc",
            "ws://127.0.0.1:12345/devtools/page/abc?token=secret",
            "http://127.0.0.1:12345/devtools/page/abc",
        ] {
            assert!(!debugger_url(url, 12345));
        }
        assert!(!debugger_url("ws://127.0.0.1:0/devtools/page/abc", 0));
    }

    #[test]
    fn rejected_or_unrecognized_authentication_stops_login() {
        for state in ["pending", "verification", "success"] {
            assert_eq!(auth_result(state), Ok(()));
        }
        for (state, error) in [
            ("rejected", LoginError::AuthRejected),
            ("captcha_rejected", LoginError::CaptchaRejected),
            ("rate_limited", LoginError::RateLimited),
            ("changed", LoginError::RendererFailed),
            ("unexpected", LoginError::RendererFailed),
        ] {
            assert_eq!(auth_result(state), Err(error));
        }
    }

    #[test]
    fn cdp_ignores_events_and_rejects_javascript_exceptions() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let mut socket = tungstenite::accept(listener.accept().unwrap().0).unwrap();
            for index in 1..=2 {
                let request: Value =
                    serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(request["method"], "Runtime.evaluate");
                socket
                    .send(Message::Text(
                        json!({"method":"Runtime.consoleAPICalled"})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
                let result = if index == 1 {
                    json!({"result":{"value":true}})
                } else {
                    json!({"exceptionDetails":{"text":"dummy-sensitive-error"}})
                };
                socket
                    .send(Message::Text(
                        json!({"id":request["id"], "result":result})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
            }
        });
        let mut cdp =
            Cdp::connect(&format!("ws://127.0.0.1:{port}/devtools/page/test"), port).unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(cdp.eval("true", &cancel), Ok(json!(true)));
        assert_eq!(
            cdp.eval("throw new Error()", &cancel),
            Err(LoginError::RendererFailed)
        );
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(cdp.eval("true", &cancel), Err(LoginError::Cancelled));
        server.join().unwrap();
    }
}
