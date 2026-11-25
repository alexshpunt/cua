// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Owner-thread presentation for the elected desktop worker.

use super::{Action, Agent, Delivery, Location};
use crate::activity::native::{
    create_protected_hud, exact_window_frame, flush_capture_exclusion, register_protected_hud_class,
};
use crate::overlay::{WinSurface, WinZOrderEnforcer};
use cursor_overlay::activity::{paint_hud_text, ActivityRect};
use cursor_overlay::{session_fill_rgba, ZOrderEnforcer};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Shell::GetScaleFactorForMonitor;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("Cua.ProtectedDesktopActivity");
const RECENT_LIMIT: usize = 5;
const AGENT_ROW_HEIGHT: i32 = 62;
const TEXT: [u8; 4] = [237, 243, 251, 255];
const MUTED: [u8; 4] = [165, 181, 203, 255];
const SHARED: [u8; 4] = [255, 202, 119, 255];
const BACKGROUND: [u8; 4] = [20, 27, 39, 250];
const ROW: [u8; 4] = [29, 39, 55, 255];
const FRAME: [u8; 4] = [63, 80, 104, 255];

/// Win32 window and its premultiplied alpha surface. Both stay on the worker thread.
struct Hud<T> {
    hwnd: HWND,
    bounds: ActivityRect,
    surface: WinSurface,
    content: T,
}

impl<T> Drop for Hud<T> {
    fn drop(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

impl<T: PartialEq> Hud<T> {
    fn update(
        slot: &mut Option<Self>,
        bounds: ActivityRect,
        content: T,
        paint: impl FnOnce(&mut Pixmap, &T),
    ) {
        if slot.as_ref().is_some_and(|hud| {
            (hud.bounds.width, hud.bounds.height) != (bounds.width, bounds.height)
        }) {
            *slot = None;
        }
        if let Some(hud) = slot {
            let redraw = hud.content != content;
            if redraw {
                hud.surface.pixmap.fill(Color::TRANSPARENT);
                paint(&mut hud.surface.pixmap, &content);
                hud.content = content;
            }
            let moved = hud.bounds != bounds;
            if moved {
                hud.surface.move_hud(bounds.x, bounds.y);
                hud.bounds = bounds;
            }
            if (redraw || moved) && !unsafe { hud.surface.present_hud(hud.hwnd) } {
                *slot = None;
            }
            return;
        }
        let Some(mut surface) = (unsafe {
            WinSurface::create(
                bounds.x,
                bounds.y,
                bounds.width as i32,
                bounds.height as i32,
            )
        }) else {
            return;
        };
        let Some(hwnd) = create_protected_hud(CLASS, bounds) else {
            return;
        };
        let mut hud = Self {
            hwnd,
            bounds,
            surface,
            content,
        };
        paint(&mut hud.surface.pixmap, &hud.content);
        if unsafe { hud.surface.present_hud(hud.hwnd) } {
            *slot = Some(hud);
        }
    }

    fn show(&self, target: Option<u64>) {
        if target.is_some() {
            WinZOrderEnforcer {
                hwnd_isize: self.hwnd.0 as isize,
            }
            .reassert(target);
        } else {
            // Global panel and unbound point are always legible. No foreground transition.
            let _ = unsafe {
                SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
                )
            };
        }
    }

    fn pending(&self) -> bool {
        !unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }
}

#[derive(Default)]
struct Cues {
    outline: [Option<Hud<[u8; 4]>>; 4],
    point: Option<Hud<PointMark>>,
}

#[derive(Default)]
pub(super) struct Renderer {
    panel: Option<Hud<Panel>>,
    cues: HashMap<String, Cues>,
}

impl Renderer {
    pub(super) fn new() -> Option<Self> {
        let old =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if old.0.is_null() {
            return None;
        }
        register_protected_hud_class(CLASS);
        Some(Self::default())
    }

