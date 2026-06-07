use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};

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
}

impl PetApp {
    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        let width = self.width;
        let height = self.height;

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

            let cx = width / 2;
            let cy = height / 2;
            let radius = (width.min(height) / 2).saturating_sub(8);

            for (i, pixel) in canvas.chunks_exact_mut(4).enumerate() {
                let x = (i as u32) % width;
                let y = (i as u32) / width;
                let dx = x as i32 - cx as i32;
                let dy = y as i32 - cy as i32;

                if dx * dx + dy * dy < (radius * radius) as i32 {
                    pixel.copy_from_slice(&[0, 165, 255, 220]);
                } else {
                    pixel.copy_from_slice(&[0, 0, 0, 0]);
                }
            }

            buffer
        };

        let surface = self.layer_surface.as_ref().unwrap().wl_surface();
        surface.damage_buffer(0, 0, width as i32, height as i32);
        buffer.attach_to(surface).expect("attach_to failed");
        surface.commit();
    }
}

impl CompositorHandler for PetApp {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {}

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.draw(qh);
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {}

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {}
}

impl OutputHandler for PetApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
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
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if configure.new_size.0 != 0 {
            self.width = configure.new_size.0;
        }
        if configure.new_size.1 != 0 {
            self.height = configure.new_size.1;
        }

        if self.pool.is_none() {
            self.pool = Some(
                SlotPool::new((self.width * self.height * 4) as usize, &self.shm)
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
    registry_handlers![OutputState];
}

delegate_compositor!(PetApp);
delegate_output!(PetApp);
delegate_shm!(PetApp);
delegate_layer!(PetApp);
delegate_registry!(PetApp);

fn main() {
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
            .expect("zwlr_layer_shell_v1 not available — is the compositor wlroots-based?"),
        layer_surface: None,
        pool: None,
        width: 128,
        height: 128,
        running: true,
    };

    let surface = app.compositor_state.create_surface(&qh);

    let layer_surface = app.layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Top,
        Some("pet-cat"),
        None,
    );

    layer_surface.set_size(app.width, app.height);
    layer_surface.set_anchor(Anchor::BOTTOM | Anchor::RIGHT);
    layer_surface.set_margin(0, 16, 16, 0);
    layer_surface.set_exclusive_zone(-1);
    layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer_surface.wl_surface().commit();

    app.layer_surface = Some(layer_surface);

    while app.running {
        event_queue.blocking_dispatch(&mut app).unwrap();
    }
}
