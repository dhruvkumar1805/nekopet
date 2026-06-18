use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};

#[derive(serde::Deserialize)]
#[serde(default)]
struct Config {
    scale:  u32,
    anim_ms: u32,
    corner: String,
    stretch_every_secs: u64,
    stretch_anim_ms: u32,
    stretch_hold_ms: u32,
    bounce_every_secs: u64,
    lean_every_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self { scale: 3, anim_ms: 120, corner: "bottom-right".into(), stretch_every_secs: 1800, stretch_anim_ms: 400, stretch_hold_ms: 1500, bounce_every_secs: 60, lean_every_secs: 45 }
    }
}

fn load_config() -> Config {
    let config_home = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
        format!("{}/.config", std::env::var("HOME").unwrap_or_else(|_| ".".into()))
    });
    let path = std::path::PathBuf::from(&config_home).join("nekopet/config.toml");
    if !path.exists() {
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let _ = std::fs::write(&path,
            "scale   = 3\nanim_ms = 120\ncorner  = \"bottom-right\"\nstretch_every_secs = 1800\nstretch_anim_ms = 400\nstretch_hold_ms = 1500\nbounce_every_secs = 60\nlean_every_secs = 45\n");
    }
    std::fs::read_to_string(&path).ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

const FRAME_W: u32 = 32;
const FRAME_H: u32 = 32;
const BODY_L_SRC: i32 = 3;
const BODY_R_SRC: i32 = 10;
const CAT_BOTTOM_MARGIN: i32 = 16;
const BREATHE_PERIOD_MS: f64 = 4000.0;
const BREATHE_AMPLITUDE: f32 = 0.04;

