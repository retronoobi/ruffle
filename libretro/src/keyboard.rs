//! Merge libretro keyboard callbacks and polling. Some frontends forward only
//! key states, so text input must also work without character callbacks.
use ruffle_core::events::{PlayerEvent, TextControlCode as C};

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Callback {
    pub down: bool,
    pub key: u32,
    pub character: u32,
    pub modifiers: u16,
}

pub struct Keyboard {
    callback_down: [bool; 324],
    held: [bool; 324],
    age: [u16; 324],
    native_repeat: [bool; 324],
    caps: bool,
}
impl Default for Keyboard {
    fn default() -> Self {
        Self {
            callback_down: [false; 324],
            held: [false; 324],
            age: [0; 324],
            native_repeat: [false; 324],
            caps: false,
        }
    }
}
impl Keyboard {
    pub fn release_callback_events(&self) -> Vec<Callback> {
        self.callback_down
            .iter()
            .enumerate()
            .filter_map(|(key, down)| {
                down.then_some(Callback {
                    down: false,
                    key: key as u32,
                    character: 0,
                    modifiers: 0,
                })
            })
            .collect()
    }
    fn edge(&mut self, id: usize, down: bool, out: &mut Vec<PlayerEvent>) {
        if self.held[id] == down {
            return;
        }
        self.held[id] = down;
        self.age[id] = 0;
        if !down {
            self.native_repeat[id] = false;
        }
        if down && id == 301 {
            self.caps = !self.caps;
        }
        if let Some(key) = crate::key_descriptor(id as u32) {
            out.push(if down {
                PlayerEvent::KeyDown { key }
            } else {
                PlayerEvent::KeyUp { key }
            });
        }
    }
    pub fn update(
        &mut self,
        physical: &[bool; 324],
        pad: &[bool; 324],
        callbacks: &[Callback],
    ) -> Vec<PlayerEvent> {
        let mut out = Vec::new();
        let mut seen = [false; 324];
        for event in callbacks {
            let id = event.key as usize;
            if id < 324 {
                seen[id] = true;
                let was_held = self.held[id];
                self.callback_down[id] = event.down;
                self.edge(id, event.down || physical[id] || pad[id], &mut out);
                if event.down {
                    self.native_repeat[id] = true;
                    if was_held && let Some(key) = crate::key_descriptor(event.key) {
                        out.push(PlayerEvent::KeyDown { key });
                    }
                    append_text(id, event.character, event.modifiers, &mut out);
                }
            } else if event.down {
                // Character-only callbacks may use an unknown key code.
                append_text(id, event.character, event.modifiers, &mut out);
            }
        }
        let shift =
            physical[303] || physical[304] || self.callback_down[303] || self.callback_down[304];
        let ctrl =
            physical[305] || physical[306] || self.callback_down[305] || self.callback_down[306];
        let alt =
            physical[307] || physical[308] || self.callback_down[307] || self.callback_down[308];
        let modifiers = u16::from(shift)
            | (u16::from(ctrl) << 1)
            | (u16::from(alt) << 2)
            | (u16::from(self.caps) << 5);
        // Caps Lock must be observed before translating letters in the same frame.
        if physical[301] && !self.held[301] && !seen[301] {
            self.edge(301, true, &mut out);
        }
        let modifiers = (modifiers & !(1 << 5)) | (u16::from(self.caps) << 5);
        for id in 0..324 {
            let down = physical[id] || pad[id] || self.callback_down[id];
            let pressed = down && !self.held[id];
            self.edge(id, down, &mut out);
            if down {
                self.age[id] = self.age[id].saturating_add(1);
                let repeat = self.age[id] >= 30 && (self.age[id] - 30).is_multiple_of(3);
                // Only a physical keyboard generates fallback text. Joypads keep
                // producing game key events without typing into focused fields.
                if physical[id] && !seen[id] && (pressed || (repeat && !self.native_repeat[id])) {
                    if !pressed && let Some(key) = crate::key_descriptor(id as u32) {
                        out.push(PlayerEvent::KeyDown { key });
                    }
                    append_text(id, 0, modifiers, &mut out);
                }
            }
        }
        out
    }
}

