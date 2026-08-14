// The pill's window hook — one wndproc subclass for every message winit
// doesn't surface.
//
// winit hands us `WindowEvent`s, and only the ones it models. Several settled
// pill behaviours need messages that never make it that far:
//
//   WM_MOUSEACTIVATE     — WS_EX_NOACTIVATE has a documented hover-to-activate
//                          hole; answering MA_NOACTIVATE closes it, so clicking
//                          the pill can never move focus off the window a
//                          dictation is about to be pasted into (#20).
//   WM_DISPLAYCHANGE     — the one event that overrides the home monitor's
//                          idle-only derivation; the alternative is a pill
//                          positioned into a coordinate space that no longer
//                          exists (#22).
//   WM_DPICHANGED        — the layered surface is pushed at device resolution,
//                          so a DPI change means re-render, not just re-place.
//   WM_POWERBROADCAST    — GUID_SESSION_DISPLAY_STATUS: stop rendering while
//                          the display is off (#21, R9).
//   WM_WTSSESSION_CHANGE — lock/unlock: same, while the session is locked
//                          (#21, R13).
//
// One subclass, not four. Four subclasses is four chances to clobber
// GWLP_WNDPROC, and unwinding a chain of them in the wrong order silently drops
// whichever one is not on top. Everything this hook does not claim is forwarded
// to the original wndproc unchanged.
//
// The home monitor consumes the two display messages (#43): both re-derive it,
// and WM_DISPLAYCHANGE breaks its idle-only latch to do so. The proximity
// ladder (#49) consumes the other two: display-off and session-lock are what
// take the loop to `ControlFlow::Wait` with no timer armed at all.
//
// The hook lives and dies with the pill window, so a session-only pill surfaces
// nothing between dictations. Residency (#42) makes the window permanent, which
// is what lets the ladder hear display-off and lock while nothing is happening
// — the state it most needs to know about.

/// A window message the app loop needs to know about, surfaced from the pill's
/// wndproc. Deliberately plain data: no HWND, no LPARAM, nothing the receiver
/// would have to be on the UI thread to read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HookEvent {
    /// Resolution, monitor topology, or colour depth changed. The pill's
    /// coordinate space may no longer exist.
    DisplayChanged,
    /// The pill's window moved to a monitor with a different scale factor, or
    /// its monitor's scale factor changed. `dpi` is the new one (96 = 100%).
    DpiChanged { dpi: u32 },
    /// The session's display went off or came back. A dimmed display counts as
    /// on — it is still being looked at.
    ///
    /// One of these always arrives immediately after the hook is installed:
    /// registering for a power setting delivers its current value straight
    /// away. So the first is a statement of state, not a change — which is what
    /// a consumer wants anyway, since it has no other way to ask.
    DisplayPower { on: bool },
    /// The session was locked or unlocked.
    SessionLock { locked: bool },
    /// The session was attached to a terminal again — an RDP client
    /// reconnecting, or the session being handed back to the physical console.
    ///
    /// Its own event rather than an unlock: the session was never locked, and
    /// what changed is which terminal is drawing it. A layered surface does not
    /// reliably survive that, so the pill re-pushes.
    SessionReconnected,
}

/// The hook itself is Win32 all the way down — off Windows the pill window
/// simply doesn't carry one, and no event is ever surfaced.
#[cfg(windows)]
pub use win::PillHook;