fn start_keyboard_watcher(flag: Arc<AtomicBool>) {
    for i in 0..32 {
        let path = format!("/dev/input/event{}", i);
        if let Ok(file) = std::fs::OpenOptions::new().read(true).open(&path) {
            let flag = flag.clone();
            std::thread::spawn(move || {
                use std::io::Read;
                let mut f = file;
                let mut buf = [0u8; 24];
                loop {
                    if f.read_exact(&mut buf).is_err() { break; }
                    let ev_type  = u16::from_ne_bytes([buf[16], buf[17]]);
                    let ev_code  = u16::from_ne_bytes([buf[18], buf[19]]);
                    let ev_value = i32::from_ne_bytes([buf[20], buf[21], buf[22], buf[23]]);
                    if ev_type == 1 && ev_value == 1 && ev_code < 256 {
                        flag.store(true, Ordering::Relaxed);
                    }
                }
            });
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Idle,
    Typing,
    Drag,
    Stretch,
    Bounce,
    Lean,
}

impl State {
    fn frame_count(self) -> usize {
        4
    }

    fn duration_ms(self, time: u32) -> u32 {
        match self {
            State::Idle    => 3_000 + (time % 4_000),
            State::Typing | State::Drag | State::Stretch | State::Bounce | State::Lean => u32::MAX,
        }
    }
}

struct Frames {
    right: Vec<Vec<u8>>,
}

fn count_frames_in_row(sheet: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>, row: u32) -> u32 {
    let max_cols = sheet.width() / FRAME_W;
    (0..max_cols)
        .take_while(|&col| {
            (0..FRAME_W).any(|x| (0..FRAME_H).any(|y| {
                sheet.get_pixel(col * FRAME_W + x, row * FRAME_H + y)[3] > 0
            }))
        })
        .count() as u32
}

fn load_anim(
    sheet: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    row: u32,
    count: u32,
    disp_w: u32,
    disp_h: u32,
) -> Frames {
    let right: Vec<Vec<u8>> = (0..count)
        .map(|col| {
            let buf = image::ImageBuffer::from_fn(FRAME_W, FRAME_H, |px, py| {
                *sheet.get_pixel(col * FRAME_W + px, row * FRAME_H + py)
            });
            image::DynamicImage::from(buf)
                .resize_exact(disp_w, disp_h, image::imageops::FilterType::Nearest)
                .into_rgba8()
                .into_raw()
                .chunks_exact(4)
                .flat_map(|p| {
                    let a = p[3];
                    let premul = |c: u8| (c as u16 * a as u16 / 255) as u8;
                    [premul(p[2]), premul(p[1]), premul(p[0]), a]
                })
                .collect()
        })
        .collect();
    Frames { right }
}

fn paint_block(canvas: &mut [u8], cw: usize, x: i32, y: i32, size: i32, color: [u8; 4]) {
    for dy in 0..size {
        for dx in 0..size {
            let px = x + dx;
            let py = y + dy;
            if px >= 0 && py >= 0 {
                let i = (py as usize * cw + px as usize) * 4;
                if i + 4 <= canvas.len() {
                    canvas[i..i + 4].copy_from_slice(&color);
                }
            }
        }
    }
}

fn shift_pupils(
    canvas: &mut [u8],
    cw: usize,
    pos_x: i32,
    pos_y: i32,
    cursor_x: f64,
    cursor_y: f64,
    scale: u32,
    disp_w: u32,
) {
    let s = scale as i32;
    let ps = 2 * s;

    let face_cx = pos_x as f64 + disp_w as f64 * 0.5;
    let face_cy = pos_y as f64 + 9.0 * scale as f64;
    let ddx = cursor_x - face_cx;
    let ddy = cursor_y - face_cy;
    let dist = (ddx * ddx + ddy * ddy).sqrt();
    let (sx, sy) = if dist < 8.0 {
        (0, 0)
    } else {
        (ddx.signum() as i32 * s, ((ddy / dist) * s as f64).round() as i32)
    };

    const EYE_BG: [u8; 4] = [102, 160, 217, 255];
    const PUPIL:  [u8; 4] = [0,   0,   0,   255];

    let py = 9 * s;
    let lx = 12 * s;
    let rx = 18 * s;

    for &base_x in &[lx, rx] {
        paint_block(canvas, cw, pos_x + base_x,      pos_y + py,      ps, EYE_BG);
        paint_block(canvas, cw, pos_x + base_x + sx, pos_y + py + sy, ps, PUPIL);
    }
}

fn tint_cat(canvas: &mut [u8], cw: usize, pos_x: i32, pos_y: i32, disp_w: u32, disp_h: u32, heat: f32) {
    if heat < 0.05 { return; }
    let x1 = pos_x.max(0) as usize;
    let x2 = (pos_x + disp_w as i32).min(cw as i32).max(0) as usize;
    let y1 = pos_y.max(0) as usize;
    let y2 = (pos_y + disp_h as i32).max(0) as usize;
    for y in y1..y2 {
        for x in x1..x2 {
            let i = (y * cw + x) * 4;
            if i + 4 > canvas.len() { continue; }
            if canvas[i + 3] == 0 { continue; }
            let b = canvas[i]   as i32;
            let g = canvas[i+1] as i32;
            let r = canvas[i+2] as i32;
            let is_body = r > 140 && g > 50 && g < 180 && b < r / 2;
            if !is_body { continue; }
            canvas[i]   = (b as f32 * (1.0 - heat) + 20.0  * heat) as u8;
            canvas[i+1] = (g as f32 * (1.0 - heat) + 20.0  * heat) as u8;
            canvas[i+2] = (r as f32 * (1.0 - heat) + 210.0 * heat) as u8;
        }
    }
}

fn blit_scaled(canvas: &mut [u8], cw: usize, ch: usize,
               src: &[u8], sw: usize, sh: usize,
               dst_x: i32, dst_y: i32, dw: usize, dh: usize) {
    for dy in 0..dh {
        let cy = dst_y + dy as i32;
        if cy < 0 || cy >= ch as i32 { continue; }
        for dx in 0..dw {
            let cx = dst_x + dx as i32;
            if cx < 0 || cx >= cw as i32 { continue; }
            let sx = (dx * sw / dw).min(sw - 1);
            let sy = (dy * sh / dh).min(sh - 1);
            let si = (sy * sw + sx) * 4;
            let di = (cy as usize * cw + cx as usize) * 4;
            if si + 4 <= src.len() && di + 4 <= canvas.len() {
                canvas[di..di + 4].copy_from_slice(&src[si..si + 4]);
            }
        }
    }
}

struct PetApp {
    registry_state: RegistryState,
    compositor_state: CompositorState,
    output_state: OutputState,
    shm: Shm,
    layer_shell: LayerShell,
    layer_surface: Option<LayerSurface>,
    pool: Option<SlotPool>,
    width: u32,
    height: u32,
    running: bool,
    idle: Frames,
    typing: Frames,
    drag: Frames,
    drag_frame_count: usize,
    state: State,
    state_start_ms: u32,
    frame_idx: usize,
    last_anim_ms: u32,
    frame_time_ms: u32,
    pos_x: i32,
    pos_y: i32,
    pos_y_initialized: bool,
    seat_state: SeatState,
    pointer: Option<wl_pointer::WlPointer>,
    dragging: bool,
    scale: u32,
    disp_w: u32,
    disp_h: u32,
    body_l: i32,
    body_r: i32,
    anim_ms: u32,
    corner: String,
    drag_start_pos_x: i32,
    drag_start_pos_y: i32,
    drag_start_local_x: f64,
    drag_start_local_y: f64,
    cursor_x: f64,
    cursor_y: f64,
    key_pressed: Arc<AtomicBool>,
    typing_until: Option<std::time::Instant>,
    typing_heat: f32,
    stretch_frames: Option<Frames>,
    stretch_frame_count: usize,
    stretch_every_secs: u64,
    stretch_anim_ms: u32,
    stretch_hold_ms: u32,
    last_stretch: std::time::Instant,
    stretch_transition: f32,
    stretch_out_t: f32,
    stretch_zooming_out: bool,
    stretch_start_x: i32,
    stretch_start_y: i32,
    stretch_hold_start: Option<u32>,
    bounce: Frames,
    bounce_frame_count: usize,
    bounce_every_secs: u64,
    last_bounce: std::time::Instant,
    lean: Frames,
    lean_frame_count: usize,
    lean_every_secs: u64,
    last_lean: std::time::Instant,
}

impl PetApp {
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let width = self.width;
        let height = self.height;
        if width == 0 || height == 0 {
            return;
        }

        let draw_state = self.state;
        let cursor_x = self.cursor_x;
        let cursor_y = self.cursor_y;
        let pos_x = self.pos_x;
        let pos_y = self.pos_y;
        let typing_heat = self.typing_heat;
        let frame_idx = self.frame_idx;

        let buffer = {
            let pool = match self.pool.as_mut() {
                Some(p) => p,
                None => return,
            };
            let (buffer, canvas) = pool
                .create_buffer(
                    width as i32,
                    height as i32,
                    (width * 4) as i32,
                    wl_shm::Format::Argb8888,
                )
                .expect("create_buffer failed");
            canvas.fill(0);

            if draw_state == State::Stretch {
                let tw = width as usize / 2;
                let th = height as usize / 2;
                let tx = (width as usize / 4) as i32;
                let ty = (height as usize / 4) as i32;
                let cat_cx = self.stretch_start_x as f32 + self.disp_w as f32 * 0.5;
                let cat_cy = self.stretch_start_y as f32 + self.disp_h as f32 * 0.5;
                let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;

                let (cur_w, cur_h, dst_x, dst_y, src_frame) =
                    if self.stretch_zooming_out {
                        let t = self.stretch_out_t;
                        let w = lerp(tw as f32, self.disp_w as f32, t) as usize;
                        let h = lerp(th as f32, self.disp_h as f32, t) as usize;
                        let cx = lerp(tx as f32 + tw as f32 * 0.5, cat_cx, t);
                        let cy = lerp(ty as f32 + th as f32 * 0.5, cat_cy, t);
                        (w, h, (cx - w as f32 * 0.5) as i32, (cy - h as f32 * 0.5) as i32, frame_idx)
                    } else if self.stretch_transition < 1.0 {
                        let t = self.stretch_transition;
                        let w = lerp(self.disp_w as f32, tw as f32, t) as usize;
                        let h = lerp(self.disp_h as f32, th as f32, t) as usize;
                        let cx = lerp(cat_cx, tx as f32 + tw as f32 * 0.5, t);
                        let cy = lerp(cat_cy, ty as f32 + th as f32 * 0.5, t);
                        (w, h, (cx - w as f32 * 0.5) as i32, (cy - h as f32 * 0.5) as i32, 0)
                    } else {
                        (tw, th, tx, ty, frame_idx)
                    };

                if let Some(ref sf) = self.stretch_frames {
                    if let Some(frame_data) = sf.right.get(src_frame) {
                        let sw = width as usize / 2;
                        let sh = height as usize / 2;
                        blit_scaled(canvas, width as usize, height as usize,
                                    frame_data, sw, sh,
                                    dst_x, dst_y, cur_w, cur_h);
                    }
                }
            } else {
                let frames = match draw_state {
                    State::Idle    => &self.idle,
                    State::Typing  => &self.typing,
                    State::Drag    => &self.drag,
                    State::Bounce  => &self.bounce,
                    State::Lean    => &self.lean,
                    State::Stretch => unreachable!(),
                };
                let frame_data: &[u8] = &frames.right[frame_idx];

                let (body_y, body_h) = if draw_state == State::Idle {
                    let phase = (self.frame_time_ms as f64 / BREATHE_PERIOD_MS) * std::f64::consts::TAU;
                    let breathe_scale = 1.0 + BREATHE_AMPLITUDE * phase.sin() as f32;
                    let h = ((self.disp_h as f32 * breathe_scale).round() as i32).max(1);
                    (pos_y + self.disp_h as i32 - h, h)
                } else {
                    (pos_y, self.disp_h as i32)
                };

                if body_h == self.disp_h as i32 {
                    let src_x_off = (-pos_x).max(0) as usize;
                    let src_y_off = (-body_y).max(0) as usize;
                    let dst_x = pos_x.max(0) as usize;
                    let dst_y = body_y.max(0) as usize;
                    let copy_w = ((self.disp_w as i32 - src_x_off as i32)
                        .min(width as i32 - dst_x as i32))
                        .max(0) as usize;
                    let copy_h = ((self.disp_h as i32 - src_y_off as i32)
                        .min(height as i32 - dst_y as i32))
                        .max(0) as usize;
                    for row in 0..copy_h {
                        if copy_w == 0 { break; }
                        let src = (row + src_y_off) * self.disp_w as usize * 4 + src_x_off * 4;
                        let dst = (dst_y + row) * width as usize * 4 + dst_x * 4;
                        canvas[dst..dst + copy_w * 4]
                            .copy_from_slice(&frame_data[src..src + copy_w * 4]);
                    }
                } else {
                    blit_scaled(canvas, width as usize, height as usize,
                                frame_data, self.disp_w as usize, self.disp_h as usize,
                                pos_x, body_y, self.disp_w as usize, body_h as usize);
                }
                if matches!(draw_state, State::Idle | State::Drag | State::Bounce | State::Lean) {
                    shift_pupils(canvas, width as usize, pos_x, body_y, cursor_x, cursor_y, self.scale, self.disp_w);
                }
                if typing_heat > 0.0 {
                    tint_cat(canvas, width as usize, pos_x, body_y, self.disp_w, body_h as u32, typing_heat);
                }
            }
            buffer
        };

        let region = Region::new(&self.compositor_state).expect("region");
        if draw_state == State::Stretch {
        } else if self.dragging {
            region.wl_region().add(0, 0, width as i32, height as i32);
        } else {
            let body_t = 1 * self.scale as i32;
            let ir_x = (self.pos_x + self.body_l).max(0);
            let ir_y = (self.pos_y + body_t).max(0);
            let ir_w = (self.disp_w as i32 - self.body_l - self.body_r)
                .min(width as i32 - ir_x)
                .max(0);
            let ir_h = ((self.pos_y + self.disp_h as i32).min(height as i32) - ir_y).max(0);
            region.wl_region().add(ir_x, ir_y, ir_w, ir_h);
        }

        let layer = self.layer_surface.as_ref().unwrap();
        let surface = layer.wl_surface();
        surface.set_input_region(Some(region.wl_region()));
        surface.frame(qh, surface.clone());
        surface.damage_buffer(0, 0, width as i32, height as i32);
        buffer.attach_to(surface).expect("attach_to failed");
        surface.commit();
    }
}

impl CompositorHandler for PetApp {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        time: u32,
    ) {
        self.frame_time_ms = time;

        if self.last_anim_ms == 0 {
            self.last_anim_ms = time;
            self.state_start_ms = time;
        }

        let anim_interval = if self.state == State::Stretch { self.stretch_anim_ms } else { self.anim_ms };
        if !matches!(self.state, State::Drag | State::Typing) && time.wrapping_sub(self.last_anim_ms) >= anim_interval {
            let total = if self.state == State::Stretch {
                self.stretch_frame_count.max(1)
            } else if self.state == State::Drag {
                self.drag_frame_count.max(1)
            } else if self.state == State::Bounce {
                self.bounce_frame_count.max(1)
            } else if self.state == State::Lean {
                self.lean_frame_count.max(1)
            } else {
                self.state.frame_count()
            };
            let next = (self.frame_idx + 1) % total;
            let is_last = next == 0;
            if is_last && self.state == State::Stretch && self.stretch_hold_start.is_none() && !self.stretch_zooming_out {
                self.stretch_hold_start = Some(time);
            } else if !is_last || (self.stretch_hold_start.is_none() && !self.stretch_zooming_out) {
                self.frame_idx = next;
            }
            if is_last && matches!(self.state, State::Bounce | State::Lean) {
                self.state = State::Idle;
                self.state_start_ms = time;
                self.frame_idx = 0;
            }
            self.last_anim_ms = time;
        }

        if !self.dragging && !matches!(self.state, State::Typing | State::Drag | State::Stretch | State::Bounce | State::Lean) {
            if time.wrapping_sub(self.state_start_ms) >= self.state.duration_ms(time) {
                self.state = State::Idle;
                self.state_start_ms = time;
                self.frame_idx = 0;
            }
        }

        if self.stretch_every_secs > 0
            && self.stretch_frame_count > 0
            && !self.dragging
            && !matches!(self.state, State::Stretch | State::Typing)
            && self.last_stretch.elapsed().as_secs() >= self.stretch_every_secs
        {
            self.stretch_start_x = self.pos_x;
            self.stretch_start_y = self.pos_y;
            self.stretch_transition = 0.0;
            self.stretch_out_t = 0.0;
            self.stretch_zooming_out = false;
            self.stretch_hold_start = None;
            self.state = State::Stretch;
            self.frame_idx = 0;
            self.state_start_ms = time;
            self.last_stretch = std::time::Instant::now();
        }

        if self.state == State::Stretch && self.stretch_transition < 1.0 {
            let elapsed = time.wrapping_sub(self.state_start_ms);
            self.stretch_transition = (elapsed as f32 / 500.0).min(1.0);
        }

        if let Some(hold_start) = self.stretch_hold_start {
            if time.wrapping_sub(hold_start) >= self.stretch_hold_ms {
                self.stretch_hold_start = None;
                self.stretch_zooming_out = true;
                self.stretch_out_t = 0.0;
                self.state_start_ms = time;
            }
        }

        if self.stretch_zooming_out {
            let elapsed = time.wrapping_sub(self.state_start_ms);
            self.stretch_out_t = (elapsed as f32 / 500.0).min(1.0);
            if self.stretch_out_t >= 1.0 {
                self.stretch_zooming_out = false;
                self.stretch_out_t = 0.0;
                self.state = State::Idle;
                self.state_start_ms = time;
                self.frame_idx = 0;
            }
        }

        if self.bounce_every_secs > 0
            && self.bounce_frame_count > 0
            && !self.dragging
            && !matches!(self.state, State::Stretch | State::Typing | State::Bounce | State::Lean)
            && self.last_bounce.elapsed().as_secs() >= self.bounce_every_secs
        {
            self.state = State::Bounce;
            self.frame_idx = 0;
            self.state_start_ms = time;
            self.last_bounce = std::time::Instant::now();
        }

        if self.lean_every_secs > 0
            && self.lean_frame_count > 0
            && !self.dragging
            && !matches!(self.state, State::Stretch | State::Typing | State::Bounce | State::Lean)
            && self.last_lean.elapsed().as_secs() >= self.lean_every_secs
        {
            self.state = State::Lean;
            self.frame_idx = 0;
            self.state_start_ms = time;
            self.last_lean = std::time::Instant::now();
        }

        if self.key_pressed.load(Ordering::Relaxed) {
            self.key_pressed.store(false, Ordering::Relaxed);
            self.typing_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(300));
            self.typing_heat = (self.typing_heat + 0.08).min(1.0);
            if !self.dragging && !matches!(self.state, State::Typing | State::Stretch) {
                self.state = State::Typing;
                self.state_start_ms = time;
                self.frame_idx = 0;
            } else if self.state == State::Typing {
                let total = self.state.frame_count().max(1);
                self.frame_idx = (self.frame_idx + 1) % total;
            }
        }
        self.typing_heat = (self.typing_heat - 0.006).max(0.0);

        if self.state == State::Typing {
            if self.typing_until.map_or(true, |u| std::time::Instant::now() >= u) {
                self.state = State::Idle;
                self.state_start_ms = time;
                self.frame_idx = 0;
            }
        }

        self.draw(qh);
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl SeatHandler for PetApp {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.pointer = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for PetApp {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            match event.kind {
                PointerEventKind::Leave { .. } => {
                    self.cursor_x = self.pos_x as f64 + self.disp_w as f64 * 0.5;
                    self.cursor_y = self.pos_y as f64 + 9.0 * self.scale as f64;
                }
                PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                    self.dragging = true;
                    self.state = State::Drag;
                    self.frame_idx = 1;
                    self.drag_start_pos_x = self.pos_x;
                    self.drag_start_pos_y = self.pos_y;
                    self.drag_start_local_x = event.position.0;
                    self.drag_start_local_y = event.position.1;
                }
                PointerEventKind::Motion { .. } => {
                    self.cursor_x = event.position.0;
                    self.cursor_y = event.position.1;
                    if self.dragging {
                        let max_x = (self.width as i32 - self.disp_w as i32 + self.body_r).max(0);
                        let max_y = (self.height as i32 - self.disp_h as i32).max(0);
                        self.pos_x = (self.drag_start_pos_x
                            + (event.position.0 - self.drag_start_local_x) as i32)
                            .clamp(-self.body_l, max_x);
                        self.pos_y = (self.drag_start_pos_y
                            + (event.position.1 - self.drag_start_local_y) as i32)
                            .clamp(0, max_y);
                    }
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    if self.dragging {
                        self.dragging = false;
                        self.state = State::Idle;
                        self.state_start_ms = self.last_anim_ms;
                        self.frame_idx = 0;
                    }
                }
                _ => {}
            }
        }
    }
}