    pub(super) fn render(&mut self, rows: &[Agent], panel: bool) {
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if let Some((bounds, panel)) = (panel && !rows.is_empty())
            .then(|| Panel::layout(rows))
            .flatten()
        {
            Hud::update(&mut self.panel, bounds, panel, Panel::paint);
        } else {
            self.panel = None;
        }

        let mut used = HashSet::new();
        let mut outline_counts: HashMap<u64, i32> = HashMap::new();
        let mut anchored = Vec::new();
        for agent in rows.iter().filter(|a| a.active && (a.outline || a.point)) {
            let Some(action) = agent
                .action
                .as_ref()
                .filter(|a| matches!(a.location, Location::Desktop))
            else {
                continue;
            };
            let frame = window_frame(action);
            if action.window.is_some() && frame.is_none() {
                continue;
            }
            used.insert(agent.id.as_str());
            let cue = self.cues.entry(agent.id.clone()).or_default();
            let color = session_fill_rgba(&agent.id);
            if let Some((id, frame)) = frame.filter(|_| agent.outline) {
                let count = outline_counts.entry(id).or_default();
                cue.set_outline(frame, *count, color);
                *count += 1;
            } else {
                cue.outline = Default::default();
            }
            let dot = action.position.filter(|&(x, y)| {
                frame.is_none_or(|(_, r)| action.position_frame == Some(r) && contains(r, x, y))
            });
            if let Some((bounds, mark)) = dot.filter(|_| agent.point).and_then(|(x, y)| {
                PointMark::layout(
                    x,
                    y,
                    &format!("{} · {}", agent.label, action.kind.label()),
                    color,
                )
            }) {
                Hud::update(&mut cue.point, bounds, mark, PointMark::paint);
            } else {
                cue.point = None;
            }
            anchored.push((agent.id.as_str(), action, frame));
        }
        self.cues.retain(|id, _| used.contains(id.as_str()));
        // Batch the composition barrier across new surfaces; avoid one DWM wait per cue edge.
        let pending =
            self.panel.as_ref().is_some_and(Hud::pending) || self.cues.values().any(Cues::pending);
        if pending && !flush_capture_exclusion() {
            self.cues.clear();
            self.panel = None;
            return;
        }
        for (id, action, frame) in anchored {
            // A target may change during upload or the DWM barrier.
            if window_frame(action) != frame {
                self.cues.remove(id);
                continue;
            }
            let cue = &self.cues[id];
            let target = frame.map(|(id, _)| id);
            for hud in cue.outline.iter().flatten() {
                hud.show(target);
            }
            if let Some(point) = &cue.point {
                point.show(target);
            }
        }
        if let Some(panel) = &self.panel {
            panel.show(None);
        }
    }
}

impl Cues {
    fn set_outline(&mut self, frame: ActivityRect, index: i32, color: [u8; 4]) {
        for (slot, bounds) in self.outline.iter_mut().zip(outline_strips(frame, index)) {
            if let Some(bounds) = bounds {
                Hud::update(slot, bounds, color, |pixmap, &[r, g, b, _]| {
                    pixmap.fill(Color::from_rgba8(r, g, b, 245));
                });
            } else {
                *slot = None;
            }
        }
    }

