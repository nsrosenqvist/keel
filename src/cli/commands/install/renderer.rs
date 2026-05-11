//! Line-redraw install renderer.
//!
//! Not a TUI — this stays in normal terminal mode and updates the
//! step list by moving the cursor up and clearing lines in place.
//! Output looks like:
//!
//! ```text
//! Installing keel steps for myapp
//!
//!   ✓ copy-env                          0.1s
//!   ✓ composer install                 12.3s
//!   ◐ migrate-fresh                     2.1s
//!     Running migrations...
//!     > 2024_01_01_create_users
//!   ○ seed-demo-data
//!   ○ install-hooks
//! ```
//!
//! The tail-output region under the running step holds the last few
//! captured lines so the user has something to watch during long
//! steps. When a step finishes, the row collapses to a single line
//! and the next step takes over.
//!
//! For interactive steps the renderer detaches: it clears the dynamic
//! region, prints a "passing through" notice, and hands the terminal
//! to the child. When the child exits, the renderer redraws the full
//! list with the step's new status.

use crate::cli::commands::install::plan::Step;
use crossterm::{
    cursor::{MoveToColumn, MoveUp},
    style::{ResetColor, Stylize},
    terminal::{Clear, ClearType},
};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::Duration;

/// How many tail lines to show under the active step.
const TAIL_LINES: usize = 3;
/// How many lines to retain for the failure summary.
const RING_LINES: usize = 30;

/// Animated braille spinner frames.
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pending,
    Running,
    Ok,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    Ok,
    Failed,
    Skipped,
}

impl StepOutcome {
    fn to_status(self) -> Status {
        match self {
            StepOutcome::Ok => Status::Ok,
            StepOutcome::Failed => Status::Failed,
            StepOutcome::Skipped => Status::Skipped,
        }
    }
}

struct StepRow {
    label: String,
    status: Status,
    duration: Option<Duration>,
    tail: VecDeque<String>,
}

pub struct Renderer {
    rows: Vec<StepRow>,
    /// Number of dynamic lines printed in the last full redraw.
    /// `MoveUp(last_line_count)` + redraw is how we update in place.
    last_line_count: u16,
    /// Tick counter advanced on every spinner-driven redraw; chooses
    /// the active braille frame.
    spinner_tick: usize,
    /// Ring buffer of the most recent captured lines, fed alongside
    /// `tail`. Used to render a failure summary.
    ring: VecDeque<String>,
    /// True when the renderer is paused for an interactive step. While
    /// paused, `append_tail` is a no-op and the dynamic region stays
    /// blank so the child has the terminal to itself.
    paused: bool,
}

impl Renderer {
    /// Build a renderer for `steps` and print the header + initial
    /// "pending" rows. Subsequent `begin` / `end` / `append_tail` /
    /// `tick` calls update the rows in place.
    pub fn new(project_name: &str, steps: &[Step]) -> io::Result<Self> {
        let rows = steps
            .iter()
            .map(|s| StepRow {
                label: s.label().to_string(),
                status: Status::Pending,
                duration: None,
                tail: VecDeque::with_capacity(TAIL_LINES),
            })
            .collect();
        let mut renderer = Self {
            rows,
            last_line_count: 0,
            spinner_tick: 0,
            ring: VecDeque::with_capacity(RING_LINES),
            paused: false,
        };
        renderer.print_header(project_name)?;
        renderer.redraw_full()?;
        Ok(renderer)
    }

    /// Mark a step as running and redraw.
    pub fn begin(&mut self, idx: usize) -> io::Result<()> {
        if let Some(row) = self.rows.get_mut(idx) {
            row.status = Status::Running;
            row.tail.clear();
        }
        self.ring.clear();
        self.redraw_full()
    }

    /// Mark a step as finished with `outcome` and redraw.
    pub fn end(&mut self, idx: usize, outcome: StepOutcome, duration: Duration) -> io::Result<()> {
        if let Some(row) = self.rows.get_mut(idx) {
            row.status = outcome.to_status();
            row.duration = Some(duration);
            row.tail.clear();
        }
        self.redraw_full()
    }

    /// Push a tail line for the currently-running step (and the ring
    /// buffer). Cheap; the actual redraw is deferred to `tick`.
    pub fn append_tail(&mut self, line: &str) {
        if self.paused {
            return;
        }
        if self.ring.len() >= RING_LINES {
            self.ring.pop_front();
        }
        self.ring.push_back(line.to_string());

        if let Some(row) = self.rows.iter_mut().find(|r| r.status == Status::Running) {
            if row.tail.len() >= TAIL_LINES {
                row.tail.pop_front();
            }
            row.tail.push_back(line.to_string());
        }
    }

    /// Advance the spinner one frame and redraw. Cheap when no step
    /// is running; the runner drives this from a fixed-cadence timer.
    pub fn tick(&mut self) -> io::Result<()> {
        if self.paused {
            return Ok(());
        }
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
        self.redraw_full()
    }

