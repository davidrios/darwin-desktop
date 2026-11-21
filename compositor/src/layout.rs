//! xkb keymaps generated from macOS keyboard layouts.
//!
//! Wayland clients turn key codes into characters with the xkb keymap the
//! compositor sends. Instead of picking the nearest stock xkb layout, the
//! keymap is generated from the macOS layout itself: every key is run through
//! `UCKeyTranslate` with no modifier, Shift, Option and Option+Shift, which
//! become xkb levels 1-4. Right Option is the level-3 shift that reaches the
//! Option characters, as on the Mac; left Option is Alt, for app shortcuts. macOS dead keys become xkb dead keysyms, which clients' compose
//! tables combine as usual. Keys that type no character (modifiers, arrows,
//! function and keypad keys) get fixed keysyms.
//!
//! The layout comes from the macOS input source (Text Input Sources API),
//! the current one by default or a named one (`--keyboard-layout`).

use std::{
    ffi::c_void,
    fmt::Write as _,
    ptr::{self, NonNull},
};

use objc2_core_foundation::{CFArray, CFData, CFDictionary, CFRetained, CFString, CFType};

use crate::keymap::mac_to_evdev;

type OSStatus = i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISCreateInputSourceList(properties: *const CFDictionary, include_all_installed: u8) -> *mut CFArray;
    fn TISGetInputSourceProperty(source: *mut c_void, key: &CFString) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: &'static CFString;
    static kTISPropertyInputSourceID: &'static CFString;
    static kTISPropertyLocalizedName: &'static CFString;
    pub static kTISNotifySelectedKeyboardInputSourceChanged: &'static CFString;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const c_void,
        virtual_key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        key_translate_options: u32,
        dead_key_state: *mut u32,
        max_string_length: usize,
        actual_string_length: *mut usize,
        unicode_string: *mut u16,
    ) -> OSStatus;
}

const KEY_ACTION_DOWN: u16 = 0;
// Carbon modifier bits, shifted right by 8 as UCKeyTranslate wants them.
const MOD_SHIFT: u32 = 0x02;
const MOD_OPTION: u32 = 0x08;
const KVK_SPACE: u16 = 0x31;

/// A macOS keyboard layout's `uchr` data, kept alive with its input source.
pub struct MacLayout {
    pub id: String,
    pub name: String,
    data: CFRetained<CFData>,
}

impl MacLayout {
    /// The layout macOS is using now.
    pub fn current() -> Option<MacLayout> {
        // SAFETY: returns a +1 reference (Copy rule) or null.
        let source = unsafe { TISCopyCurrentKeyboardLayoutInputSource() };
        let source = NonNull::new(source.cast::<CFType>())?;
        // SAFETY: we own the reference returned above.
        let source = unsafe { CFRetained::from_raw(source) };
        Self::from_source(&source)
    }

    /// An installed layout by its input source id, e.g. `com.apple.keylayout.Brazilian-Pro`.
    pub fn by_id(id: &str) -> Option<MacLayout> {
        // SAFETY: Create rule: a +1 array or null; include all installed sources.
        let list = unsafe { TISCreateInputSourceList(ptr::null(), 1) };
        let list = NonNull::new(list)?;
        let list = unsafe { CFRetained::from_raw(list) };
        let count = list.count();
        (0..count).find_map(|i| {
            // SAFETY: index in range; the array holds input sources (CF objects).
            let item = unsafe { list.value_at_index(i) } as *mut CFType;
            let item = unsafe { CFRetained::retain(NonNull::new(item)?) };
            let layout = Self::from_source(&item)?;
            (layout.id == id).then_some(layout)
        })
    }

