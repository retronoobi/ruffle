//! Exercise the real State/player/audio/renderer across destruction and reload.
use super::*;
unsafe extern "C" fn low_quality_environment(command: u32, data: *mut c_void) -> bool {
    if command == 15 {
        let variable = unsafe { &mut *(data as *mut crate::Variable) };
        if !variable.key.is_null()
            && unsafe { std::ffi::CStr::from_ptr(variable.key) }.to_bytes() == b"ruffle_quality"
        {
            variable.value = c"low".as_ptr();
            return true;
        }
    }
    false
}
pub(super) unsafe fn savestate_roundtrip(
    path: &str,
    proc: unsafe extern "C" fn(*const c_char) -> *const c_void,
) {
    let movie = ruffle_core::tag_utils::movie_from_path(path, None).unwrap();
    let (w, h) = (
        movie.width().to_pixels() as u32,
        movie.height().to_pixels() as u32,
    );
    let root = test_output().join("state-roundtrip");
    std::fs::create_dir_all(&root).unwrap();
    let mut owner =
        unsafe { GlRenderer::new(|name| proc(CString::new(name).unwrap().as_ptr()), w, h) }
            .unwrap();
    let image = owner.texture(w, h, &[]);
    let texture = (&*image.0 as &dyn std::any::Any)
        .downcast_ref::<Texture>()
        .unwrap();
    let fb = unsafe { owner.gl.create_framebuffer() }.unwrap();
    unsafe {
        owner.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
        owner.gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture.id),
            0,
        );
    }
    let make = || {
        let mut s = crate::State {
            path: Some(path.into()),
            w,
            h,
            save: root.join("saves"),
            ..Default::default()
        };
        s.hw.proc = Some(proc);
        s
    };
    if let Ok(file) = std::env::var("RUFFLE_STATE_LOAD") {
        let bytes = std::fs::read(crate::offline::local_path(std::path::Path::new(&file)).unwrap())
            .unwrap();
        let mut tape = crate::savestate::Tape::decode(
            &bytes,
            &crate::savestate::hash(&std::fs::read(path).unwrap()),
        )
        .unwrap();
        if std::env::var_os("RUFFLE_STATE_FAST_REPLAY").is_some() {
            let count = tape.frames.len();
            for (i, frame) in tape.frames.iter_mut().enumerate() {
                frame.render = i + 1 == count;
            }
        }
        let mut state = make();
        state.restoring = Some(crate::savestate::Restore::new(tape));
        while state.restoring.is_some() {
            state.restore_chunk(fb.0.get() as usize);
        }
        assert!(state.player.is_some(), "state reconstruction failed");
        let checkpoint = state.frames;
        if let Ok(quality) = std::env::var("RUFFLE_STATE_QUALITY") {
            state
                .player
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .set_quality(quality.parse().ok().expect("invalid test quality"));
        }
        let save_png = |name: &str, bytes: Vec<u8>| {
            let mut encoder =
                png::Encoder::new(std::fs::File::create(root.join(name)).unwrap(), w, h);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&bytes)
                .unwrap();
        };
        save_png("loaded.png", owner.read_texture(texture).unwrap());
        let frames = std::env::var("RUFFLE_STATE_ANALYZE_FRAMES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(600usize);
        let mut pcm = vec![];
        let mut mixed = vec![];
        let mut timings = String::from(
            "frame,core_step_ms,tick_ms,render_ms,audio_ms,draws,groups,new_textures,readbacks,cpu_filter_us,group_pixels,target_allocations,target_us,texture_us,tessellation_us,msaa_samples\n",
        );
        let mut input = crate::savestate::Input {
            mouse: [state.mouse.0, state.mouse.1],
            ..Default::default()
        };
        for frame in 0..frames {
            input.callbacks = std::mem::take(&mut *crate::EVENTS.lock().unwrap());
            let started = std::time::Instant::now();
            pcm.extend(state.step(&input, true, 1., fb.0.get() as usize, false));
            timings.push_str(&format!(
                "{frame},{:.3},{:.3},{:.3},{:.3},{},{},{},{},{},{},{},{},{},{},{}\n",
                started.elapsed().as_secs_f64() * 1000.,
                state.metrics.tick,
                state.metrics.render,
                state.metrics.audio,
                state.metrics.graphics.draws,
                state.metrics.graphics.groups,
                state.metrics.graphics.textures,
                state.metrics.graphics.readbacks,
                state.metrics.graphics.cpu_filter_us,
                state.metrics.graphics.group_pixels,
                state.metrics.graphics.target_allocations,
                state.metrics.graphics.target_us,
                state.metrics.graphics.texture_us,
                state.metrics.graphics.tessellation_us,
                state.metrics.graphics.samples
            ));
            mixed.extend_from_slice(&state.audio_float);
        }
        let data = bytemuck::cast_slice::<i16, u8>(&pcm);
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend((36 + data.len() as u32).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(48000u32.to_le_bytes());
        wav.extend(192000u32.to_le_bytes());
        wav.extend(4u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend((data.len() as u32).to_le_bytes());
        wav.extend(data);
        std::fs::write(root.join("continued.wav"), wav).unwrap();
        std::fs::write(
            root.join("mixed.f32"),
            bytemuck::cast_slice::<f32, u8>(&mixed),
        )
        .unwrap();
        std::fs::write(root.join("timings.csv"), timings).unwrap();
        save_png("continued.png", owner.read_texture(texture).unwrap());
        let peak = mixed.iter().fold(0f32, |a, b| a.max(b.abs()));
        let over = mixed.iter().filter(|x| x.abs() > 1.).count();
        std::fs::write(root.join("analysis.txt"),format!("state={file}\ncheckpoint_frame={checkpoint}\nanalyzed_frames={frames}\nmixed_peak={peak}\nover_range_samples={over}\noutput_pcm_peak={}\n",pcm.iter().map(|x|(*x as i32).abs()).max().unwrap_or(0))).unwrap();
        drop(state);
        unsafe {
            owner.gl.delete_framebuffer(fb);
        }
        return;
    }
    let frames = std::env::var("RUFFLE_STATE_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2400usize);
    let actions: serde_json::Value = std::env::var("RUFFLE_STATE_ACTIONS")
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(serde_json::json!([]));
    let mut input = crate::savestate::Input {
        mouse: [w as f64 / 2., h as f64 / 2.],
        ..Default::default()
    };
    let mut inputs = vec![];
    for frame in 0..frames + 120 {
        input.buttons = 0;
        input.physical.clear();
        if let Some(actions) = actions.as_array() {
            for event in actions {
                let at = event["frame"].as_u64().unwrap_or(0) as usize;
                if let Some(key) = event["polled_key"]
                    .as_u64()
                    .or_else(|| event["key"].as_u64())
                {
                    if frame >= at && frame < at + event["duration"].as_u64().unwrap_or(1) as usize
                    {
                        input.physical.push(key as u16);
                    }
                    continue;
                }
                if event["x"].is_number() {
                    if frame + 3 == at || frame == at {
                        input.mouse = [
                            event["x"].as_f64().unwrap() * w as f64,
                            event["y"].as_f64().unwrap() * h as f64,
                        ];
                    }
                    if frame == at {
                        input.buttons = 1;
                    }
                }
            }
        }
        inputs.push(input.clone());
    }

    let mut state = make();
    state.create_player().unwrap();
    let resume_low = std::env::var_os("RUFFLE_STATE_RESUME_LOW").is_some();
    if resume_low {
        state
            .journal
            .as_mut()
            .unwrap()
            .initial
            .options
            .insert("ruffle_quality".into(), "high".into());
    }
    let skip = std::env::var_os("RUFFLE_STATE_SKIP_PATTERN").is_some();
    for (n, i) in inputs[..frames].iter().enumerate() {
        let render = !skip || n % 7 != 0;
        if std::env::var_os("RUFFLE_STATE_QUALITY_SWITCH").is_some() && n == frames / 2 {
            state.pending_quality = Some(StageQuality::Low);
        }
        state.step(i, render, 1., fb.0.get() as usize, false);
        state.record_frame(i.clone(), render, 1.);
    }
    let pixels_saved = owner.read_texture(texture).unwrap();
    crate::with_state(|global| *global.borrow_mut() = state);
    let mut bytes = vec![0u8; crate::retro_serialize_size()];
    assert!(unsafe { crate::retro_serialize(bytes.as_mut_ptr() as *mut c_void, bytes.len()) });
    state = crate::with_state(|global| std::mem::take(&mut *global.borrow_mut()));
    let used = 52 + u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    std::fs::write(root.join("checkpoint.ruffle-state"), &bytes[..used]).unwrap();
    let mut audio_expected = vec![];
    if resume_low {
        state.pending_quality = Some(StageQuality::Low);
    }
    for (n, i) in inputs[frames..].iter().enumerate() {
        audio_expected.extend(state.step(
            i,
            !skip || !(n + frames).is_multiple_of(7),
            1.,
            fb.0.get() as usize,
            false,
        ));
    }
    let pixels_expected = owner.read_texture(texture).unwrap();
    drop(state); // Prove the file contains everything needed, not a live pointer.
    crate::with_state(|global| {
        let mut next = make();
        if resume_low {
            next.env = Some(low_quality_environment);
        }
        *global.borrow_mut() = next;
    });
    assert!(unsafe { crate::retro_unserialize(bytes.as_ptr() as *const c_void, bytes.len()) });
    let mut restored = crate::with_state(|global| std::mem::take(&mut *global.borrow_mut()));
    while restored.restoring.is_some() {
        restored.restore_chunk(fb.0.get() as usize);
    }
    assert!(restored.player.is_some(), "reconstruction failed");
    assert_eq!(restored.frames as usize, frames);
    if resume_low {
        assert_eq!(
            restored.pending_quality,
            Some(StageQuality::Low),
            "current frontend quality must survive loading a high-quality state"
        );
    }
    let pixels_restored = owner.read_texture(texture).unwrap();
    assert_eq!(
        crate::savestate::hash(&pixels_saved),
        crate::savestate::hash(&pixels_restored),
        "restored picture differs"
    );
    // These tests use poll-only input. Callback release reconciliation is recorded
    // by the next real capture_input call in the frontend.
    let mut audio_restored = vec![];
    for (n, i) in inputs[frames..].iter().enumerate() {
        audio_restored.extend(restored.step(
            i,
            !skip || !(n + frames).is_multiple_of(7),
            1.,
            fb.0.get() as usize,
            false,
        ));
    }
    assert_eq!(
        crate::savestate::hash(bytemuck::cast_slice::<i16, u8>(&audio_expected)),
        crate::savestate::hash(bytemuck::cast_slice::<i16, u8>(&audio_restored)),
        "restored PCM differs"
    );
    assert_eq!(
        crate::savestate::hash(&pixels_expected),
        crate::savestate::hash(&owner.read_texture(texture).unwrap()),
        "continued picture differs"
    );
    std::fs::write(root.join("result.txt"),format!("game={path}\ncheckpoint_frame={frames}\nstate_bytes={}\nrestored_pixels=exact\ncontinued_pixels=exact\ncontinued_pcm_120_frames=exact\ncontinued_nonzero_samples={}\n",bytes.len(),audio_expected.iter().filter(|x|**x!=0).count())).unwrap();
    drop(restored);
    unsafe {
        owner.gl.delete_framebuffer(fb);
    }
}
