// Waking the event loop.
//
// Draft's loop rests at `ControlFlow::Wait`, which on Windows is an indefinite
// `MsgWaitForMultipleObjectsEx` — it wakes for a *message*, and a crossbeam
// send is not one. Every producer that talks to the loop over a channel
// therefore has to post one after sending, or its news would sit in the channel
// until something else happened to wake the loop.
//
// That is what this is: one clonable handle, held by the hotkey handler, the
// tray's menu handler, each transcription worker, the settings watcher and the
// update check. `send_event` posts to winit's own message window, so the loop
// wakes, dispatches, and drains every channel in `about_to_wait` exactly as it
// did when a 20 Hz timer was doing the waking.
//
// The payload is deliberately empty. The channels already carry *what*
// happened, and a second copy of it in the user event would be two places to
// keep in step; all the loop needs from this is "look again".

use winit::event_loop::EventLoopProxy;

/// The user event. Carries nothing — see the module header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Wake;

/// A handle onto the event loop that any thread may poke.
///
/// Cheap to clone, and cheap to call: a failed post only means the loop is
/// gone, which is not something a producer can do anything about.
#[derive(Clone)]
pub struct Waker(EventLoopProxy<Wake>);

impl Waker {
    pub fn new(proxy: EventLoopProxy<Wake>) -> Self {
        Self(proxy)
    }

    /// Tell the loop there is something to drain. Safe to call from any
    /// thread, including the loop's own.
    pub fn wake(&self) {
        let _ = self.0.send_event(Wake);
    }
}