    fn from_source(source: &CFType) -> Option<MacLayout> {
        let src = source as *const CFType as *mut c_void;
        // SAFETY: Get rule: borrowed values owned by the source, retained below.
        let data = unsafe { TISGetInputSourceProperty(src, kTISPropertyUnicodeKeyLayoutData) } as *mut CFData;
        let id = unsafe { TISGetInputSourceProperty(src, kTISPropertyInputSourceID) } as *const CFString;
        let name = unsafe { TISGetInputSourceProperty(src, kTISPropertyLocalizedName) } as *const CFString;
        let data = unsafe { CFRetained::retain(NonNull::new(data)?) };
        let id = unsafe { id.as_ref() }.map(|s| s.to_string()).unwrap_or_default();
        let name = unsafe { name.as_ref() }.map(|s| s.to_string()).unwrap_or_else(|| id.clone());
        Some(MacLayout { id, name, data })
    }

    fn translate(&self, code: u16, modifiers: u32, dead_state: &mut u32) -> String {
        let mut buf = [0u16; 8];
        let mut len = 0usize;
        // SAFETY: the layout data is a valid `uchr` blob owned by self; the
        // output buffer is 8 UTF-16 units.
        let status = unsafe {
            UCKeyTranslate(
                self.data.byte_ptr() as *const c_void,
                code,
                KEY_ACTION_DOWN,
                modifiers,
                LMGetKbdType() as u32,
                0,
                dead_state,
                buf.len(),
                &mut len,
                buf.as_mut_ptr(),
            )
        };
        if status != 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len.min(buf.len())])
    }

    /// What a key types with these modifiers: a character, a dead key, or nothing.
    fn key_output(&self, code: u16, modifiers: u32) -> Output {
        let mut dead = 0u32;
        let text = self.translate(code, modifiers, &mut dead);
        if text.is_empty() && dead != 0 {
            // A dead key: Space after it gives the accent on its own.
            let accent = self.translate(KVK_SPACE, 0, &mut dead);
            return match accent.chars().next() {
                Some(c) => Output::Dead(c),
                None => Output::None,
            };
        }
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if !c.is_control() => Output::Char(c),
            _ => Output::None,
        }
    }

    /// The xkb keymap for this layout.
    pub fn xkb_keymap(&self) -> String {
        let mut keycodes = String::new();
        let mut symbols = String::new();
        for code in 0u16..128 {
            let Some(evdev) = mac_to_evdev(code) else { continue };
            let xkb = evdev + 8;
            let _ = writeln!(keycodes, "        <K{evdev}> = {xkb};");
            let line = match fixed_keysyms(evdev) {
                Some(syms) => format!("        key <K{evdev}> {{ [ {syms} ] }};"),
                None => {
                    let levels = [0, MOD_SHIFT, MOD_OPTION, MOD_OPTION | MOD_SHIFT].map(|m| self.key_output(code, m));
                    if levels.iter().all(|l| matches!(l, Output::None)) {
                        continue;
                    }
                    let semi = matches!((&levels[0], &levels[1]),
                        (Output::Char(a), Output::Char(b)) if a.is_lowercase() && a.to_uppercase().eq(std::iter::once(*b)));
                    let ty = if semi { "FOUR_LEVEL_SEMIALPHABETIC" } else { "FOUR_LEVEL" };
                    let syms: Vec<String> = levels.iter().map(Output::keysym).collect();
                    format!("        key <K{evdev}> {{ type = \"{ty}\", [ {} ] }};", syms.join(", "))
                }
            };
            let _ = writeln!(symbols, "{line}");
        }
        format!(
            "// Generated from the macOS keyboard layout \"{name}\" ({id}).\n\
xkb_keymap {{\n\
    xkb_keycodes \"macos\" {{\n\
        minimum = 8;\n\
        maximum = 255;\n\
{keycodes}\
        indicator 1 = \"Caps Lock\";\n\
    }};\n\
    xkb_types {{ include \"complete\" }};\n\
    xkb_compat {{ include \"complete\" }};\n\
    xkb_symbols \"macos\" {{\n\
        name[Group1] = \"{name}\";\n\
{symbols}\
        modifier_map Shift {{ <K42>, <K54> }};\n\
        modifier_map Lock {{ <K58> }};\n\
        modifier_map Control {{ <K29>, <K97> }};\n\
        modifier_map Mod1 {{ <K56> }};\n\
        modifier_map Mod4 {{ <K125>, <K126> }};\n\
        modifier_map Mod5 {{ <K100> }};\n\
    }};\n\
}};\n",
            name = self.name.replace('"', "'"),
            id = self.id,
        )
    }
}

