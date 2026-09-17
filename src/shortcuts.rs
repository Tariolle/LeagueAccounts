//! Account-table actions and the native Windows auto-type accelerator.

use eframe::egui::{Context, Event, Key, Modifiers};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TableActions {
    pub copy: bool,
    pub delete: bool,
}

/// Run after the widgets, so text editors get their normal copy/delete events.
/// The caller must establish that a visible account-table cell owns focus.
pub fn take_table_actions(ctx: &Context, table_has_focus: bool) -> TableActions {
    if !table_has_focus {
        return TableActions::default();
    }
    ctx.input_mut(|input| {
        let mut copy = false;
        // egui-winit emits Copy, not a pressed Key::C, for Ctrl+C.
        input.events.retain(|event| {
            if matches!(event, Event::Copy) {
                copy = true;
                false
            } else {
                true
            }
        });
        TableActions {
            copy,
            delete: input.consume_key(Modifiers::NONE, Key::Delete),
        }
    })
}

#[cfg(any(windows, test))]
#[derive(Debug, Default, PartialEq, Eq)]
struct ChordAction {
    consume: bool,
    trigger: bool,
}

#[cfg(any(windows, test))]
#[derive(Default)]
struct AutoTypeChord {
    captured: bool,
}

#[cfg(any(windows, test))]
impl AutoTypeChord {
    fn on_v_key(&mut self, pressed: bool, repeat: bool, modifiers: Modifiers) -> ChordAction {
        if !pressed {
            return ChordAction {
                consume: std::mem::take(&mut self.captured),
                trigger: false,
            };
        }
        if self.captured && repeat {
            return ChordAction {
                consume: true,
                trigger: false,
            };
        }
        // A fresh key-down also rearms the latch if key-up went to another
        // window after Alt+Tab. Queued repeats remain consumed until then.
        self.captured = false;
        if !repeat && modifiers.ctrl && modifiers.shift && !modifiers.alt && !modifiers.mac_cmd {
            self.captured = true;
            return ChordAction {
                consume: true,
                trigger: true,
            };
        }
        ChordAction::default()
    }
}

#[cfg(windows)]
pub use native::NativeAutoType;

