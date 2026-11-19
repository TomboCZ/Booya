//! Render a source-time tutorial edit list. Annotations use normalized source
//! coordinates, so they stay attached to the GUI as the camera moves.
use crate::ffmpeg::{self, FrameReader};
use crate::fsutil::{same_file, Staged};
use crate::project::{Background, Project, Shape, ShapeKind};
use crate::render::{Frame, Renderer};
use crate::zoom::ZoomSegment;
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

const FPS: f64 = 30.0;

#[derive(Deserialize)]
struct Plan {
    source: PathBuf,
    width: u32,
    height: u32,
    scenes: Vec<Scene>,
}

#[derive(Deserialize)]
struct Scene {
    source_start: f64,
    source_end: f64,
    #[serde(default)]
    hold: f64,
    #[serde(default)]
    annotations: Vec<Annotation>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Focus,
    Click,
}

#[derive(Deserialize)]
struct Annotation {
    kind: Kind,
    start: f64,
    end: f64,
    #[serde(rename = "box")]
    bounds: [f64; 4],
    point: Option<[f64; 2]>,
    camera_box: Option<[f64; 4]>,
    outline_windows: Option<Vec<[f64; 2]>>,
}

impl Scene {
    fn duration(&self) -> f64 {
        self.source_end - self.source_start + self.hold
    }
    fn validate(&self, source_duration: f64) -> Result<()> {
        ensure!(
            self.source_start >= 0.0
                && self.source_end > self.source_start
                && self.source_end <= source_duration + 1.0 / FPS
                && self.hold >= 0.0,
            "invalid source interval or hold"
        );
        for a in &self.annotations {
            ensure!(
                a.start >= 0.0 && a.end > a.start && a.end <= self.duration() + 1.0 / FPS,
                "annotation outside its scene"
            );
            let [x1, y1, x2, y2] = a.bounds;
            ensure!(
                a.bounds.iter().all(|v| (0.0..=1.0).contains(v)) && x1 < x2 && y1 < y2,
                "invalid source box"
            );
            if let Some([l, t, r, b]) = a.camera_box {
                ensure!(
                    0.0 <= l
                        && l <= x1
                        && x2 <= r
                        && r <= 1.0
                        && 0.0 <= t
                        && t <= y1
                        && y2 <= b
                        && b <= 1.0,
                    "camera context must contain its subject"
                );
            }
            if let Some(windows) = &a.outline_windows {
                ensure!(
                    windows
                        .iter()
                        .all(|[l, r]| a.start <= *l && l < r && *r <= a.end),
                    "invalid outline interval"
                );
            }
            if matches!(a.kind, Kind::Click) {
                ensure!(
                    a.point
                        .is_some_and(|v| v.iter().all(|n| (0.0..=1.0).contains(n))),
                    "click requires a normalized position"
                );
            }
        }
        Ok(())
    }

    fn compose(&self, plan: &Plan, width: u32, height: u32) -> (Renderer, Vec<Shape>) {
        let mut p = Project::new(
            plan.source.clone(),
            None,
            width,
            height,
            self.duration() * 1000.0,
        );
        p.style.background = Background::None;
        p.style.padding = 0.0;
        p.style.corner_radius = 0.0;
        p.style.shadow = 0.0;
        p.style.glass.enabled = false;
        let mut shapes = Vec::new();
        for a in &self.annotations {
            let [x1, y1, x2, y2] = a.camera_box.unwrap_or(a.bounds);
            let (kind, bounds, color) = match a.kind {
                Kind::Focus => {
                    let coverage = if a.camera_box.is_some() { 0.94 } else { 0.78 };
                    p.zoom.push(ZoomSegment {
                        start_ms: (a.start * 1000.0 - 250.0).max(0.0),
                        end_ms: (a.end * 1000.0 + 250.0).min(p.duration_ms),
                        scale: (coverage / (x2 - x1).max(y2 - y1)).clamp(1.0, 2.35),
                        follow_cursor: false,
                        focus: (
                            (x1 + x2) * width as f64 / 2.0,
                            (y1 + y2) * height as f64 / 2.0,
                        ),
                        reason: "focus".into(),
                    });
                    (ShapeKind::Box, a.bounds, "#00ddd2")
                }
                Kind::Click => {
                    let [x, y] = a.point.expect("validated click");
                    let r = 0.019 * width.min(height) as f64;
                    (
                        ShapeKind::Ellipse,
                        [
                            x - r / width as f64,
                            y - r / height as f64,
                            x + r / width as f64,
                            y + r / height as f64,
                        ],
                        "#ffb000",
                    )
                }
            };
            let default_window = [[a.start, a.end]];
            for &[start, end] in a.outline_windows.as_deref().unwrap_or(&default_window) {
                shapes.push(Shape {
                    kind,
                    start_ms: start * 1000.0,
                    end_ms: end * 1000.0,
                    x1: bounds[0],
                    y1: bounds[1],
                    x2: bounds[2],
                    y2: bounds[3],
                    color: color.into(),
                    width: if matches!(a.kind, Kind::Focus) {
                        0.003
                    } else {
                        0.005
                    },
                });
            }
        }
        (
            Renderer::new(p, None, plan.width, plan.height, None),
            shapes,
        )
    }
}

