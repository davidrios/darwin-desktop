//! AppKit input to Wayland seat events.

use std::collections::HashSet;

use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventPhase};
use smithay::{
    backend::input::{Axis, AxisSource, ButtonState, InputTime, KeyState},
    input::{
        keyboard::{FilterResult, Keycode},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{keymap::mac_to_evdev, state::State};

/// evdev button codes (`linux/input-event-codes.h`).
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

/// The event's time. `NSEvent.timestamp` is seconds since boot (monotonic).
pub fn event_time(event: &NSEvent) -> InputTime {
    InputTime::from_micros((event.timestamp() * 1_000_000.0) as u64)
}

/// Keys we told clients are down, so they can be released when we lose focus.
#[derive(Default)]
pub struct Keys {
    down: HashSet<u32>,
}

impl Keys {
    pub fn key(&mut self, state: &mut State, evdev: u32, pressed: bool, time: InputTime) {
        let changed = if pressed { self.down.insert(evdev) } else { self.down.remove(&evdev) };
        if !changed && !pressed {
            return; // a release for a press we never sent
        }
        tracing::debug!(evdev, pressed, "key");
        let Some(keyboard) = state.seat.get_keyboard() else { return };
        let key_state = if pressed { KeyState::Pressed } else { KeyState::Released };
        keyboard.input::<(), _>(
            state,
            Keycode::new(evdev + 8),
            key_state,
            SERIAL_COUNTER.next_serial(),
            time,
            |_, _, _| FilterResult::Forward,
        );
    }

    /// `keyDown:`/`keyUp:` from the event monitor. Returns false for keys with no evdev code.
    pub fn key_event(&mut self, state: &mut State, event: &NSEvent, pressed: bool) -> bool {
        let Some(evdev) = mac_to_evdev(event.keyCode()) else { return false };
        if pressed && event.isARepeat() {
            return true; // clients repeat keys themselves (wl_keyboard.repeat_info)
        }
        self.key(state, evdev, pressed, event_time(event));
        true
    }

    /// `flagsChanged:`: a modifier went down or up. Which one, and which side,
    /// comes from the device-dependent bits of the flags (as in mitsuami).
    pub fn modifier_event(&mut self, state: &mut State, event: &NSEvent) {
        let code = event.keyCode();
        let Some(evdev) = mac_to_evdev(code) else { return };
        let flags = event.modifierFlags().0;
        let side = match code {
            0x38 => 0x02,   // left Shift
            0x3C => 0x04,   // right Shift
            0x3B => 0x01,   // left Control
            0x3E => 0x2000, // right Control
            0x3A => 0x20,   // left Option
            0x3D => 0x40,   // right Option
            0x37 => 0x08,   // left Command
            0x36 => 0x10,   // right Command
            0x39 => {
                // Caps Lock reports its toggle, not the key: press and release.
                let time = event_time(event);
                self.key(state, evdev, true, time);
                self.key(state, evdev, false, time);
                return;
            }
            _ => return,
        };
        let pressed = flags & side != 0;
        if pressed != self.down.contains(&evdev) {
            self.key(state, evdev, pressed, event_time(event));
        }
    }

    /// The window stopped being the key window: release everything held, or
    /// a Cmd+Tab away would leave Cmd down in the client.
    pub fn release_all(&mut self, state: &mut State, time: InputTime) {
        let down: Vec<u32> = self.down.iter().copied().collect();
        for evdev in down {
            self.key(state, evdev, false, time);
        }
    }

    pub fn any_down(&self) -> bool {
        !self.down.is_empty()
    }
}

pub fn is_quit_chord(event: &NSEvent) -> bool {
    let flags = event.modifierFlags();
    // Cmd+Shift+Q (kVK_ANSI_Q = 0x0C): leave the session (docs/decisions.md).
    event.keyCode() == 0x0C
        && flags.contains(NSEventModifierFlags::Command)
        && flags.contains(NSEventModifierFlags::Shift)
        && !flags.contains(NSEventModifierFlags::Control)
        && !flags.contains(NSEventModifierFlags::Option)
}

pub fn pointer_motion(state: &mut State, location: Point<f64, Logical>, time: InputTime) {
    state.pointer_location = location;
    let under = state.surface_under(location);
    if under.is_none() {
        // Over no client: the default cursor (clients set theirs on enter).
        state.cursor_status = smithay::input::pointer::CursorImageStatus::default_named();
    }
    let Some(pointer) = state.seat.get_pointer() else { return };
    pointer.motion(state, under, &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time });
    pointer.frame(state);
}

pub fn pointer_button(state: &mut State, button: u32, pressed: bool, time: InputTime) {
    let Some(pointer) = state.seat.get_pointer() else { return };
    let serial = SERIAL_COUNTER.next_serial();
    // Click to focus, unless a grab (a popup menu, a drag) owns the pointer.
    if pressed && !pointer.is_grabbed() {
        let window = state.space.element_under(state.pointer_location).map(|(w, _)| w.clone());
        if window.is_some() {
            state.focus(window, serial);
        }
    }
    let button_state = if pressed { ButtonState::Pressed } else { ButtonState::Released };
    pointer.button(state, &ButtonEvent { button, state: button_state, serial, time });
    pointer.frame(state);
}

pub fn scroll(state: &mut State, event: &NSEvent) {
    let Some(pointer) = state.seat.get_pointer() else { return };
    let time = event_time(event);
    // macOS deltas are already "natural"-adjusted and point the other way from Wayland's axes.
    let (dx, dy) = (-event.scrollingDeltaX(), -event.scrollingDeltaY());
    let mut frame;
    if event.hasPreciseScrollingDeltas() {
        frame = AxisFrame::new(time).source(AxisSource::Finger);
        if dx != 0.0 {
            frame = frame.value(Axis::Horizontal, dx);
        }
        if dy != 0.0 {
            frame = frame.value(Axis::Vertical, dy);
        }
        let phase = event.phase();
        let momentum = event.momentumPhase();
        let ended = phase.contains(NSEventPhase::Ended) || phase.contains(NSEventPhase::Cancelled);
        if ended && momentum == NSEventPhase::None {
            frame = frame.stop(Axis::Horizontal).stop(Axis::Vertical);
        }
    } else {
        // A wheel: deltas in lines. One notch is 120 in v120 units.
        frame = AxisFrame::new(time).source(AxisSource::Wheel);
        if dx != 0.0 {
            frame = frame.value(Axis::Horizontal, dx * 15.0).v120(Axis::Horizontal, (dx * 120.0) as i32);
        }
        if dy != 0.0 {
            frame = frame.value(Axis::Vertical, dy * 15.0).v120(Axis::Vertical, (dy * 120.0) as i32);
        }
    }
    pointer.axis(state, frame);
    pointer.frame(state);
}