fn append_text(id: usize, character: u32, modifiers: u16, out: &mut Vec<PlayerEvent>) {
    let shift = modifiers & 1 != 0;
    let ctrl = modifiers & 2 != 0;
    let alt = modifiers & 4 != 0;
    let control = match id {
        8 => Some(if ctrl { C::BackspaceWord } else { C::Backspace }),
        127 => Some(if ctrl { C::DeleteWord } else { C::Delete }),
        13 | 271 => Some(C::Enter),
        276 => Some(match (shift, ctrl) {
            (true, true) => C::SelectLeftWord,
            (true, false) => C::SelectLeft,
            (false, true) => C::MoveLeftWord,
            _ => C::MoveLeft,
        }),
        275 => Some(match (shift, ctrl) {
            (true, true) => C::SelectRightWord,
            (true, false) => C::SelectRight,
            (false, true) => C::MoveRightWord,
            _ => C::MoveRight,
        }),
        278 => Some(match (shift, ctrl) {
            (true, true) => C::SelectLeftDocument,
            (true, false) => C::SelectLeftLine,
            (false, true) => C::MoveLeftDocument,
            _ => C::MoveLeftLine,
        }),
        279 => Some(match (shift, ctrl) {
            (true, true) => C::SelectRightDocument,
            (true, false) => C::SelectRightLine,
            (false, true) => C::MoveRightDocument,
            _ => C::MoveRightLine,
        }),
        97 if ctrl => Some(C::SelectAll),
        99 if ctrl => Some(C::Copy),
        118 if ctrl => Some(C::Paste),
        120 if ctrl => Some(C::Cut),
        _ => None,
    };
    if let Some(code) = control {
        out.push(PlayerEvent::TextControl { code });
        return;
    }
    if let Some(codepoint) = char::from_u32(character).filter(|c| !c.is_control() && character != 0)
    {
        out.push(PlayerEvent::TextInput { codepoint });
        return;
    }
    if ctrl || alt {
        return;
    }
    if let Some(codepoint) = fallback_character(id, shift, modifiers & 32 != 0) {
        out.push(PlayerEvent::TextInput { codepoint });
    }
}
fn fallback_character(id: usize, shift: bool, caps: bool) -> Option<char> {
    match id {
        97..=122 => char::from_u32(id as u32 - if shift ^ caps { 32 } else { 0 }),
        32..=126 => {
            let ch = char::from_u32(id as u32)?;
            Some(if shift {
                match ch {
                    '1' => '!',
                    '2' => '@',
                    '3' => '#',
                    '4' => '$',
                    '5' => '%',
                    '6' => '^',
                    '7' => '&',
                    '8' => '*',
                    '9' => '(',
                    '0' => ')',
                    '-' => '_',
                    '=' => '+',
                    '[' => '{',
                    ']' => '}',
                    '\\' => '|',
                    ';' => ':',
                    '\'' => '"',
                    ',' => '<',
                    '.' => '>',
                    '/' => '?',
                    '`' => '~',
                    _ => ch,
                }
            } else {
                ch
            })
        }
        256..=265 => char::from_u32((id - 256 + 48) as u32),
        266 => Some('.'),
        267 => Some('/'),
        268 => Some('*'),
        269 => Some('-'),
        270 => Some('+'),
        272 => Some('='),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(events: &[PlayerEvent]) -> String {
        events
            .iter()
            .filter_map(|e| {
                if let PlayerEvent::TextInput { codepoint } = e {
                    Some(*codepoint)
                } else {
                    None
                }
            })
            .collect()
    }
    fn callback(down: bool, key: u32, character: u32) -> Callback {
        Callback {
            down,
            key,
            character,
            modifiers: 0,
        }
    }
    #[test]
    fn poll_only_keyboard_types_releases_and_repeats() {
        let mut k = Keyboard::default();
        let mut p = [false; 324];
        let pad = [false; 324];
        p[97] = true;
        assert_eq!(text(&k.update(&p, &pad, &[])), "a");
        for _ in 0..28 {
            assert_eq!(text(&k.update(&p, &pad, &[])), "");
        }
        assert_eq!(text(&k.update(&p, &pad, &[])), "a");
        p[97] = false;
        assert!(
            k.update(&p, &pad, &[])
                .iter()
                .any(|e| matches!(e, PlayerEvent::KeyUp { .. }))
        );
        p[304] = true;
        p[97] = true;
        assert_eq!(text(&k.update(&p, &pad, &[])), "A");
    }
    #[test]
    fn unicode_callbacks_do_not_duplicate_polled_text() {
        let mut k = Keyboard::default();
        let mut p = [false; 324];
        p[97] = true;
        assert_eq!(
            text(&k.update(&p, &[false; 324], &[callback(true, 97, 'á' as u32)])),
            "á"
        );
        for _ in 0..40 {
            assert_eq!(text(&k.update(&p, &[false; 324], &[])), "");
        }
    }
    #[test]
    fn callbacks_without_poll_or_characters_still_work_including_quick_taps() {
        let mut k = Keyboard::default();
        let e = k.update(
            &[false; 324],
            &[false; 324],
            &[callback(true, 98, 0), callback(false, 98, 0)],
        );
        assert_eq!(text(&e), "b");
        assert!(matches!(e.first(), Some(PlayerEvent::KeyDown { .. })));
        assert!(matches!(e.last(), Some(PlayerEvent::KeyUp { .. })));
    }
    #[test]
    fn text_editing_and_gamepad_sources_are_separate() {
        let mut k = Keyboard::default();
        let mut p = [false; 324];
        let mut pad = [false; 324];
        pad[97] = true;
        assert_eq!(text(&k.update(&p, &pad, &[])), "");
        p[8] = true;
        assert!(
            k.update(&p, &pad, &[])
                .iter()
                .any(|e| matches!(e, PlayerEvent::TextControl { code: C::Backspace }))
        );
    }
    #[test]
    fn caps_lock_and_control_shortcuts_do_not_insert_spurious_letters() {
        let mut k = Keyboard::default();
        let mut p = [false; 324];
        let pad = [false; 324];
        p[301] = true;
        p[97] = true;
        assert_eq!(text(&k.update(&p, &pad, &[])), "A");
        p.fill(false);
        k.update(&p, &pad, &[]);
        p[97] = true;
        p[304] = true;
        assert_eq!(text(&k.update(&p, &pad, &[])), "a");
        p.fill(false);
        k.update(&p, &pad, &[]);
        p[97] = true;
        p[306] = true;
        let e = k.update(&p, &pad, &[]);
        assert_eq!(text(&e), "");
        assert!(
            e.iter()
                .any(|e| matches!(e, PlayerEvent::TextControl { code: C::SelectAll }))
        );
    }
}
