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

const FRAME_W: u32 = 32;
const FRAME_H: u32 = 32;
const SCALE: u32 = 3;
const DISP_W: u32 = FRAME_W * SCALE;
const DISP_H: u32 = FRAME_H * SCALE;
const WALK_PX: i32 = 2;
const ANIM_MS: u32 = 120;
const CAT_BOTTOM_MARGIN: i32 = 16;

#[derive(Clone, Copy, PartialEq)]
enum State {
    Walk,
    Idle,
    Sleep,
    Alert,
    Jump,
}

impl State {
    fn frame_count(self) -> usize {
        match self {
            State::Walk  => 8,
            State::Idle  => 4,
            State::Sleep => 4,
            State::Alert => 4,
            State::Jump  => 4,
        }
    }

    fn duration_ms(self, time: u32) -> u32 {
        match self {
            State::Walk  => 8_000 + (time % 6_000),
            State::Idle  => 3_000 + (time % 4_000),
            State::Sleep => 8_000 + (time % 8_000),
            State::Alert | State::Jump => u32::MAX,
        }
    }

    fn next(self, time: u32) -> State {
        match self {
            State::Walk  => State::Idle,
            State::Idle  => if (time / 1_000) % 3 == 0 { State::Sleep } else { State::Walk },
            State::Sleep => State::Idle,
            State::Alert | State::Jump => State::Idle,
        }
    }
}

struct Frames {
    right: Vec<Vec<u8>>,
    left: Vec<Vec<u8>>,
}

