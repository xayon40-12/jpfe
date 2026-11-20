//! What happened while the app started, and how long it took, for the debug information: storage reads and
//! writes (with their sizes, and the time the main thread spent copying them), books decoded or parsed,
//! dictionary files unpacked, the dictionary loaded, and the longest frame with the steps that ran in it.
//!
//! Startup is slow in some browsers only (Safari froze for seconds where Chrome did not), so the numbers are
//! shown in the app itself, where they can be read off in any browser.

use std::cell::RefCell;

use web_time::Instant;

/// Events kept at most, so a long session does not grow the list without end.
const MAX_EVENTS: usize = 200;
/// Steps of a frame shorter than this are not listed in the longest frame.
const MIN_STEP_MS: f64 = 2.0;

/// The longest frame: when it started (seconds), its time (ms) and its steps.
struct LongestFrame {
    at: f64,
    took: f64,
    steps: Vec<(&'static str, f64)>,
}

struct Timeline {
    started: Instant,
    /// Seconds since the start, and what happened
    events: Vec<(f64, String)>,
    /// Steps of the frame in progress, with their time in milliseconds
    frame: Vec<(&'static str, f64)>,
    frame_started: Option<Instant>,
    longest_frame: Option<LongestFrame>,
    /// The longest pause between the end of a frame and the start of the next one while the app was still loading
    /// (pauses while it waits for input are not counted): when, and how long
    longest_pause: Option<(f64, f64)>,
    /// When the last frame ended, if the app was still loading then
    last_frame_end: Option<Instant>,
}

thread_local! {
    static TIMELINE: RefCell<Timeline> = RefCell::new(Timeline {
        started: Instant::now(),
        events: Vec::new(),
        frame: Vec::new(),
        frame_started: None,
        longest_frame: None,
        longest_pause: None,
        last_frame_end: None,
    });
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

/// Note something that happened, now.
pub fn note(text: impl Into<String>) {
    TIMELINE.with_borrow_mut(|t| {
        if t.events.len() < MAX_EVENTS {
            let at = t.started.elapsed().as_secs_f64();
            t.events.push((at, text.into()));
        }
    });
}

/// Run `work` as a step of the current frame, listed if the frame turns out to be the longest.
pub fn step<T>(name: &'static str, work: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = work();
    let took = ms(start);
    if took >= MIN_STEP_MS {
        TIMELINE.with_borrow_mut(|t| t.frame.push((name, took)));
    }
    result
}

pub fn frame_started() {
    TIMELINE.with_borrow_mut(|t| {
        let now = Instant::now();
        if let Some(end) = t.last_frame_end {
            let pause = now.duration_since(end).as_secs_f64() * 1000.0;
            if t.longest_pause.is_none_or(|(_, longest)| pause > longest) {
                t.longest_pause = Some((end.duration_since(t.started).as_secs_f64(), pause));
            }
        }
        t.frame_started = Some(now);
        t.frame.clear();
    });
}

/// End of a frame; `loading` while books or the dictionary are still being loaded.
pub fn frame_ended(loading: bool) {
    TIMELINE.with_borrow_mut(|t| {
        let Some(start) = t.frame_started.take() else { return };
        let took = ms(start);
        if t.longest_frame.as_ref().is_none_or(|longest| took > longest.took) {
            let mut steps = std::mem::take(&mut t.frame);
            // The rest of the frame: drawing, and other work
            let rest = took - steps.iter().map(|(_, ms)| ms).sum::<f64>();
            if rest >= MIN_STEP_MS {
                steps.push(("drawing and other", rest));
            }
            t.longest_frame = Some(LongestFrame { at: start.duration_since(t.started).as_secs_f64(), took, steps });
        }
        t.last_frame_end = loading.then(Instant::now);
    });
}

/// The timeline as lines of text: the longest frame and pause, then the events.
pub fn lines() -> Vec<String> {
    TIMELINE.with_borrow(|t| {
        let mut lines = Vec::new();
        if let Some(frame) = &t.longest_frame {
            let steps: Vec<String> = frame.steps.iter().map(|(name, ms)| format!("{name} {ms:.0}")).collect();
            lines.push(format!("Longest frame: {:.0} ms at {:.2} s ({})", frame.took, frame.at, steps.join(", ")));
        }
        if let Some((at, pause)) = t.longest_pause {
            lines.push(format!("Longest pause between frames while loading: {pause:.0} ms at {at:.2} s"));
        }
        lines.extend(t.events.iter().map(|(at, text)| format!("{at:6.2} s  {text}")));
        lines
    })
}

/// A size in bytes, short.
pub fn size(bytes: usize) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.0} kB", bytes as f64 / 1e3),
        _ => format!("{:.1} MB", bytes as f64 / 1e6),
    }
}

/// Milliseconds since `since`, for the notes.
pub fn since(since: Instant) -> f64 {
    ms(since)
}