impl OutputHandler for PetApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_output::WlOutput,
    ) {
    }
}

impl ShmHandler for PetApp {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl LayerShellHandler for PetApp {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.running = false;
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if configure.new_size.0 != 0 {
            self.width = configure.new_size.0;
        }
        if configure.new_size.1 != 0 {
            self.height = configure.new_size.1;
        }

        if self.stretch_frames.is_none() && self.width > 0 && self.height > 0 && self.stretch_every_secs > 0 {
            if let Ok(img) = image::open("assets/own.png") {
                let sheet = img.into_rgba8();
                let count = count_frames_in_row(&sheet, 2);
                if count > 0 {
                    self.stretch_frame_count = count as usize;
                    self.stretch_frames = Some(load_anim(&sheet, 2, count, self.width / 2, self.height / 2));
                }
            }
            if self.stretch_frames.is_none() {
                self.stretch_frames = Some(Frames { right: vec![] });
            }
        }

        if !self.pos_y_initialized && self.width > 0 && self.height > self.disp_h {
            let m = CAT_BOTTOM_MARGIN;
            self.pos_x = if self.corner.contains("right") {
                (self.width as i32 - self.disp_w as i32 - m).max(0)
            } else {
                m
            };
            self.pos_y = if self.corner.contains("top") {
                m
            } else {
                (self.height as i32 - self.disp_h as i32 - m).max(0)
            };
            self.pos_y_initialized = true;
        }

