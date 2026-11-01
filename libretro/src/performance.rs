use crate::gl::FrameStats;
#[derive(Default)]
pub struct Frontend {
    frames: u32,
    core: f64,
    video: f64,
    audio: f64,
    peak: f64,
}
impl Frontend {
    pub fn record(&mut self, core: f64, video: f64, audio: f64) -> Option<String> {
        self.frames += 1;
        self.core += core;
        self.video += video;
        self.audio += audio;
        self.peak = self.peak.max(core + video + audio);
        if self.frames < 300 {
            return None;
        }
        let n = self.frames as f64;
        let message = format!(
            "frontend 300 frames: core={:.2}ms video_callback={:.2}ms audio_callback={:.2}ms peak_total={:.2}ms",
            self.core / n,
            self.video / n,
            self.audio / n,
            self.peak
        );
        *self = Self::default();
        Some(message)
    }
}
#[derive(Default, Clone, Copy)]
pub struct Metrics {
    pub tick: f64,
    pub render: f64,
    pub audio: f64,
    pub graphics: FrameStats,
}
#[derive(Default)]
pub struct Window {
    frames: u32,
    tick: f64,
    render: f64,
    peak: f64,
    draws: u64,
    upload: u64,
    textures: u64,
    groups: u64,
    filters: u64,
    targets: u64,
    target_us: u64,
    texture_us: u64,
    samples: u32,
}
impl Window {
    pub fn record(&mut self, tick: f64, render: f64, stats: FrameStats) -> Option<String> {
        self.frames += 1;
        self.tick += tick;
        self.render += render;
        self.peak = self.peak.max(tick + render);
        self.draws += stats.draws;
        self.upload += stats.uploaded_bytes;
        self.textures += stats.textures;
        self.groups += stats.groups;
        self.filters += stats.cpu_filter_us;
        self.targets += stats.target_allocations;
        self.target_us += stats.target_us;
        self.texture_us += stats.texture_us;
        self.samples = stats.samples;
        if self.frames < 300 {
            return None;
        }
        let n = self.frames as f64;
        let message = format!(
            "perf 300 frames: tick={:.2}ms render={:.2}ms peak_tick_render={:.2}ms draws={:.0} upload={:.1}KB new_textures={:.2} groups={:.1} cpu_filters={:.2}ms new_targets={:.2} target_alloc={:.2}ms texture_alloc={:.2}ms aa_samples={} (averages except peak)",
            self.tick / n,
            self.render / n,
            self.peak,
            self.draws as f64 / n,
            self.upload as f64 / n / 1024.,
            self.textures as f64 / n,
            self.groups as f64 / n,
            self.filters as f64 / n / 1000.,
            self.targets as f64 / n,
            self.target_us as f64 / n / 1000.,
            self.texture_us as f64 / n / 1000.,
            self.samples
        );
        *self = Self::default();
        Some(message)
    }
}
