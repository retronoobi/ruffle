//! Keep Flash ticks/audio at 60 Hz when rendering temporarily exceeds budget.
//! No wall-clock time is added to the simulation and no audio is discarded.
#[derive(Default)]
pub struct Pacing {
    debt_ms: f64,
    consecutive: u8,
    skipped: u32,
    frames: u32,
}
impl Pacing {
    pub fn render(&mut self, automatic: bool) -> bool {
        if !automatic {
            self.debt_ms = 0.;
            self.consecutive = 0;
            return true;
        }
        if self.debt_ms > 1. && self.consecutive < 2 {
            self.consecutive += 1;
            false
        } else {
            self.consecutive = 0;
            true
        }
    }
    pub fn finish(&mut self, cost_ms: f64, rendered: bool) -> Option<String> {
        self.debt_ms = (self.debt_ms + cost_ms - 1000. / 60.).clamp(-1000. / 60., 1000. / 30.);
        self.frames += 1;
        self.skipped += u32::from(!rendered);
        if self.frames < 300 {
            return None;
        }
        let message = if self.skipped > 0 {
            Some(format!(
                "render pacing 300 frames: repeated_video_frames={} (Flash ticks and audio continue)",
                self.skipped
            ))
        } else {
            None
        };
        self.frames = 0;
        self.skipped = 0;
        message
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expensive_render_is_followed_by_recovery_without_skipping_simulation() {
        let mut p = Pacing::default();
        assert!(p.render(true));
        p.finish(25., true);
        assert!(!p.render(true));
        p.finish(4., false);
        assert!(p.render(true));
        for _ in 0..120 {
            p.finish(8., true);
            assert!(p.render(true));
        }
    }
    #[test]
    fn sustained_load_reduces_graphical_work_to_the_available_budget() {
        let mut p = Pacing::default();
        let mut draws = 0;
        let mut total = 0.;
        for _ in 0..600 {
            let render = p.render(true);
            let cost = if render { 24. } else { 4. };
            draws += usize::from(render);
            total += cost;
            p.finish(cost, render);
        }
        assert!((370..=390).contains(&draws));
        assert!(total / 600. < 16.8);
    }
    #[test]
    fn never_hides_video_for_more_than_two_frames_and_can_be_disabled() {
        let mut p = Pacing::default();
        p.finish(200., true);
        assert!(!p.render(true));
        p.finish(80., false);
        assert!(!p.render(true));
        p.finish(80., false);
        assert!(p.render(true));
        assert!(p.render(false));
        assert!(p.render(true));
    }
}