#[cfg(windows)]
mod win {
    use super::HookEvent;
    use crossbeam_channel::Sender;
    use windows::core::GUID;
    use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, TRUE, WPARAM};
    use windows::Win32::System::Power::{
        RegisterPowerSettingNotification, UnregisterPowerSettingNotification, HPOWERNOTIFY,
        POWERBROADCAST_SETTING,
    };
    use windows::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows::Win32::System::SystemServices::GUID_SESSION_DISPLAY_STATUS;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, GetPropW, GetWindowLongPtrW, RemovePropW, SetPropW,
        SetWindowLongPtrW, DEVICE_NOTIFY_WINDOW_HANDLE, GWLP_WNDPROC, MA_NOACTIVATE,
        PBT_POWERSETTINGCHANGE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_MOUSEACTIVATE, WM_NCDESTROY,
        WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WNDPROC, WTS_CONSOLE_CONNECT, WTS_REMOTE_CONNECT,
        WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };

    /// The subclass is the classic `GWLP_WNDPROC` swap, not comctl32's
    /// `SetWindowSubclass`. That API is exported by name only from comctl32
    /// version 6, which is a side-by-side assembly: importing it makes the whole
    /// process fail to load with STATUS_ENTRYPOINT_NOT_FOUND unless a manifest
    /// activates v6. The pill is the only thing that ever subclasses this
    /// window, so the ordering guarantees comctl32 buys are not needed.
    ///
    /// The window property carrying our state — the previous wndproc and the
    /// sender — so nothing lives in a global and the state dies with the window.
    const HOOK_PROP: windows::core::PCWSTR = windows::core::w!("DraftPillHook");

    /// What the hook needs on every message, hung off the window itself.
    struct HookState {
        /// The wndproc we displaced. Every message we don't claim goes here.
        prev: WNDPROC,
        tx: Sender<HookEvent>,
    }

    /// What the hook does with a message.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Reaction {
        /// Not ours. Forward to the original wndproc unchanged.
        Pass,
        /// Ours to answer: return `result` without forwarding.
        Answer {
            result: LRESULT,
            event: Option<HookEvent>,
        },
        /// Ours to *watch*: surface it, then forward — winit acts on these too,
        /// and swallowing them would break its own scale-factor handling.
        Watch(HookEvent),
    }

    /// The whole hook, as a pure function of a message. Kept apart from the
    /// wndproc so the decisions are assertable without a window: the wndproc
    /// below only dereferences pointers and performs what this returns.
    ///
    /// `power` is WM_POWERBROADCAST's setting payload, already dereferenced.
    fn classify(msg: u32, wparam: usize, power: Option<(GUID, u32)>) -> Reaction {
        match msg {
            // The click never activates and never moves focus. Not forwarded:
            // DefWindowProc's answer is MA_ACTIVATE, which is the hole.
            WM_MOUSEACTIVATE => Reaction::Answer {
                result: LRESULT(MA_NOACTIVATE as isize),
                event: None,
            },
            WM_DISPLAYCHANGE => Reaction::Watch(HookEvent::DisplayChanged),
            // The new DPI is the low word of wParam (both words carry it).
            WM_DPICHANGED => Reaction::Watch(HookEvent::DpiChanged {
                dpi: (wparam & 0xffff) as u32,
            }),
            // Every power setting we registered for arrives on this message, so
            // the GUID is the discriminator — anything else is not ours.
            // Answered rather than forwarded: "An application should return
            // TRUE if it processes this message."
            WM_POWERBROADCAST if wparam as u32 == PBT_POWERSETTINGCHANGE => match power {
                Some((setting, data)) if setting == GUID_SESSION_DISPLAY_STATUS => {
                    Reaction::Answer {
                        // 0 off, 1 on, 2 dimmed. Dimmed is still being looked at.
                        result: LRESULT(TRUE.0 as isize),
                        event: Some(HookEvent::DisplayPower { on: data != 0 }),
                    }
                }
                _ => Reaction::Pass,
            },
            // Forwarded, unlike the power broadcast: this message's return value
            // is documented as ignored, so there is nothing to claim.
            WM_WTSSESSION_CHANGE => match wparam as u32 {
                WTS_SESSION_LOCK => Reaction::Watch(HookEvent::SessionLock { locked: true }),
                WTS_SESSION_UNLOCK => Reaction::Watch(HookEvent::SessionLock { locked: false }),
                // The session arriving at a terminal: an RDP client
                // reconnecting, or it being handed back to the physical
                // console. Both rebuild what is drawing the desktop.
                WTS_REMOTE_CONNECT | WTS_CONSOLE_CONNECT => {
                    Reaction::Watch(HookEvent::SessionReconnected)
                }
                // The matching disconnects, logon and logoff: real messages,
                // but nothing is on screen to repair when the session *leaves*
                // a terminal — the reconnect is where the work is.
                _ => Reaction::Pass,
            },
            _ => Reaction::Pass,
        }
    }

    /// The subclass procedure. Runs on the winit event-loop thread, inside
    /// `DispatchMessage`, so the sender it posts to is drained by the very loop
    /// that is calling it.
    unsafe extern "system" fn pill_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // No state means we are being called after the hook came off — nothing
        // to forward to but the default.
        let state = hook_state(hwnd);
        if state.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        // Copied out, not borrowed: `uninstall` below frees what `state` points
        // at, and nothing may still be reading it when it does.
        let prev = (*state).prev;

        // Safety net: [`PillHook::drop`] takes the hook off while the window is
        // still alive, so this normally never fires. It matters if the HWND is
        // ever destroyed out from under the hook — leaving a wndproc pointing
        // into a freed state is how this turns into a crash.
        if msg == WM_NCDESTROY {
            let result = call_prev(prev, hwnd, msg, wparam, lparam);
            uninstall(hwnd);
            return result;
        }

        let power = if msg == WM_POWERBROADCAST && wparam.0 as u32 == PBT_POWERSETTINGCHANGE {
            read_power_setting(lparam)
        } else {
            None
        };

        // The app loop's receiver is owned by `App` and outlives every window,
        // so a failed send only means we are shutting down.
        let surface = |event| {
            let _ = (*state).tx.send(event);
        };
        match classify(msg, wparam.0, power) {
            Reaction::Pass => call_prev(prev, hwnd, msg, wparam, lparam),
            Reaction::Watch(event) => {
                surface(event);
                call_prev(prev, hwnd, msg, wparam, lparam)
            }
            Reaction::Answer { result, event } => {
                if let Some(event) = event {
                    surface(event);
                }
                result
            }
        }
    }

    /// Our wndproc as `GWLP_WNDPROC` holds it. One place, so the install, the
    /// removal, and the "is it still ours" check cannot drift apart.
    fn pill_wndproc_addr() -> isize {
        pill_wndproc as *const () as isize
    }

    /// The hook's state for this window, or null if it is not installed —
    /// including when the HWND is already dead.
    unsafe fn hook_state(hwnd: HWND) -> *const HookState {
        GetPropW(hwnd, HOOK_PROP).0 as *const HookState
    }

    /// Hand the message on to whoever owned the window before us. A window
    /// always has a wndproc, so `None` means the swap never happened.
    unsafe fn call_prev(
        prev: WNDPROC,
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match prev {
            Some(_) => CallWindowProcW(prev, hwnd, msg, wparam, lparam),
            None => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Read WM_POWERBROADCAST's `POWERBROADCAST_SETTING` payload. Every setting
    /// we register for carries a DWORD, so a shorter payload is not ours.
    unsafe fn read_power_setting(lparam: LPARAM) -> Option<(GUID, u32)> {
        let ptr = lparam.0 as *const POWERBROADCAST_SETTING;
        if ptr.is_null() {
            return None;
        }
        let setting = &*ptr;
        if (setting.DataLength as usize) < std::mem::size_of::<u32>() {
            return None;
        }
        // `Data` is declared `[u8; 1]` — a trailing array. Read unaligned: the
        // struct's alignment says nothing about this field's.
        let data = std::ptr::read_unaligned(setting.Data.as_ptr() as *const u32);
        Some((setting.PowerSetting, data))
    }

    /// Put the original wndproc back and free our state. Idempotent: whichever
    /// of `PillHook::drop` and the WM_NCDESTROY safety net runs first takes the
    /// property with it, and the other finds nothing to do. Both run on the UI
    /// thread, so that check cannot race.
    unsafe fn uninstall(hwnd: HWND) {
        let ptr = hook_state(hwnd) as *mut HookState;
        if ptr.is_null() {
            return;
        }
        // Only unwind our own swap. If something else subclassed on top of us,
        // restoring `prev` would drop that one on the floor — the documented
        // hazard of wndproc chains. Leave the hook in place instead: it still
        // forwards correctly, and the state has to stay alive for it to.
        if GetWindowLongPtrW(hwnd, GWLP_WNDPROC) != pill_wndproc_addr() {
            tracing::warn!("another wndproc sits on top of the pill's; leaving the hook installed");
            return;
        }
        let prev = (*ptr).prev.map(|p| p as usize as isize).unwrap_or(0);
        SetWindowLongPtrW(hwnd, GWLP_WNDPROC, prev);
        let _ = RemovePropW(hwnd, HOOK_PROP);
        drop(Box::from_raw(ptr));
    }

    /// The hook's lifetime, tied to the pill window's. Installing registers for
    /// the two notifications that arrive as messages rather than being sent
    /// unasked; dropping unregisters both and takes the subclass with it.
    pub struct PillHook {
        hwnd: HWND,
        /// `None` when the registration failed — the display-power events are
        /// simply absent, which degrades the wakeup ladder rather than the app.
        power: Option<HPOWERNOTIFY>,
        session_notifications: bool,
    }

    impl PillHook {
        /// Install the one subclass on the pill window. A failure is logged and
        /// survivable: without the hook the pill behaves as it did before it
        /// existed.
        pub fn install(hwnd: HWND, tx: Sender<HookEvent>) -> Self {
            // Every early return below builds its own `Self`. Hoisting one into
            // a local would install the hook and then drop it again on the way
            // out — `PillHook` uninstalls in `Drop`.
            let hookless = || Self {
                hwnd,
                power: None,
                session_notifications: false,
            };
            // The state hangs off the window rather than a global — one window,
            // one sender, freed by whoever takes the hook off. Set before the
            // swap, so the first message already finds it.
            let state = Box::into_raw(Box::new(HookState { prev: None, tx }));
            if let Err(e) = unsafe { SetPropW(hwnd, HOOK_PROP, HANDLE(state as *mut _)) } {
                tracing::error!(error = %e, "could not attach the pill's hook state");
                drop(unsafe { Box::from_raw(state) });
                return hookless();
            }
            unsafe {
                let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, pill_wndproc_addr());
                // `prev` is recorded after the swap, so in between it reads as
                // "no previous wndproc". Nothing can observe that: a message
                // only reaches us from this thread's own message pump, and
                // there is no pump between these two lines.
                //
                // A window always has a wndproc, so 0 means the swap failed —
                // which `WNDPROC` being an `Option` represents exactly.
                (*state).prev = std::mem::transmute::<isize, WNDPROC>(prev);
                if prev == 0 {
                    tracing::error!("could not subclass the pill window");
                    let _ = RemovePropW(hwnd, HOOK_PROP);
                    drop(Box::from_raw(state));
                    return hookless();
                }
            }

            // GUID_SESSION_DISPLAY_STATUS, not GUID_MONITOR_POWER_ON (deprecated)
            // and not GUID_CONSOLE_DISPLAY_STATE (the session-0 choice).
            let power = unsafe {
                RegisterPowerSettingNotification(
                    HANDLE(hwnd.0),
                    &GUID_SESSION_DISPLAY_STATUS,
                    DEVICE_NOTIFY_WINDOW_HANDLE,
                )
            };
            let power = match power {
                Ok(h) => Some(h),
                Err(e) => {
                    tracing::warn!(error = %e, "display-status notifications unavailable");
                    None
                }
            };

            let session_notifications =
                unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) };
            let session_notifications = match session_notifications {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "session lock notifications unavailable");
                    false
                }
            };

            tracing::debug!(
                power = power.is_some(),
                session_notifications,
                "pill wndproc hook installed"
            );
            Self {
                hwnd,
                power,
                session_notifications,
            }
        }
    }

    impl Drop for PillHook {
        fn drop(&mut self) {
            unsafe {
                if let Some(power) = self.power.take() {
                    let _ = UnregisterPowerSettingNotification(power);
                }
                if self.session_notifications {
                    let _ = WTSUnRegisterSessionNotification(self.hwnd);
                }
                uninstall(self.hwnd);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::UI::WindowsAndMessaging::{
            WM_CLOSE, WM_MOUSEMOVE, WM_PAINT, WM_SETFOCUS, WM_SIZE, WTS_CONSOLE_DISCONNECT,
            WTS_REMOTE_DISCONNECT, WTS_SESSION_LOGON,
        };

        /// The whole reason the pill can be clicked at all: the click is
        /// answered here, never forwarded, so DefWindowProc's MA_ACTIVATE — the
        /// documented WS_EX_NOACTIVATE hole — is never reached.
        #[test]
        fn a_click_on_the_pill_is_answered_without_activating_it() {
            assert_eq!(
                classify(WM_MOUSEACTIVATE, 0, None),
                Reaction::Answer {
                    result: LRESULT(MA_NOACTIVATE as isize),
                    event: None
                }
            );
        }

        /// winit acts on both of these itself, so surfacing must not swallow
        /// them.
        #[test]
        fn display_and_dpi_changes_are_surfaced_and_still_forwarded() {
            assert_eq!(
                classify(WM_DISPLAYCHANGE, 32, None),
                Reaction::Watch(HookEvent::DisplayChanged)
            );
            // wParam packs the new DPI into both words; we read the low one.
            assert_eq!(
                classify(WM_DPICHANGED, 0x0090_0090, None),
                Reaction::Watch(HookEvent::DpiChanged { dpi: 144 })
            );
        }

        #[test]
        fn the_display_going_off_and_coming_back_are_surfaced() {
            let off = classify(
                WM_POWERBROADCAST,
                PBT_POWERSETTINGCHANGE as usize,
                Some((GUID_SESSION_DISPLAY_STATUS, 0)),
            );
            assert_eq!(
                off,
                Reaction::Answer {
                    result: LRESULT(TRUE.0 as isize),
                    event: Some(HookEvent::DisplayPower { on: false })
                }
            );
            for data in [1, 2] {
                assert_eq!(
                    classify(
                        WM_POWERBROADCAST,
                        PBT_POWERSETTINGCHANGE as usize,
                        Some((GUID_SESSION_DISPLAY_STATUS, data)),
                    ),
                    Reaction::Answer {
                        result: LRESULT(TRUE.0 as isize),
                        event: Some(HookEvent::DisplayPower { on: true })
                    },
                    "display data {data} (2 is dimmed, which is still on)"
                );
            }
        }

        /// We register for one power setting, but the message is shared —
        /// anything else arriving on it belongs to whoever asked for it.
        #[test]
        fn another_power_setting_passes_through() {
            let other = GUID::from_u128(0x0da73b3a_0000_0000_0000_000000000000);
            assert_eq!(
                classify(
                    WM_POWERBROADCAST,
                    PBT_POWERSETTINGCHANGE as usize,
                    Some((other, 0))
                ),
                Reaction::Pass
            );
            // Suspend/resume broadcasts share the message too.
            assert_eq!(classify(WM_POWERBROADCAST, 0x0004, None), Reaction::Pass);
        }

        /// Surfaced and still forwarded — this message's return value is
        /// documented as ignored, so answering it would claim nothing.
        #[test]
        fn locking_and_unlocking_the_session_are_surfaced() {
            assert_eq!(
                classify(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK as usize, None),
                Reaction::Watch(HookEvent::SessionLock { locked: true })
            );
            assert_eq!(
                classify(WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK as usize, None),
                Reaction::Watch(HookEvent::SessionLock { locked: false })
            );
            // The same message carries logon and the disconnects, which
            // nothing acts on.
            for code in [
                WTS_SESSION_LOGON,
                WTS_CONSOLE_DISCONNECT,
                WTS_REMOTE_DISCONNECT,
            ] {
                assert_eq!(
                    classify(WM_WTSSESSION_CHANGE, code as usize, None),
                    Reaction::Pass,
                    "session code {code}"
                );
            }
        }

        /// An RDP client reconnecting, and the session going back to the
        /// physical console, both rebuild what is drawing the desktop — and a
        /// layered surface does not reliably survive that. The pill has to hear
        /// about it or it comes back invisible, with no way to recover but to
        /// start a dictation.
        #[test]
        fn reattaching_the_session_to_a_terminal_is_surfaced() {
            for code in [WTS_REMOTE_CONNECT, WTS_CONSOLE_CONNECT] {
                assert_eq!(
                    classify(WM_WTSSESSION_CHANGE, code as usize, None),
                    Reaction::Watch(HookEvent::SessionReconnected),
                    "session code {code}"
                );
            }
        }

        /// The hook claims five messages and nothing else. Everything the
        /// window does — painting, sizing, focus, closing, the mouse moving
        /// over it — has to reach the original wndproc untouched.
        #[test]
        fn every_other_message_passes_through() {
            for msg in [
                WM_PAINT,
                WM_SIZE,
                WM_CLOSE,
                WM_SETFOCUS,
                WM_MOUSEMOVE,
                WM_NCDESTROY,
            ] {
                assert_eq!(classify(msg, 0, None), Reaction::Pass, "message {msg:#x}");
            }
        }
    }

    /// The install/remove half, against a real window. `classify` can be tested
    /// on its own; that the subclass is actually reached, that it answers rather
    /// than forwards, and that it comes off cleanly, cannot.
    #[cfg(test)]
    mod window_tests {
        use super::*;
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, SendMessageW, HWND_MESSAGE, WINDOW_EX_STYLE,
            WINDOW_STYLE, WM_GETTEXTLENGTH, WM_MOUSEACTIVATE,
        };

        /// A real HWND on this thread, so `SendMessageW` reaches the wndproc
        /// directly. "STATIC" is a predefined class with a wndproc of its own —
        /// which is the point: it is what the pass-through has to reach.
        fn message_window() -> HWND {
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("STATIC"),
                    w!("draft"),
                    WINDOW_STYLE(0),
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    None,
                    None,
                    None,
                )
                .expect("create test window")
            }
        }

        fn hook_installed(hwnd: HWND) -> bool {
            unsafe { GetWindowLongPtrW(hwnd, GWLP_WNDPROC) == pill_wndproc_addr() }
        }

        #[test]
        fn the_hook_installs_answers_surfaces_and_comes_off_cleanly() {
            let (tx, rx) = crossbeam_channel::unbounded();
            let hwnd = message_window();
            let hook = PillHook::install(hwnd, tx);
            assert!(hook_installed(hwnd), "subclass not installed");

            // Answered by the hook, never forwarded: MA_ACTIVATE is the hole.
            let answer = unsafe { SendMessageW(hwnd, WM_MOUSEACTIVATE, WPARAM(0), LPARAM(0)) };
            assert_eq!(answer.0, MA_NOACTIVATE as isize);
            assert!(rx.try_recv().is_err(), "a click is not an app-loop event");

            // Surfaced to the app loop.
            unsafe { SendMessageW(hwnd, WM_DISPLAYCHANGE, WPARAM(0), LPARAM(0)) };
            assert_eq!(rx.try_recv(), Ok(HookEvent::DisplayChanged));

            // Everything else still reaches STATIC's own wndproc — which is the
            // only thing that knows this window's text is 5 characters long.
            let len = unsafe { SendMessageW(hwnd, WM_GETTEXTLENGTH, WPARAM(0), LPARAM(0)) };
            assert_eq!(len.0, "draft".len() as isize);

            drop(hook);
            assert!(!hook_installed(hwnd), "subclass outlived the hook");
            unsafe { DestroyWindow(hwnd).expect("destroy test window") };
        }

        /// The other order: the window dies first. WM_NCDESTROY unwinds the
        /// hook, and the later `Drop` has to find nothing left to free — a
        /// second free here would take the process down.
        #[test]
        fn a_window_destroyed_under_the_hook_unwinds_it_exactly_once() {
            let (tx, _rx) = crossbeam_channel::unbounded();
            let hwnd = message_window();
            let hook = PillHook::install(hwnd, tx);
            assert!(hook_installed(hwnd), "hook not installed");
            unsafe { DestroyWindow(hwnd).expect("destroy test window") };
            drop(hook);
        }
    }
}
