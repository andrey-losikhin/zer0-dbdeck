use std::{cell::RefCell, rc::Rc};

use relm4::gtk::{self, prelude::*};

const HEIGHT: i32 = 3;
const CYCLE_US: i64 = 1_800_000;
const TRACK_ALPHA: f64 = 0.2;

/// Thin indeterminate loading bar animated on the frame clock, so motion
/// stays smooth regardless of how busy the event-polling timer is.
pub fn new() -> gtk::DrawingArea {
    let bar = gtk::DrawingArea::new();
    bar.set_content_height(HEIGHT);
    bar.set_hexpand(true);
    bar.set_visible(false);
    // libadwaita `.accent` sets `color: @accent_color`, which follows the theme.
    bar.add_css_class("accent");

    let started_at = Rc::new(RefCell::new(None::<i64>));
    let draw_start = started_at.clone();
    bar.set_draw_func(move |bar, cr, width, height| {
        let now = bar.frame_clock().map_or(0, |clock| clock.frame_time());
        let start = *draw_start.borrow_mut().get_or_insert(now);
        let phase = ((now - start).rem_euclid(CYCLE_US)) as f64 / CYCLE_US as f64;
        #[allow(deprecated)]
        let color = bar.style_context().color();
        let (r, g, b) = (
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
        );
        let (w, h) = (f64::from(width), f64::from(height));
        cr.set_source_rgba(r, g, b, TRACK_ALPHA);
        cr.rectangle(0.0, 0.0, w, h);
        let _ = cr.fill();
        cr.set_source_rgba(r, g, b, 1.0);
        for (tail, head) in segments(phase) {
            if head > tail {
                rounded_bar(cr, tail * w, (head - tail) * w, h);
            }
        }
        let _ = cr.fill();
    });

    let tick = Rc::new(RefCell::new(None::<gtk::TickCallbackId>));
    let map_tick = tick.clone();
    bar.connect_map(move |bar| {
        *started_at.borrow_mut() = None;
        let id = bar.add_tick_callback(|bar, _| {
            bar.queue_draw();
            gtk::glib::ControlFlow::Continue
        });
        if let Some(old) = map_tick.borrow_mut().replace(id) {
            old.remove();
        }
    });
    bar.connect_unmap(move |_| {
        if let Some(id) = tick.borrow_mut().take() {
            id.remove();
        }
    });
    bar
}

fn rounded_bar(cr: &gtk::cairo::Context, x: f64, width: f64, height: f64) {
    let radius = (height / 2.0).min(width / 2.0);
    let (left, right) = (x + radius, x + width - radius);
    let middle = height / 2.0;
    cr.new_sub_path();
    cr.arc(
        right,
        middle,
        radius,
        -std::f64::consts::FRAC_PI_2,
        std::f64::consts::FRAC_PI_2,
    );
    cr.arc(
        left,
        middle,
        radius,
        std::f64::consts::FRAC_PI_2,
        3.0 * std::f64::consts::FRAC_PI_2,
    );
    cr.close_path();
}

fn ease_in_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn span(phase: f64, start: f64, duration: f64) -> f64 {
    ease_in_out((phase - start) / duration)
}

/// Two segments per cycle as (tail, head) fractions of the width: a long one
/// that stretches and shrinks, then a shorter one chasing it.
fn segments(phase: f64) -> [(f64, f64); 2] {
    [
        (span(phase, 0.15, 0.5), span(phase, 0.0, 0.5)),
        (span(phase, 0.65, 0.35), span(phase, 0.5, 0.35)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_stay_within_bounds_and_ordered() {
        for step in 0..=1000 {
            let phase = f64::from(step) / 1000.0;
            for (tail, head) in segments(phase) {
                assert!((0.0..=1.0).contains(&tail));
                assert!((0.0..=1.0).contains(&head));
                assert!(tail <= head + f64::EPSILON);
            }
        }
    }

    #[test]
    fn cycle_starts_and_ends_empty_so_loop_is_seamless() {
        for phase in [0.0, 1.0] {
            for (tail, head) in segments(phase) {
                assert!((head - tail).abs() < 1e-9);
            }
        }
    }
}