#[cfg(windows)]
mod native {
    use super::AutoTypeChord;
    use eframe::egui::{Context, Modifiers};
    use std::cell::RefCell;
    use std::io;
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK, WH_KEYBOARD,
    };

    struct HookState {
        ctx: Context,
        is_focused: Box<dyn Fn() -> bool>,
        chord: AutoTypeChord,
        requested: bool,
    }

    thread_local! {
        static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
    }

    /// A hook on this application's UI thread only, never a global keyboard hook.
    /// Intercept V before egui-winit turns Ctrl+Shift+V into Paste (or drops it
    /// entirely for an empty clipboard). The hook queues work; it does not type.
    pub struct NativeAutoType {
        handle: HHOOK,
    }

    impl NativeAutoType {
        pub fn install(ctx: Context, is_focused: impl Fn() -> bool + 'static) -> io::Result<Self> {
            if STATE.with(|state| state.borrow().is_some()) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "an auto-type hook is already installed on this thread",
                ));
            }
            // SAFETY: the callback is in this process, the module is therefore
            // null, and the hook is restricted to the current UI thread.
            let handle = unsafe {
                SetWindowsHookExW(
                    WH_KEYBOARD,
                    Some(keyboard_hook),
                    std::ptr::null_mut(),
                    GetCurrentThreadId(),
                )
            };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            STATE.with(|state| {
                *state.borrow_mut() = Some(HookState {
                    ctx,
                    is_focused: Box::new(is_focused),
                    chord: AutoTypeChord::default(),
                    requested: false,
                });
            });
            Ok(Self { handle })
        }

        pub fn take_requested(&self) -> bool {
            STATE.with(|state| {
                state
                    .borrow_mut()
                    .as_mut()
                    .is_some_and(|state| {
                        let requested = std::mem::take(&mut state.requested);
                        requested && (state.is_focused)()
                    })
            })
        }
    }

    impl Drop for NativeAutoType {
        fn drop(&mut self) {
            // SAFETY: this handle was installed by this object on this thread.
            unsafe { UnhookWindowsHookEx(self.handle) };
            STATE.with(|state| {
                let _ = state.borrow_mut().take();
            });
        }
    }

    unsafe extern "system" fn keyboard_hook(code: i32, key: WPARAM, flags: LPARAM) -> LRESULT {
        // HC_NOREMOVE is only a peek; do not trigger or change the latch until
        // Windows actually removes the key message from the thread's queue.
        if code == HC_ACTION as i32 && key == VK_V as usize {
            let consumed = STATE.with(|slot| {
                // Never panic across the Windows callback boundary on reentry.
                let Ok(mut slot) = slot.try_borrow_mut() else {
                    return false;
                };
                let Some(state) = slot.as_mut() else {
                    return false;
                };
                let focused = (state.is_focused)();
                if !focused {
                    state.requested = false;
                }
                // GetKeyState reflects the modifiers associated with this
                // queued message, unlike polling their later physical state.
                let down = |key: u16| unsafe { GetKeyState(i32::from(key)) < 0 };
                let modifiers = if focused {
                    Modifiers {
                        ctrl: down(VK_CONTROL),
                        shift: down(VK_SHIFT),
                        alt: down(VK_MENU),
                        mac_cmd: down(VK_LWIN) || down(VK_RWIN),
                        ..Modifiers::default()
                    }
                } else {
                    // Do not trigger in another window, but still consume
                    // repeats/key-up from the chord captured before Alt+Tab.
                    Modifiers::NONE
                };
                let action = state.chord.on_v_key(
                    flags & (1_isize << 31) == 0,
                    flags & (1_isize << 30) != 0,
                    modifiers,
                );
                if action.trigger {
                    state.requested = true;
                    state.ctx.request_repaint();
                }
                action.consume
            });
            if consumed {
                return 1;
            }
        }
        // SAFETY: forward untouched arguments to the remaining hook chain.
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, key, flags) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delete_event() -> Event {
        Event::Key {
            key: Key::Delete,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn table_actions_do_not_steal_text_editor_events() {
        let ctx = Context::default();
        let events = vec![Event::Copy, delete_event()];
        let input = eframe::egui::RawInput {
            events: events.clone(),
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |_| {
            assert_eq!(take_table_actions(&ctx, false), TableActions::default());
            ctx.input(|input| assert_eq!(input.events, events));
        });
    }

    #[test]
    fn focused_table_consumes_copy_event_and_delete_but_not_paste() {
        let ctx = Context::default();
        let input = eframe::egui::RawInput {
            events: vec![Event::Copy, delete_event(), Event::Paste("ordinary paste".into())],
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |_| {
            assert_eq!(
                take_table_actions(&ctx, true),
                TableActions { copy: true, delete: true }
            );
            ctx.input(|input| {
                assert_eq!(input.events, vec![Event::Paste("ordinary paste".into())]);
            });
        });
    }

    #[test]
    fn native_chord_triggers_without_a_clipboard_event() {
        let mut chord = AutoTypeChord::default();
        // No clipboard read or Paste event is needed, even when it is empty.
        assert_eq!(
            chord.on_v_key(true, false, Modifiers::CTRL | Modifiers::SHIFT),
            ChordAction { consume: true, trigger: true }
        );
    }

    #[test]
    fn native_chord_swallows_repeats_and_keyup_without_retriggering() {
        let mut chord = AutoTypeChord::default();
        let modifiers = Modifiers::CTRL | Modifiers::SHIFT;
        assert!(chord.on_v_key(true, false, modifiers).trigger);
        assert_eq!(
            chord.on_v_key(true, true, modifiers),
            ChordAction { consume: true, trigger: false }
        );
        // The modifiers may be released before V.
        assert_eq!(
            chord.on_v_key(false, false, Modifiers::NONE),
            ChordAction { consume: true, trigger: false }
        );
        assert!(chord.on_v_key(true, false, modifiers).trigger);
    }

    #[test]
    fn native_chord_leaves_normal_paste_and_unrelated_modifiers_alone() {
        for modifiers in [
            Modifiers::NONE,
            Modifiers::CTRL,
            Modifiers::SHIFT,
            Modifiers::CTRL | Modifiers::SHIFT | Modifiers::ALT,
        ] {
            let mut chord = AutoTypeChord::default();
            assert_eq!(chord.on_v_key(true, false, modifiers), ChordAction::default());
            assert_eq!(chord.on_v_key(false, false, modifiers), ChordAction::default());
        }
    }

    #[test]
    fn adding_modifiers_to_an_already_held_v_does_not_trigger() {
        let mut chord = AutoTypeChord::default();
        assert_eq!(
            chord.on_v_key(true, true, Modifiers::CTRL | Modifiers::SHIFT),
            ChordAction::default()
        );
    }
    #[test]
    fn captured_repeats_stay_consumed_after_focus_or_modifiers_change() {
        let mut chord = AutoTypeChord::default();
        assert!(chord.on_v_key(true, false, Modifiers::CTRL | Modifiers::SHIFT).trigger);
        assert_eq!(
            chord.on_v_key(true, true, Modifiers::NONE),
            ChordAction { consume: true, trigger: false }
        );
        assert_eq!(
            chord.on_v_key(false, true, Modifiers::NONE),
            ChordAction { consume: true, trigger: false }
        );
    }

    #[test]
    fn fresh_press_rearms_when_keyup_went_to_another_window() {
        let mut chord = AutoTypeChord::default();
        let modifiers = Modifiers::CTRL | Modifiers::SHIFT;
        assert!(chord.on_v_key(true, false, modifiers).trigger);
        // A missing key-up must not swallow the next ordinary paste.
        assert_eq!(chord.on_v_key(true, false, Modifiers::CTRL), ChordAction::default());
        assert!(chord.on_v_key(true, false, modifiers).trigger);
        // A second complete chord can also arrive without a local key-up.
        assert!(chord.on_v_key(true, false, modifiers).trigger);
    }

}