enum Output {
    Char(char),
    Dead(char),
    None,
}

impl Output {
    fn keysym(&self) -> String {
        match self {
            Output::Char(c) => char_keysym(*c),
            Output::Dead(c) => dead_keysym(*c).map(str::to_string).unwrap_or_else(|| char_keysym(*c)),
            Output::None => "NoSymbol".into(),
        }
    }
}

/// A character's keysym: Latin-1 keysyms equal their code points; everything
/// else is a Unicode keysym (0x01000000 + code point).
fn char_keysym(c: char) -> String {
    let cp = c as u32;
    if (0x20..=0x7e).contains(&cp) || (0xa0..=0xff).contains(&cp) {
        format!("0x{cp:04x}")
    } else {
        format!("0x{:08x}", 0x0100_0000 + cp)
    }
}

/// The xkb dead keysym for the accent a macOS dead key types on its own.
fn dead_keysym(accent: char) -> Option<&'static str> {
    Some(match accent {
        '`' => "dead_grave",
        '´' | '\'' => "dead_acute",
        '^' | 'ˆ' => "dead_circumflex",
        '~' | '˜' => "dead_tilde",
        '¨' | '"' => "dead_diaeresis",
        '¸' => "dead_cedilla",
        'ˇ' => "dead_caron",
        '˘' => "dead_breve",
        '˙' => "dead_abovedot",
        '˚' => "dead_abovering",
        '˝' => "dead_doubleacute",
        '˛' => "dead_ogonek",
        '¯' => "dead_macron",
        _ => return None,
    })
}

/// Keys whose meaning doesn't come from the layout.
fn fixed_keysyms(evdev: u32) -> Option<&'static str> {
    Some(match evdev {
        1 => "Escape",
        14 => "BackSpace",
        15 => "Tab, ISO_Left_Tab",
        28 => "Return",
        29 => "Control_L",
        42 => "Shift_L",
        54 => "Shift_R",
        // Left Option is Alt (app shortcuts, menu mnemonics); right Option is
        // the level-3 shift that types the layout's Option characters.
        56 => "Alt_L",
        100 => "ISO_Level3_Shift",
        57 => "space",
        58 => "Caps_Lock",
        59..=68 => ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10"][evdev as usize - 59],
        69 => "Clear",
        71 => "KP_7",
        72 => "KP_8",
        73 => "KP_9",
        74 => "KP_Subtract",
        75 => "KP_4",
        76 => "KP_5",
        77 => "KP_6",
        78 => "KP_Add",
        79 => "KP_1",
        80 => "KP_2",
        81 => "KP_3",
        82 => "KP_0",
        83 => "KP_Decimal",
        55 => "KP_Multiply",
        87 => "F11",
        88 => "F12",
        96 => "KP_Enter",
        97 => "Control_R",
        98 => "KP_Divide",
        102 => "Home",
        103 => "Up",
        104 => "Prior",
        105 => "Left",
        106 => "Right",
        107 => "End",
        108 => "Down",
        109 => "Next",
        110 => "Insert",
        111 => "Delete",
        113 => "XF86AudioMute",
        114 => "XF86AudioLowerVolume",
        115 => "XF86AudioRaiseVolume",
        117 => "KP_Equal",
        125 => "Super_L",
        126 => "Super_R",
        127 => "Menu",
        183..=190 => ["F13", "F14", "F15", "F16", "F17", "F18", "F19", "F20"][evdev as usize - 183],
        _ => return None,
    })
}