    fn pending(&self) -> bool {
        self.point.as_ref().is_some_and(Hud::pending)
            || self.outline.iter().flatten().any(Hud::pending)
    }
}

fn window_frame(action: &Action) -> Option<(u64, ActivityRect)> {
    let w = action.window.as_ref()?;
    let frame = exact_window_frame(w.window_id, w.pid, &w.identity)?;
    // Pinned z-order requires the top-level HWND whose geometry we just validated.
    let hwnd = HWND(w.window_id as *mut _);
    (unsafe { GetAncestor(hwnd, GA_ROOT) } == hwnd).then_some((w.window_id, frame))
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Option<ActivityRect> {
    let rect = ActivityRect {
        x,
        y,
        width: w.try_into().ok()?,
        height: h.try_into().ok()?,
    };
    rect.valid().then_some(rect)
}

fn contains(r: ActivityRect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.width as i32 && y < r.y + r.height as i32
}

fn outline_strips(frame: ActivityRect, index: i32) -> [Option<ActivityRect>; 4] {
    const STROKE: i32 = 3;
    // Concentric edges show concurrent work on one exact window with independently colored cues.
    let inset = index * (STROKE + 1);
    let x = frame.x + inset;
    let y = frame.y + inset;
    let w = frame.width as i32 - 2 * inset;
    let h = frame.height as i32 - 2 * inset;
    if w < 2 * STROKE || h < 2 * STROKE {
        return [None; 4];
    }
    [
        rect(x, y, w, STROKE),
        rect(x, y + h - STROKE, w, STROKE),
        rect(x, y + STROKE, STROKE, h - 2 * STROKE),
        rect(x + w - STROKE, y + STROKE, STROKE, h - 2 * STROKE),
    ]
}

struct Monitor {
    work: ActivityRect,
    screen: ActivityRect,
    scale: u32,
}

fn monitor(point: POINT, primary_fallback: bool) -> Option<Monitor> {
    let handle = unsafe {
        MonitorFromPoint(
            point,
            if primary_fallback {
                MONITOR_DEFAULTTOPRIMARY
            } else {
                MONITOR_DEFAULTTONULL
            },
        )
    };
    if handle.0.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(handle, &mut info) }.as_bool() {
        return None;
    }
    let scale = unsafe { GetScaleFactorForMonitor(handle) }
        .ok()
        .map_or(100, |s| s.0.clamp(100, 300) as u32);
    let convert = |r: windows::Win32::Foundation::RECT| {
        rect(r.left, r.top, r.right - r.left, r.bottom - r.top)
    };
    Some(Monitor {
        work: convert(info.rcWork)?,
        screen: convert(info.rcMonitor)?,
        scale,
    })
}

#[derive(PartialEq, Eq)]
struct PointMark {
    origin: (i32, i32),
    label: String,
    color: [u8; 4],
    scale: u32,
    put_label_right: bool,
}

impl PointMark {
    fn layout(x: i32, y: i32, label: &str, color: [u8; 4]) -> Option<(ActivityRect, Self)> {
        let Monitor { screen, scale, .. } = monitor(POINT { x, y }, false)?;
        let s = |n| scaled(n, scale);
        let right = screen.x + screen.width as i32;
        let bottom = screen.y + screen.height as i32;
        let w = s(168).min(screen.width as i32);
        let h = s(28).min(screen.height as i32);
        let put_label_right = right - x > x - screen.x;
        let left = (x - if put_label_right { s(10) } else { w - s(10) }).clamp(screen.x, right - w);
        let top = (y - h / 2).clamp(screen.y, bottom - h);
        Some((
            rect(left, top, w, h)?,
            Self {
                origin: (x - left, y - top),
                label: label.to_owned(),
                color,
                scale,
                put_label_right,
            },
        ))
    }