/// Writes a silent H.264 MP4 through a staging file. The caller owns narration and muxing.
pub fn render(plan_path: &Path, output: &Path) -> Result<()> {
    let plan: Plan = serde_json::from_reader(std::fs::File::open(plan_path)?)?;
    ensure!(
        !same_file(&plan.source, output) && !same_file(plan_path, output),
        "output overlaps input"
    );
    let valid_dimension = |n: u32| (2..=4096).contains(&n) && n.is_multiple_of(2);
    ensure!(
        valid_dimension(plan.width) && valid_dimension(plan.height) && !plan.scenes.is_empty(),
        "invalid output dimensions or empty scene list"
    );
    let info = ffmpeg::probe(&plan.source)?;
    for scene in &plan.scenes {
        scene.validate(info.duration_ms / 1000.0)?;
    }
    let staged = Staged::new(output);
    let dims = format!("{}x{}", plan.width, plan.height);
    let mut child = crate::proc::command(ffmpeg::ffmpeg_bin())
        .args([
            "-nostdin", "-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &dims,
            "-r", "30", "-i", "pipe:0", "-an",
        ])
        .args(ffmpeg::best_encoder().export_args(plan.width, plan.height, FPS))
        .args(["-pix_fmt", "yuv420p", "-f", "mp4"])
        .arg(staged.path())
        .stdin(Stdio::piped())
        .spawn()
        .context("start tutorial encoder")?;
    let mut input = child.stdin.take().context("open encoder pipe")?;
    let result = write_frames(&plan, &info, &mut input);
    drop(input);
    let status = child.wait()?;
    result?;
    ensure!(status.success(), "tutorial encoder returned {status}");
    staged.commit()
}

fn write_frames(plan: &Plan, info: &ffmpeg::MediaInfo, out: &mut impl Write) -> Result<()> {
    let mut elapsed = 0.0;
    for scene in &plan.scenes {
        let frame0 = (elapsed * FPS).round() as usize;
        elapsed += scene.duration();
        let count = (elapsed * FPS).round() as usize - frame0;
        let (renderer, shapes) = scene.compose(plan, info.width, info.height);
        let (w, h) = renderer.decode_size();
        let mut reader = FrameReader::open(&plan.source, scene.source_start * 1000.0, FPS, w, h)?;
        let mut last: Option<Frame> = None;
        for i in 0..count {
            let t = i as f64 / FPS;
            if t < scene.source_end - scene.source_start {
                if let Some(frame) = reader.next()? {
                    last = Some(frame);
                } else {
                    ensure!(
                        t + 1.5 / FPS >= scene.source_end - scene.source_start,
                        "decoder stopped early"
                    );
                }
            }
            let frame = last.as_ref().context("empty decoded scene")?;
            let ms = t * 1000.0;
            // Mark the source before the camera moves, including during still-frame holds.
            if shapes.iter().any(|s| ms >= s.start_ms && ms <= s.end_ms) {
                let mut marked = frame.clone();
                Renderer::draw_shapes(&mut marked, &shapes, ms);
                out.write_all(&renderer.render(&marked, ms, None).data)?;
            } else {
                out.write_all(&renderer.render(frame, ms, None).data)?;
            }
        }
    }
    Ok(())
}
