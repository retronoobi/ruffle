use crate::*;
impl State {
    pub(super) fn capture_input(&mut self) -> savestate::Input {
        let mut result = savestate::Input {
            mouse: [self.mouse.0, self.mouse.1],
            ..Default::default()
        };
        let Some(input) = self.input else {
            return result;
        };
        let read = |device, index, id| unsafe { input(0, device, index, id) };
        result.mouse[0] = (result.mouse[0]
            + read(2, 0, 0) as f64 * self.sensitivity
            + read(5, 1, 0) as f64 / 32768. * 6.)
            .clamp(0., self.w as f64 - 1.);
        result.mouse[1] = (result.mouse[1]
            + read(2, 0, 1) as f64 * self.sensitivity
            + read(5, 1, 1) as f64 / 32768. * 6.)
            .clamp(0., self.h as f64 - 1.);
        if self.pointer {
            let (x, y) = (read(6, 0, 0), read(6, 0, 1));
            if x != i16::MIN && y != i16::MIN {
                result.mouse = [
                    (x as f64 + 32767.) / 65534. * (self.w - 1) as f64,
                    (y as f64 + 32767.) / 65534. * (self.h - 1) as f64,
                ];
            }
        }
        for (i, (id, pad)) in [(2, 12), (3, 13), (6, 14)].into_iter().enumerate() {
            if read(2, 0, id) != 0 || read(1, 0, pad) != 0 {
                result.buttons |= 1 << i;
            }
        }
        result.wheel = read(2, 0, 4).saturating_sub(read(2, 0, 5));
        for id in 0..324 {
            if read(3, 0, id) != 0 {
                result.physical.push(id as u16);
            }
        }
        let mut keys = [false; 324];
        for (pad, key) in self.pad_keys.iter().enumerate() {
            if *key > 0 && *key < 324 {
                keys[*key as usize] |= read(1, 0, pad as u32) != 0;
            }
        }
        let (ax, ay) = (read(5, 0, 0), read(5, 0, 1));
        for (pad, down) in [
            (6, ax < -16000),
            (7, ax > 16000),
            (4, ay < -16000),
            (5, ay > 16000),
        ] {
            let key = self.pad_keys[pad] as usize;
            if key > 0 && key < 324 {
                keys[key] |= down;
            }
        }
        result.pad = keys
            .iter()
            .enumerate()
            .filter_map(|(i, down)| down.then_some(i as u16))
            .collect();
        result.callbacks = std::mem::take(&mut *EVENTS.lock().unwrap());
        result
    }
    pub(super) fn apply_input(&mut self, input: &savestate::Input) {
        self.mouse = (input.mouse[0], input.mouse[1]);
        let (x, y) = self.mouse;
        let Some(player) = self.player.as_ref() else {
            return;
        };
        let mut player = player.lock().unwrap();
        player.handle_event(PlayerEvent::MouseMove { x, y });
        for (i, button) in [MouseButton::Left, MouseButton::Right, MouseButton::Middle]
            .into_iter()
            .enumerate()
        {
            let down = input.buttons & (1 << i) != 0;
            if down != self.buttons[i] {
                self.buttons[i] = down;
                player.handle_event(if down {
                    PlayerEvent::MouseDown {
                        x,
                        y,
                        button,
                        index: None,
                    }
                } else {
                    PlayerEvent::MouseUp { x, y, button }
                });
            }
        }
        if input.wheel != 0 {
            player.handle_event(PlayerEvent::MouseWheel {
                delta: MouseWheelDelta::Lines(input.wheel as f64),
            });
        }
        let mut physical = [false; 324];
        let mut pad = [false; 324];
        for id in &input.physical {
            physical[*id as usize] = true;
        }
        for id in &input.pad {
            pad[*id as usize] = true;
        }
        for event in self.keyboard.update(&physical, &pad, &input.callbacks) {
            player.handle_event(event);
        }
    }
}