    /// Pause output before handing the terminal to an interactive
    /// child. Clears the dynamic region so the child starts on a clean
    /// canvas.
    pub fn pause_for_interactive(&mut self, idx: usize) -> io::Result<()> {
        if let Some(row) = self.rows.get_mut(idx) {
            row.status = Status::Running;
            row.tail.clear();
        }
        // Discard captured lines from previous non-interactive steps —
        // they'd be misleading if this interactive step fails (the
        // user already saw any output it produced on the terminal).
        self.ring.clear();
        // Clear what we drew last; the child will print directly from here.
        let mut out = io::stdout();
        if self.last_line_count > 0 {
            crossterm::execute!(
                out,
                MoveUp(self.last_line_count),
                Clear(ClearType::FromCursorDown)
            )?;
            self.last_line_count = 0;
        }
        writeln!(out, "  {} {}", "▶".bold().blue(), self.rows[idx].label)?;
        writeln!(out, "  (passing terminal to step)")?;
        writeln!(out)?;
        out.flush()?;
        self.paused = true;
        Ok(())
    }

    /// Resume drawing after an interactive child exits. The runner
    /// has already updated the row's status via `end`; we just need
    /// to redraw the list.
    pub fn resume_after_interactive(&mut self) -> io::Result<()> {
        self.paused = false;
        // Make a visual break between the child's output and the
        // refreshed list so they don't smear together.
        writeln!(io::stdout())?;
        self.last_line_count = 0;
        self.redraw_full()
    }

    /// Print the failure summary (last captured lines) below the
    /// rendered list. Call after `end(_, Failed, _)` and before
    /// dropping the renderer so the caller sees what went wrong.
    pub fn print_failure_summary(&mut self, step_name: &str) -> io::Result<()> {
        let mut out = io::stdout();
        writeln!(out)?;
        writeln!(out, "Output from `{}`:", step_name.bold())?;
        if self.ring.is_empty() {
            writeln!(out, "  (no output captured)")?;
        } else {
            for line in &self.ring {
                writeln!(out, "  {line}")?;
            }
        }
        out.flush()?;
        // The summary is below the dynamic region — there's no further
        // redraw after this, so we don't need to update last_line_count.
        Ok(())
    }

    fn print_header(&mut self, project_name: &str) -> io::Result<()> {
        let mut out = io::stdout();
        writeln!(out, "Installing keel steps for {}", project_name.bold())?;
        writeln!(out)?;
        out.flush()
    }

    fn redraw_full(&mut self) -> io::Result<()> {
        let mut out = io::stdout();
        if self.last_line_count > 0 {
            crossterm::execute!(
                out,
                MoveUp(self.last_line_count),
                MoveToColumn(0),
                Clear(ClearType::FromCursorDown)
            )?;
        }
        let mut lines_drawn: u16 = 0;
        for row in &self.rows {
            self.write_row(&mut out, row)?;
            lines_drawn = lines_drawn.saturating_add(1);
            if row.status == Status::Running {
                for tail in &row.tail {
                    writeln!(out, "      {}", tail.as_str().dark_grey())?;
                    lines_drawn = lines_drawn.saturating_add(1);
                }
            }
        }
        out.flush()?;
        self.last_line_count = lines_drawn;
        Ok(())
    }

    fn write_row<W: Write>(&self, out: &mut W, row: &StepRow) -> io::Result<()> {
        let glyph = self.glyph(row.status);
        let label = match row.status {
            Status::Pending => row.label.as_str().dark_grey().to_string(),
            Status::Running => row.label.as_str().bold().to_string(),
            Status::Ok => row.label.clone(),
            Status::Failed => row.label.as_str().red().bold().to_string(),
            Status::Skipped => row.label.as_str().yellow().to_string(),
        };
        let duration = match row.duration {
            None => String::new(),
            Some(d) => {
                let d = format_duration(d);
                format!("  {}", d.as_str().dark_grey())
            }
        };
        writeln!(out, "  {glyph} {label}{duration}")?;
        Ok(())
    }

    fn glyph(&self, status: Status) -> String {
        match status {
            Status::Pending => "○".dark_grey().to_string(),
            Status::Running => {
                let frame = SPINNER_FRAMES[self.spinner_tick % SPINNER_FRAMES.len()];
                frame.blue().bold().to_string()
            }
            Status::Ok => "✓".green().bold().to_string(),
            Status::Failed => "✗".red().bold().to_string(),
            Status::Skipped => "→".yellow().to_string(),
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // Make sure the cursor ends up on a clean line and any
        // pending styles are reset, even on early termination.
        let _ = crossterm::execute!(io::stdout(), ResetColor);
        let _ = writeln!(io::stdout());
    }
}

fn format_duration(d: Duration) -> String {
    let total = d.as_secs_f64();
    if total < 10.0 {
        format!("{total:.1}s")
    } else if total < 60.0 {
        format!("{:.0}s", total)
    } else {
        let mins = total / 60.0;
        let secs = total % 60.0;
        format!("{:.0}m{:02.0}s", mins.floor(), secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_under_ten_seconds_is_decimal() {
        assert_eq!(format_duration(Duration::from_millis(83)), "0.1s");
        assert_eq!(format_duration(Duration::from_millis(2_500)), "2.5s");
    }

    #[test]
    fn duration_above_ten_seconds_is_integer() {
        assert_eq!(format_duration(Duration::from_secs(12)), "12s");
        assert_eq!(format_duration(Duration::from_secs(59)), "59s");
    }

    #[test]
    fn duration_above_minute_uses_mm_ss() {
        assert_eq!(format_duration(Duration::from_secs(60)), "1m00s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m05s");
    }
}
