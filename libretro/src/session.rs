use crate::*;
impl State {
    pub(super) fn record_frame(&mut self, input: savestate::Input, rendered: bool, volume: f32) {
        let quality = self.applied_quality.take();
        if let Some(journal) = &mut self.journal {
            journal.record(input, rendered, volume);
            if let Some(q) = quality {
                journal.frames.last_mut().unwrap().quality = Some(q.to_string());
            }
        }
    }
    pub(super) fn step(
        &mut self,
        input: &savestate::Input,
        rendered: bool,
        volume: f32,
        fb: usize,
        replaying: bool,
    ) -> Vec<i16> {
        self.metrics = performance::Metrics::default();
        self.applied_quality = None;
        if !replaying
            && let Some(quality) = self.pending_quality.take()
            && let Some(player) = &self.player
        {
            player.lock().unwrap().set_quality(quality);
            self.applied_quality = Some(quality);
            log(format!("Quality changed to {quality}"));
        }
        if let Some(clock) = &self.clock {
            clock.frame(self.frames);
        }
        if let Some(player) = &self.player {
            let mut p = player.lock().unwrap();
            p.set_deterministic_time(Some((self.frames * 1000 / 60) as u32));
            if let Some(r) =
                (p.renderer_mut() as &mut dyn std::any::Any).downcast_mut::<gl::GlRenderer>()
            {
                r.target(fb);
            }
        }
        self.executor.run();
        if let Some(player) = &self.player {
            player
                .lock()
                .unwrap()
                .preload(&mut ruffle_core::limits::ExecutionLimit::none());
        }
        self.apply_input(input);
        if let Some(player) = self.player.as_ref() {
            let mut p = player.lock().unwrap();
            let started = std::time::Instant::now();
            p.tick(FloatDuration::from_millis(1000. / 60.));
            let tick = started.elapsed().as_secs_f64() * 1000.;
            let started = std::time::Instant::now();
            if rendered {
                p.render();
            }
            let render = started.elapsed().as_secs_f64() * 1000.;
            self.metrics.tick = tick;
            self.metrics.render = render;
            if rendered
                && self.cursor
                && p.ui().mouse_visible()
                && let Some(r) =
                    (p.renderer_mut() as &mut dyn std::any::Any).downcast_mut::<gl::GlRenderer>()
            {
                r.draw_cursor(self.mouse.0, self.mouse.1, self.cursor_size);
            }
            if let Some(r) =
                (p.renderer_mut() as &mut dyn std::any::Any).downcast_mut::<gl::GlRenderer>()
            {
                let stats = r.take_stats();
                self.metrics.graphics = stats;
                if !replaying && let Some(message) = self.performance.record(tick, render, stats) {
                    log(message);
                }
            }
        }
        let started = std::time::Instant::now();
        if let Some(mixer) = &self.mixer {
            mixer.mix(&mut self.audio_float);
        } else {
            self.audio_float.fill(0.);
        }
        let samples = self.audio_output.convert(&self.audio_float, volume);
        {
            use sha2::Digest;
            self.audio_signature
                .update(bytemuck::cast_slice::<f32, u8>(&self.audio_float));
        }
        if !replaying
            && let Some(message) = self
                .audio_output
                .report(started.elapsed().as_secs_f64() * 1000.)
        {
            log(message);
        }
        self.metrics.audio = started.elapsed().as_secs_f64() * 1000.;
        self.frames += 1;
        samples
    }
    pub(super) fn restore_chunk(&mut self, fb: usize) -> (Vec<i16>, bool) {
        let mut restore = self.restoring.take().unwrap();
        if !restore.started {
            self.initial_override = Some(restore.tape.initial.clone());
            if let Err(error) = self.create_player() {
                self.option_override = None;
                log(format!("State restore failed: {error}"));
                return (vec![0; 1600], false);
            }
            restore.started = true;
            log(format!(
                "Reconstructing savestate: {} frames",
                restore.tape.frames.len()
            ));
        }
        let started = std::time::Instant::now();
        let mut rendered = false;
        for _ in 0..240 {
            if restore.position >= restore.tape.frames.len() {
                break;
            }
            let frame = &restore.tape.frames[restore.position];
            if let Some(input) = &frame.input {
                restore.input = input.clone();
            }
            if let Some(quality) = &frame.quality {
                let quality = quality.parse().unwrap_or(StageQuality::High);
                self.quality_option = quality;
                if let Some(player) = &self.player {
                    player.lock().unwrap().set_quality(quality);
                }
            }
            self.step(&restore.input, frame.render, frame.volume, fb, true);
            rendered |= frame.render;
            restore.position += 1;
            if started.elapsed().as_millis() >= 10 {
                break;
            }
        }
        if restore.position == restore.tape.frames.len() {
            if let Some(expected) = &restore.tape.audio_signature {
                use sha2::Digest;
                let actual: String = self
                    .audio_signature
                    .clone()
                    .finalize()
                    .iter()
                    .map(|x| format!("{x:02x}"))
                    .collect();
                if expected != &actual {
                    log("Savestate reconstruction diverged: mixed audio signature mismatch");
                    self.message("Ruffle: estado incompatível (execução divergente)", 300);
                    self.player = None;
                    return (vec![0; 1600], false);
                }
            }
            if let Some(storage) = &self.storage {
                if let Err(error) = storage.initial() {
                    log(format!("State reconstruction diverged: {error}"));
                    self.player = None;
                    return (vec![0; 1600], false);
                }
                storage.finish_replay();
            }
            if let Some(quality) = restore.resume_quality.and_then(|q| q.parse().ok())
                && quality != self.quality_option
            {
                self.quality_option = quality;
                self.pending_quality = Some(quality);
            }
            restore.tape.resume();
            self.journal = Some(restore.tape);
            self.pacing = pacing::Pacing::default();
            let mut events = EVENTS.lock().unwrap();
            events.clear();
            events.extend(self.keyboard.release_callback_events());
            drop(events);
            log(format!("Savestate restored at frame {}", self.frames));
            self.message("Ruffle: estado restaurado", 180);
        } else {
            let progress = restore.position * 100 / restore.tape.frames.len().max(1);
            if progress as u8 != restore.last_progress {
                restore.last_progress = progress as u8;
                self.message(&format!("Ruffle: restaurando estado... {progress}%"), 60);
            }
            self.restoring = Some(restore);
        }
        (vec![0; 1600], rendered)
    }
    pub(super) fn message(&self, text: &str, frames: u32) {
        #[repr(C)]
        struct Message {
            text: *const c_char,
            frames: u32,
        }
        let text = CString::new(text).unwrap();
        self.environment(
            6,
            &mut Message {
                text: text.as_ptr(),
                frames,
            },
        );
    }
}