        if self.pool.is_none() {
            self.pool = Some(
                SlotPool::new((self.width * self.height * 4 * 2) as usize, &self.shm)
                    .expect("SlotPool::new failed"),
            );
        }

        self.draw(qh);
    }
}

impl ProvidesRegistryState for PetApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(PetApp);
delegate_output!(PetApp);
delegate_shm!(PetApp);
delegate_layer!(PetApp);
delegate_registry!(PetApp);
delegate_seat!(PetApp);
delegate_pointer!(PetApp);

fn main() {
    let cfg = load_config();
    let disp_w = FRAME_W * cfg.scale;
    let disp_h = FRAME_H * cfg.scale;
    let body_l = BODY_L_SRC * cfg.scale as i32;
    let body_r = BODY_R_SRC * cfg.scale as i32;

    let own_sheet = image::open("assets/own.png")
        .expect("assets/own.png not found")
        .into_rgba8();
    let idle   = load_anim(&own_sheet, 0, 4, disp_w, disp_h);
    let typing = load_anim(&own_sheet, 1, 4, disp_w, disp_h);
    let drag_count = count_frames_in_row(&own_sheet, 3).max(1) as usize;
    let drag   = load_anim(&own_sheet, 3, drag_count as u32, disp_w, disp_h);
    let bounce_count = count_frames_in_row(&own_sheet, 4).max(1) as usize;
    let bounce = load_anim(&own_sheet, 4, bounce_count as u32, disp_w, disp_h);
    let lean_count = count_frames_in_row(&own_sheet, 5).max(1) as usize;
    let lean = load_anim(&own_sheet, 5, lean_count as u32, disp_w, disp_h);

    let key_pressed = Arc::new(AtomicBool::new(false));
    start_keyboard_watcher(key_pressed.clone());

    let conn = Connection::connect_to_env()
        .expect("Could not connect to Wayland display. Is $WAYLAND_DISPLAY set?");
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();

    let mut app = PetApp {
        registry_state: RegistryState::new(&globals),
        compositor_state: CompositorState::bind(&globals, &qh)
            .expect("wl_compositor not available"),
        output_state: OutputState::new(&globals, &qh),
        shm: Shm::bind(&globals, &qh).expect("wl_shm not available"),
        layer_shell: LayerShell::bind(&globals, &qh)
            .expect("zwlr_layer_shell_v1 not available"),
        layer_surface: None,
        pool: None,
        width: disp_w,
        height: disp_h,
        running: true,
        idle,
        typing,
        drag,
        drag_frame_count: drag_count,
        bounce,
        bounce_frame_count: bounce_count,
        lean,
        lean_frame_count: lean_count,
        state: State::Idle,
        state_start_ms: 0,
        frame_idx: 0,
        last_anim_ms: 0,
        frame_time_ms: 0,
        pos_x: 0,
        pos_y: 0,
        pos_y_initialized: false,
        seat_state: SeatState::new(&globals, &qh),
        pointer: None,
        dragging: false,
        scale: cfg.scale,
        disp_w,
        disp_h,
        body_l,
        body_r,
        anim_ms: cfg.anim_ms,
        corner: cfg.corner,
        drag_start_pos_x: 0,
        drag_start_pos_y: 0,
        drag_start_local_x: 0.0,
        drag_start_local_y: 0.0,
        cursor_x: 0.0,
        cursor_y: 0.0,
        key_pressed,
        typing_until: None,
        typing_heat: 0.0,
        stretch_frames: None,
        stretch_frame_count: 0,
        stretch_every_secs: cfg.stretch_every_secs,
        stretch_anim_ms: cfg.stretch_anim_ms,
        stretch_hold_ms: cfg.stretch_hold_ms,
        last_stretch: std::time::Instant::now(),
        stretch_transition: 1.0,
        stretch_out_t: 0.0,
        stretch_zooming_out: false,
        stretch_start_x: 0,
        stretch_start_y: 0,
        stretch_hold_start: None,
        bounce_every_secs: cfg.bounce_every_secs,
        last_bounce: std::time::Instant::now(),
        lean_every_secs: cfg.lean_every_secs,
        last_lean: std::time::Instant::now(),
    };

    let surface = app.compositor_state.create_surface(&qh);
    let layer_surface = app.layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Top,
        Some("nekopet"),
        None,
    );
    layer_surface.set_size(0, 0);
    layer_surface.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::BOTTOM | Anchor::RIGHT);
    layer_surface.set_exclusive_zone(-1);
    layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer_surface.wl_surface().commit();
    app.layer_surface = Some(layer_surface);

    while app.running {
        event_queue.blocking_dispatch(&mut app).unwrap();
    }
}