fn load_anim(
    sheet: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    row: u32,
    count: u32,
) -> Frames {
    let right: Vec<Vec<u8>> = (0..count)
        .map(|col| {
            let buf = image::ImageBuffer::from_fn(FRAME_W, FRAME_H, |px, py| {
                *sheet.get_pixel(col * FRAME_W + px, row * FRAME_H + py)
            });
            image::DynamicImage::from(buf)
                .resize_exact(DISP_W, DISP_H, image::imageops::FilterType::Nearest)
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
    let left = right.iter().map(|f| flip_h(f, DISP_W, DISP_H)).collect();
    Frames { right, left }
}

fn flip_h(frame: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u8; frame.len()];
    for y in 0..h {
        for x in 0..w {
            let src = (y * w + (w - 1 - x)) * 4;
            let dst = (y * w + x) * 4;
            out[dst..dst + 4].copy_from_slice(&frame[src..src + 4]);
        }
    }
    out
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
    walk: Frames,
    idle: Frames,
    sleep: Frames,
    alert: Frames,
    jump: Frames,
    state: State,
    state_start_ms: u32,
    frame_idx: usize,
    last_anim_ms: u32,
    pos_x: i32,
    pos_y: i32,
    vel_x: i32,
    pos_y_initialized: bool,
    seat_state: SeatState,
    pointer: Option<wl_pointer::WlPointer>,
    dragging: bool,
    drag_start_pos_x: i32,
    drag_start_pos_y: i32,
    drag_start_local_x: f64,
    drag_start_local_y: f64,
}

impl PetApp {
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let width = self.width;
        let height = self.height;
        if width == 0 || height == 0 {
            return;
        }

        let frames = match self.state {
            State::Walk  => &self.walk,
            State::Idle  => &self.idle,
            State::Sleep => &self.sleep,
            State::Alert => &self.alert,
            State::Jump  => &self.jump,
        };
        let frame_data: &[u8] = if self.vel_x >= 0 {
            &frames.right[self.frame_idx]
        } else {
            &frames.left[self.frame_idx]
        };

        let cat_x = self.pos_x.clamp(0, width as i32 - DISP_W as i32) as usize;
        let cat_y = self.pos_y.clamp(0, height as i32 - DISP_H as i32) as usize;

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
            for row in 0..DISP_H as usize {
                let src = row * DISP_W as usize * 4;
                let dst = (cat_y + row) * width as usize * 4 + cat_x * 4;
                canvas[dst..dst + DISP_W as usize * 4]
                    .copy_from_slice(&frame_data[src..src + DISP_W as usize * 4]);
            }
            buffer
        };

        let region = Region::new(&self.compositor_state).expect("region");
        if self.dragging {
            region.wl_region().add(0, 0, width as i32, height as i32);
        } else {
            region.wl_region().add(cat_x as i32, cat_y as i32, DISP_W as i32, DISP_H as i32);
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
        if self.last_anim_ms == 0 {
            self.last_anim_ms = time;
            self.state_start_ms = time;
        }

        if time.wrapping_sub(self.last_anim_ms) >= ANIM_MS {
            let next = (self.frame_idx + 1) % self.state.frame_count();
            if next == 0 && self.state == State::Jump {
                self.state = State::Idle;
                self.state_start_ms = time;
                self.frame_idx = 0;
            } else {
                self.frame_idx = next;
            }
            self.last_anim_ms = time;
        }

        if !self.dragging && !matches!(self.state, State::Alert | State::Jump) {
            if time.wrapping_sub(self.state_start_ms) >= self.state.duration_ms(time) {
                self.state = self.state.next(time);
                self.state_start_ms = time;
                self.frame_idx = 0;
            }

            if self.state == State::Walk {
                self.pos_x += self.vel_x;
                let max_x = (self.width as i32 - DISP_W as i32).max(0);
                if self.pos_x <= 0 {
                    self.pos_x = 0;
                    self.vel_x = WALK_PX;
                } else if self.pos_x >= max_x {
                    self.pos_x = max_x;
                    self.vel_x = -WALK_PX;
                }
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
                PointerEventKind::Enter { .. } if !self.dragging => {
                    self.state = State::Alert;
                    self.frame_idx = 0;
                }
                PointerEventKind::Leave { .. } if !self.dragging => {
                    if self.state == State::Alert {
                        self.state = State::Idle;
                        self.state_start_ms = self.last_anim_ms;
                        self.frame_idx = 0;
                    }
                }
                PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                    self.dragging = true;
                    self.drag_start_pos_x = self.pos_x;
                    self.drag_start_pos_y = self.pos_y;
                    self.drag_start_local_x = event.position.0;
                    self.drag_start_local_y = event.position.1;
                }
                PointerEventKind::Motion { .. } if self.dragging => {
                    let max_x = (self.width as i32 - DISP_W as i32).max(0);
                    let max_y = (self.height as i32 - DISP_H as i32).max(0);
                    self.pos_x = (self.drag_start_pos_x
                        + (event.position.0 - self.drag_start_local_x) as i32)
                        .clamp(0, max_x);
                    self.pos_y = (self.drag_start_pos_y
                        + (event.position.1 - self.drag_start_local_y) as i32)
                        .clamp(0, max_y);
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    self.dragging = false;
                    self.state = State::Jump;
                    self.state_start_ms = self.last_anim_ms;
                    self.frame_idx = 0;
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

        if !self.pos_y_initialized && self.height > DISP_H {
            self.pos_y = self.height as i32 - DISP_H as i32 - CAT_BOTTOM_MARGIN;
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
    let sheet = image::open("assets/sheet.png")
        .expect("Failed to open assets/sheet.png")
        .into_rgba8();

    let walk  = load_anim(&sheet, 4, 8);
    let idle  = load_anim(&sheet, 0, 4);
    let sleep = load_anim(&sheet, 6, 4);
    let alert = load_anim(&sheet, 7, 4);
    let jump  = load_anim(&sheet, 8, 4);

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
        width: DISP_W,
        height: DISP_H,
        running: true,
        walk,
        idle,
        sleep,
        alert,
        jump,
        state: State::Walk,
        state_start_ms: 0,
        frame_idx: 0,
        last_anim_ms: 0,
        pos_x: 0,
        pos_y: 0,
        vel_x: WALK_PX,
        pos_y_initialized: false,
        seat_state: SeatState::new(&globals, &qh),
        pointer: None,
        dragging: false,
        drag_start_pos_x: 0,
        drag_start_pos_y: 0,
        drag_start_local_x: 0.0,
        drag_start_local_y: 0.0,
    };

    let surface = app.compositor_state.create_surface(&qh);
    let layer_surface = app.layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Top,
        Some("pet-cat"),
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
