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

use crate::{
    binds::{Mods, lower},
    keymap::mac_to_evdev,
    state::State,
};

/// macOS trackpad points to libinput-like finger scroll units. Both sides
/// are logical units, so no screen scale is involved; GTK multiplies finger
/// scrolling by 2.5 and macOS deltas are already accelerated. Tuned by hand
/// (2026-11-24): 0.4 still felt about twice too fast.
const TOUCHPAD_SCROLL_SCALE: f64 = 0.2;

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
    /// Keys whose press triggered a bind: their release is swallowed too.
    bound: HashSet<u32>,
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
        let bound = &mut self.bound;
        // Binds are matched on the layout's keysym for the key (its Latin
        // keysym on non-Latin layouts), with the modifiers held.
        let hit = keyboard.input(state, Keycode::new(evdev + 8), key_state, SERIAL_COUNTER.next_serial(), time, |state, mods, handle| {
            if !pressed {
                return if bound.remove(&evdev) { FilterResult::Intercept(None) } else { FilterResult::Forward };
            }
            let Some(sym) = handle.raw_latin_sym_or_raw_current_sym() else { return FilterResult::Forward };
            match state.config.find_bind(Mods::from(mods), lower(sym)) {
                Some(hit) => {
                    bound.insert(evdev);
                    FilterResult::Intercept(Some(hit))
                }
                None => FilterResult::Forward,
            }
        });
        if let Some(Some(hit)) = hit {
            state.run_bind(hit);
        }
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
    // A mouse-bind drag owns the pointer.
    if state.drag.is_some() {
        state.drag_motion(location);
        return;
    }
    let under = state.surface_under(location);
    if under.is_none() {
        // Over no client: the default cursor (clients set theirs on enter).
        state.cursor_status = smithay::input::pointer::CursorImageStatus::default_named();
    }
    let Some(pointer) = state.seat.get_pointer() else { return };
    // follow_mouse: keyboard focus goes to the window under the pointer.
    if state.config.settings.follow_mouse >= 1 && !pointer.is_grabbed() {
        if let Some(window) = state.window_under(location) {
            if state.focused_window().as_ref() != Some(&window) {
                state.focus(Some(window), SERIAL_COUNTER.next_serial());
            }
        }
    }
    pointer.motion(state, under, &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time });
    pointer.frame(state);
}

pub fn pointer_button(state: &mut State, button: u32, pressed: bool, time: InputTime) {
    let Some(pointer) = state.seat.get_pointer() else { return };
    let serial = SERIAL_COUNTER.next_serial();
    // Mouse binds (Cmd+drag): the press and its release stay with the compositor.
    if !pressed {
        if state.drag.as_ref().is_some_and(|d| d.button == button) {
            state.end_drag();
            state.bound_buttons.remove(&button);
            return;
        }
        if state.bound_buttons.remove(&button) {
            return;
        }
    } else if !pointer.is_grabbed() && state.drag.is_none() {
        let mods = state.seat.get_keyboard().map(|k| Mods::from(&k.modifier_state())).unwrap_or_default();
        let hit = state.config.find_mouse_bind(mods, crate::binds::MouseKey::Button(button));
        if let Some(hit) = hit {
            let taken = match state.config.bind_action(&hit) {
                Some(action) => state.mouse_bind(button, action),
                None => {
                    state.run_bind(hit);
                    true
                }
            };
            if taken {
                state.bound_buttons.insert(button);
                return;
            }
        }
    }
    // Click to focus, unless a grab (a popup menu, a drag) owns the pointer.
    if pressed && !pointer.is_grabbed() {
        use smithay::wayland::shell::wlr_layer::Layer;
        let pos = state.pointer_location;
        let upper = state.layer_surface_under(state.upper_layers(), pos);
        if let Some((layer, _, _)) = upper {
            state.focus_layer_on_click(&layer);
        } else if let Some(window) = state.window_under(pos) {
            state.focus(Some(window), serial);
        } else if let Some((layer, _, _)) = state.layer_surface_under(&[Layer::Bottom, Layer::Background], pos) {
            state.focus_layer_on_click(&layer);
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
    // Wheel binds (Cmd+scroll), one per notch. Trackpad scrolling doesn't
    // trigger them: it would fire dozens of times a swipe.
    if !event.hasPreciseScrollingDeltas() {
        use crate::binds::MouseKey;
        let mods = state.seat.get_keyboard().map(|k| Mods::from(&k.modifier_state())).unwrap_or_default();
        let key = if dy > 0.0 {
            Some(MouseKey::WheelDown)
        } else if dy < 0.0 {
            Some(MouseKey::WheelUp)
        } else if dx > 0.0 {
            Some(MouseKey::WheelRight)
        } else if dx < 0.0 {
            Some(MouseKey::WheelLeft)
        } else {
            None
        };
        if let Some(hit) = key.and_then(|k| state.config.find_mouse_bind(mods, k)) {
            state.run_bind(hit);
            return;
        }
    }
    let mut frame;
    if event.hasPreciseScrollingDeltas() {
        // macOS's own momentum after the fingers lift: dropped. Wayland
        // clients do their own kinetic scrolling from the stop event, as on
        // Linux; passing both made scrolls fly.
        if event.momentumPhase() != NSEventPhase::None {
            return;
        }
        // macOS deltas are pixels; clients expect libinput's finger units,
        // which GTK multiplies by 2.5 (MAGIC_SCROLL_FACTOR). Scale to match,
        // times Hyprland's `input.touchpad.scroll_factor`.
        let factor = TOUCHPAD_SCROLL_SCALE * state.config.settings.touchpad_scroll_factor;
        let (dx, dy) = (dx * factor, dy * factor);
        frame = AxisFrame::new(time).source(AxisSource::Finger);
        if dx != 0.0 {
            frame = frame.value(Axis::Horizontal, dx);
        }
        if dy != 0.0 {
            frame = frame.value(Axis::Vertical, dy);
        }
        let phase = event.phase();
        if phase.contains(NSEventPhase::Ended) || phase.contains(NSEventPhase::Cancelled) {
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