    fn paint(pixmap: &mut Pixmap, mark: &Self) {
        let s = |n| scaled(n, mark.scale);
        let (x, y) = mark.origin;
        let w = pixmap.width() as i32;
        let (lead, trail) = if mark.put_label_right {
            (x + s(10), w)
        } else {
            (0, x - s(10))
        };
        let label_top = s(3);
        fill(pixmap, lead, label_top, trail - lead, s(22), BACKGROUND);
        line(
            pixmap,
            &mark.label,
            lead + s(6),
            label_top + s(3),
            trail - lead - s(12),
            s(13),
            TEXT,
        );
        if let Some(ring) = PathBuilder::from_circle(x as f32 + 0.5, y as f32 + 0.5, s(7) as f32) {
            pixmap.fill_path(
                &ring,
                &paint(BACKGROUND),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        if let Some(dot) = PathBuilder::from_circle(x as f32 + 0.5, y as f32 + 0.5, s(4) as f32) {
            pixmap.fill_path(
                &dot,
                &paint(mark.color),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
}

#[derive(PartialEq, Eq)]
struct PanelAgent {
    name: String,
    purpose: String,
    route: String,
    status: &'static str,
    color: [u8; 4],
    shared: bool,
}

#[derive(PartialEq, Eq)]
struct Panel {
    scale: u32,
    active: Vec<PanelAgent>,
    hidden: usize,
    shared: usize,
    unknown: usize,
    recent: Vec<(String, String)>,
}

impl Panel {
    fn layout(rows: &[Agent]) -> Option<(ActivityRect, Self)> {
        let Monitor { work, scale, .. } = monitor(POINT { x: 0, y: 0 }, true)?;
        let s = |n| scaled(n, scale);
        let width = s(456).min(work.width as i32 - s(24));
        let height_limit = work.height as i32 - s(24);
        let mut active = rows.iter().filter(|agent| agent.active).collect::<Vec<_>>();
        let shared = active
            .iter()
            .filter(|a| {
                a.action
                    .as_ref()
                    .is_some_and(|action| matches!(action.delivery, Delivery::Shared))
            })
            .count();
        let unknown = active
            .iter()
            .filter(|a| {
                a.action
                    .as_ref()
                    .is_some_and(|action| matches!(action.delivery, Delivery::Unknown))
            })
            .count();
        active.sort_by_key(|&a| {
            (
                !a.action
                    .as_ref()
                    .is_some_and(|action| matches!(action.delivery, Delivery::Shared)),
                a.action.is_none(),
                a.label.as_str(),
                a.id.as_str(),
            )
        });
        let reserve = if rows.iter().any(|a| !a.recent.is_empty()) {
            s(67)
        } else {
            0
        };
        let max_agents = ((height_limit - s(78) - reserve).max(0) / s(AGENT_ROW_HEIGHT)) as usize;
        let hidden = active.len().saturating_sub(max_agents);
        let active = active
            .into_iter()
            .take(max_agents)
            .map(|agent| {
                let (status, route, shared) = agent.action.as_ref().map_or(
                    ("Between actions", String::new(), false),
                    |action| {
                        let location = action
                            .window
                            .as_ref()
                            .filter(|_| matches!(action.location, Location::Desktop))
                            .map_or_else(
                                || match action.location {
                                    Location::Desktop => "Desktop · target unknown".to_owned(),
                                    other => other.label().to_owned(),
                                },
                                |w| format!("Window {}", w.window_id),
                            );
                        (
                            action.kind.label(),
                            format!("{}  ·  {}", action.delivery.label(), location),
                            matches!(action.delivery, Delivery::Shared),
                        )
                    },
                );
                PanelAgent {
                    name: agent.label.clone(),
                    purpose: agent.intent.clone(),
                    route,
                    status,
                    color: session_fill_rgba(&agent.id),
                    shared,
                }
            })
            .collect::<Vec<_>>();
        let active_height =
            s(58) + active.len() as i32 * s(AGENT_ROW_HEIGHT) + if hidden > 0 { s(19) } else { 0 };
        let max_recent = ((height_limit - active_height - s(25)).max(0) / s(21)) as usize;
        let mut recent = rows
            .iter()
            .flat_map(|a| a.recent.iter().map(|entry| (&a.label, entry)))
            .collect::<Vec<_>>();
        recent.sort_by_key(|&(name, entry)| (Reverse(entry.at_ms), name.as_str()));
        let recent = recent
            .into_iter()
            .take(max_recent.min(RECENT_LIMIT))
            .map(|(name, entry)| {
                let shared = if entry.shared { " · Shared input" } else { "" };
                (
                    name.clone(),
                    format!(
                        "{} · {}{}",
                        entry.kind.label(),
                        entry.outcome.label(),
                        shared
                    ),
                )
            })
            .collect::<Vec<_>>();
        let height = active_height
            + if recent.is_empty() {
                0
            } else {
                s(25) + recent.len() as i32 * s(21)
            };
        let bounds = rect(
            work.x + work.width as i32 - width - s(12),
            work.y + work.height as i32 - height - s(12),
            width,
            height,
        )?;
        Some((
            bounds,
            Self {
                scale,
                active,
                hidden,
                shared,
                unknown,
                recent,
            },
        ))
    }

    fn paint(pixmap: &mut Pixmap, panel: &Self) {
        let s = |n| scaled(n, panel.scale);
        let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
        pixmap.fill(color(FRAME));
        fill(pixmap, 1, 1, w - 2, h - 2, BACKGROUND);
        line(
            pixmap,
            "Computer activity",
            s(14),
            s(8),
            w - s(28),
            s(19),
            TEXT,
        );
        let shared = match panel.shared {
            0 => "No shared input observed".to_owned(),
            1 => "1 agent using shared input".to_owned(),
            n => format!("{n} agents using shared input"),
        };
        let summary = if panel.unknown == 0 {
            shared
        } else {
            format!("{shared}  ·  {} unknown", panel.unknown)
        };
        line(
            pixmap,
            &summary,
            s(14),
            s(35),
            w - s(28),
            s(13),
            if panel.shared > 0 { SHARED } else { MUTED },
        );
        let mut y = s(55);
        let split = (w / 3).max(s(24));
        for a in &panel.active {
            fill(pixmap, s(8), y, w - s(16), s(AGENT_ROW_HEIGHT - 4), ROW);
            fill(pixmap, s(8), y, s(3), s(AGENT_ROW_HEIGHT - 4), a.color);
            line(pixmap, &a.name, s(16), y + s(3), split - s(22), s(14), TEXT);
            line(
                pixmap,
                a.status,
                split,
                y + s(3),
                w - split - s(14),
                s(14),
                if a.shared { SHARED } else { TEXT },
            );
            line(
                pixmap,
                &a.purpose,
                s(16),
                y + s(37),
                w - s(32),
                s(12),
                MUTED,
            );
            line(
                pixmap,
                &a.route,
                split,
                y + s(20),
                w - split - s(14),
                s(12),
                MUTED,
            );
            y += s(AGENT_ROW_HEIGHT);
        }
        if panel.hidden > 0 {
            line(
                pixmap,
                &format!("+{} more active", panel.hidden),
                s(14),
                y,
                w - s(28),
                s(12),
                MUTED,
            );
            y += s(19);
        }
        if !panel.recent.is_empty() {
            fill(pixmap, s(14), y + s(3), w - s(28), 1, FRAME);
            line(pixmap, "Recent", s(14), y + s(8), w - s(28), s(14), TEXT);
            y += s(25);
            for (name, detail) in &panel.recent {
                line(pixmap, name, s(14), y, split - s(20), s(12), MUTED);
                line(pixmap, detail, split, y, w - split - s(14), s(12), TEXT);
                y += s(21);
            }
        }
    }
}

fn scaled(value: i32, scale: u32) -> i32 {
    (value * scale as i32 + 50) / 100
}

fn color([r, g, b, a]: [u8; 4]) -> Color {
    Color::from_rgba8(r, g, b, a)
}

fn paint(rgba: [u8; 4]) -> Paint<'static> {
    Paint {
        shader: tiny_skia::Shader::SolidColor(color(rgba)),
        anti_alias: true,
        ..Default::default()
    }
}

fn fill(pixmap: &mut Pixmap, x: i32, y: i32, w: i32, h: i32, rgba: [u8; 4]) {
    if let Some(rect) = Rect::from_xywh(x as f32, y as f32, w as f32, h as f32) {
        pixmap.fill_rect(rect, &paint(rgba), Transform::identity(), None);
    }
}

fn line(pixmap: &mut Pixmap, text: &str, x: i32, y: i32, w: i32, size: i32, rgba: [u8; 4]) {
    if let Some(rect) = Rect::from_xywh(x as f32, y as f32, w as f32, (size + size / 3) as f32) {
        paint_hud_text(pixmap, text, rect, size as f32, rgba);
    }
}
