//! Serialize login admission with shutdown, and keep the event loop alive
//! until the worker has dropped its renderer and restored the normal client.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use tauri::{plugin::TauriPlugin, AppHandle, RunEvent, WindowEvent, Wry};

#[derive(Default)]
struct State {
    running: bool,
    exiting: bool,
}

#[derive(Default)]
pub struct LoginControl {
    state: Mutex<State>,
    idle: Condvar,
    pub cancel: AtomicBool,
}

pub struct LoginGuard(Arc<LoginControl>);

#[derive(Debug, PartialEq, Eq)]
enum ExitAction {
    Ready,
    Wait,
    Waiting,
}

impl LoginControl {
    pub fn try_start(self: &Arc<Self>) -> Option<LoginGuard> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.running || state.exiting {
            return None;
        }
        state.running = true;
        // Reset under the same lock as exit admission: a queued login cannot
        // erase a cancellation requested by the window-close handler.
        self.cancel.store(false, Ordering::SeqCst);
        Some(LoginGuard(Arc::clone(self)))
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    fn request_exit(&self) -> ExitAction {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let first_request = !state.exiting;
        state.exiting = true;
        self.request_cancel();
        if !state.running {
            ExitAction::Ready
        } else if first_request {
            ExitAction::Wait
        } else {
            ExitAction::Waiting
        }
    }

    fn wait_idle(&self) {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let _idle = self
            .idle
            .wait_while(state, |state| state.running)
            .unwrap_or_else(|error| error.into_inner());
    }
}

impl Drop for LoginGuard {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|error| error.into_inner());
        state.running = false;
        self.0.idle.notify_all();
    }
}

fn defer_exit(app: &AppHandle, control: &Arc<LoginControl>, code: i32) -> bool {
    match control.request_exit() {
        ExitAction::Ready => false,
        ExitAction::Waiting => true,
        ExitAction::Wait => {
            let app = app.clone();
            let control = Arc::clone(control);
            std::thread::spawn(move || {
                // Do not block Tauri's event loop while native cleanup runs.
                control.wait_idle();
                app.exit(code);
            });
            true
        }
    }
}

pub fn plugin(control: Arc<LoginControl>) -> TauriPlugin<Wry> {
    tauri::plugin::Builder::new("login-cleanup")
        .on_event(move |app, event| match event {
            RunEvent::WindowEvent {
                label,
                event: WindowEvent::CloseRequested { api, .. },
                ..
            } if label == "main" && defer_exit(app, &control, 0) => {
                api.prevent_close();
            }
            RunEvent::ExitRequested { api, code, .. }
                if defer_exit(app, &control, code.unwrap_or(0)) =>
            {
                api.prevent_exit();
            }
            _ => {}
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn logins_are_exclusive_and_a_new_attempt_resets_cancellation() {
        let control = Arc::new(LoginControl::default());
        let guard = control.try_start().unwrap();
        assert!(control.try_start().is_none());
        control.request_cancel();
        assert!(control.cancel.load(Ordering::SeqCst));
        drop(guard);
        let _guard = control.try_start().unwrap();
        assert!(!control.cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn idle_exit_closes_admission_before_a_worker_can_start() {
        let control = Arc::new(LoginControl::default());
        assert_eq!(control.request_exit(), ExitAction::Ready);
        assert!(control.try_start().is_none());
        assert!(control.cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn close_during_a_queued_or_running_login_cancels_and_waits_once() {
        let control = Arc::new(LoginControl::default());
        // This guard is acquired before spawn_blocking, not inside its worker.
        let guard = control.try_start().unwrap();
        assert_eq!(control.request_exit(), ExitAction::Wait);
        assert_eq!(control.request_exit(), ExitAction::Waiting);
        assert!(control.cancel.load(Ordering::SeqCst));
        assert!(control.try_start().is_none());
        drop(guard);
        assert_eq!(control.request_exit(), ExitAction::Ready);
        assert!(control.try_start().is_none());
    }

    #[test]
    fn exit_waiter_is_released_only_after_worker_cleanup() {
        let control = Arc::new(LoginControl::default());
        let guard = control.try_start().unwrap();
        assert_eq!(control.request_exit(), ExitAction::Wait);
        let (tx, rx) = mpsc::channel();
        let waiting = Arc::clone(&control);
        let waiter = std::thread::spawn(move || {
            waiting.wait_idle();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        drop(guard);
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn worker_unwinding_drops_cleanup_before_releasing_admission() {
        struct Cleanup(Arc<AtomicBool>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let control = Arc::new(LoginControl::default());
        let guard = control.try_start().unwrap();
        let cleaned = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cleaned);
        let result = std::panic::catch_unwind(move || {
            let _guard = guard;
            let _cleanup = Cleanup(signal);
            panic!("simulated worker failure");
        });
        assert!(result.is_err());
        control.wait_idle();
        assert!(cleaned.load(Ordering::SeqCst));
        assert!(control.try_start().is_some());
    }
}
