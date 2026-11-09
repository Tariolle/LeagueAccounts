//! Serialize login and refresh admission with shutdown, keeping the event
//! loop alive until native cleanup and admitted storage work have finished.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use tauri::{plugin::TauriPlugin, AppHandle, RunEvent, WindowEvent, Wry};

#[derive(Default)]
struct State {
    running: bool,
    exiting: bool,
    refreshes: usize,
    full_refresh: bool,
}

#[derive(Default)]
pub struct LoginControl {
    state: Mutex<State>,
    idle: Condvar,
    pub cancel: AtomicBool,
}

pub struct LoginGuard(Arc<LoginControl>);

pub struct RefreshGuard {
    control: Arc<LoginControl>,
    exclusive: bool,
}

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

    pub fn is_running(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).running
    }

    pub fn try_start_refresh(self: &Arc<Self>, exclusive: bool) -> Option<RefreshGuard> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.exiting || (exclusive && state.full_refresh) {
            return None;
        }
        state.refreshes += 1;
        state.full_refresh |= exclusive;
        Some(RefreshGuard { control: Arc::clone(self), exclusive })
    }

    pub fn refresh_running(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).full_refresh
    }

    pub fn has_refreshes(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).refreshes != 0
    }

    pub fn is_exiting(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).exiting
    }

    /// Atomically close all admission only after login and refreshes are idle.
    pub fn close_if_idle(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.running || state.refreshes != 0 {
            return false;
        }
        state.exiting = true;
        true
    }

    fn request_exit(&self) -> ExitAction {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let first_request = !state.exiting;
        state.exiting = true;
        self.request_cancel();
        if !state.running && state.refreshes == 0 {
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
            .wait_while(state, |state| state.running || state.refreshes != 0)
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

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        let mut state = self.control.state.lock().unwrap_or_else(|error| error.into_inner());
        state.refreshes -= 1;
        if self.exclusive {
            state.full_refresh = false;
        }
        self.control.idle.notify_all();
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
    fn update_restart_waits_for_a_running_login_and_then_blocks_new_ones() {
        let control = Arc::new(LoginControl::default());
        let guard = control.try_start().unwrap();
        assert!(control.is_running());
        assert!(!control.close_if_idle());
        assert!(!control.cancel.load(Ordering::SeqCst));
        drop(guard);
        assert!(control.close_if_idle());
        assert!(control.try_start().is_none());
        // The exit that follows proceeds at once.
        assert_eq!(control.request_exit(), ExitAction::Ready);
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

    #[test]
    fn refresh_admission_blocks_restart_and_is_released_on_failure() {
        let control = Arc::new(LoginControl::default());
        let partial = control.try_start_refresh(false).unwrap();
        let full = control.try_start_refresh(true).unwrap();
        assert!(control.try_start_refresh(true).is_none());
        assert!(!control.close_if_idle());
        drop(full);
        assert!(!control.refresh_running());
        assert!(!control.close_if_idle());
        drop(partial);
        assert!(control.close_if_idle());
        assert!(control.try_start_refresh(false).is_none());
        assert!(control.try_start_refresh(true).is_none());
    }

    #[test]
    fn exit_waits_for_all_refreshes_without_readmitting_work() {
        let control = Arc::new(LoginControl::default());
        let first = control.try_start_refresh(false).unwrap();
        let second = control.try_start_refresh(false).unwrap();
        assert_eq!(control.request_exit(), ExitAction::Wait);
        assert!(control.try_start_refresh(false).is_none());
        drop(first);
        assert_eq!(control.request_exit(), ExitAction::Waiting);
        drop(second);
        assert_eq!(control.request_exit(), ExitAction::Ready);
    }
}
