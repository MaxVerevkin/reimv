use std::collections::HashSet;
use std::ffi::CString;

use resvg::tiny_skia;

use wayrs_client::cstr;
use wayrs_client::object::ObjectId;
use wayrs_client::proxy::Proxy;
use wayrs_client::Connection;
use wayrs_protocols::viewporter::*;
use wayrs_protocols::xdg_shell::*;

use wayrs_client::protocol::*;
use wayrs_protocols::fractional_scale_v1::*;
use wayrs_protocols::xdg_decoration_unstable_v1::*;
use wayrs_utils::shm_alloc::BufferSpec;

use crate::globals::Globals;
use crate::EventCtx;
use crate::State;

pub struct Window {
    pub surface: WlSurface,
    pub xdg_surface: XdgSurface,
    pub xdg_toplevel: XdgToplevel,
    pub viewport: WpViewport,
    pub fractional_scale: Option<WpFractionalScaleV1>,
    pub xdg_decoration: Option<ZxdgToplevelDecorationV1>,

    pub outputs: HashSet<ObjectId>,
    pub scale120: Option<u32>,

    pub mapped: bool,
    pub wl_frame_cb: Option<WlCallback>,
    pub width: u32,
    pub height: u32,
    pub fullscreen: bool,
    pub closed: bool,
}

impl Window {
    pub fn new(conn: &mut Connection<State>, globals: &Globals, title: String) -> Self {
        let surface = globals
            .wl_compositor
            .create_surface_with_cb(conn, wl_surface_cb);

        let viewport = globals.wp_viewporter.get_viewport(conn, surface);

        let fractional_scale = globals
            .wp_fractional_scale_manager
            .map(|fs| fs.get_fractional_scale_with_cb(conn, surface, fractional_scale_cb));

        let xdg_surface =
            globals
                .xdg_wm_base
                .get_xdg_surface_with_cb(conn, surface, xdg_surface_cb);

        let xdg_toplevel = xdg_surface.get_toplevel_with_cb(conn, xdg_toplevel_cb);
        xdg_toplevel.set_app_id(conn, cstr!("reimv").into());
        xdg_toplevel.set_title(conn, CString::new(title).expect("title has nul bytes"));

        // We don't care what the compositor prefers, thus no callback. There are no plans to
        // implement CSD.
        let xdg_decoration = globals
            .xdg_decoration_manager
            .map(|fs| fs.get_toplevel_decoration(conn, xdg_toplevel));
        if let Some(xdg_decoration) = xdg_decoration {
            xdg_decoration.set_mode(conn, zxdg_toplevel_decoration_v1::Mode::ServerSide);
        }

        surface.commit(conn);

        Self {
            surface,
            xdg_surface,
            xdg_toplevel,
            viewport,
            fractional_scale,
            xdg_decoration,

            scale120: None,
            outputs: HashSet::new(),

            mapped: false,
            wl_frame_cb: None,
            width: 400,
            height: 300,
            fullscreen: false,
            closed: false,
        }
    }

    pub fn request_frame(&mut self, conn: &mut Connection<State>) {
        if self.mapped && self.wl_frame_cb.is_none() {
            self.wl_frame_cb = Some(self.surface.frame_with_cb(conn, |ctx| {
                assert_eq!(ctx.state.window.wl_frame_cb, Some(ctx.proxy));
                ctx.state.window.wl_frame_cb = None;
                Self::frame(ctx.state, ctx.conn);
            }));
            self.surface.commit(conn);
        }
    }

    pub fn frame(state: &mut State, conn: &mut Connection<State>) {
        let this = &state.window;
        assert!(this.mapped);

        let (pix_width, pix_height, scale_f) = match this.scale120 {
            Some(scale120) => (
                // rounding halfway away from zero
                (this.width * scale120 + 60) / 120,
                (this.height * scale120 + 60) / 120,
                scale120 as f64 / 120.0,
            ),
            None => {
                let scale = this.get_int_scale(state);
                (this.width * scale, this.height * scale, scale as f64)
            }
        };

        let (buffer, canvas) = state.shm_alloc.alloc_buffer(
            conn,
            BufferSpec {
                width: pix_width,
                height: pix_height,
                stride: pix_width * 4,
                format: wl_shm::Format::Abgr8888,
            },
        );

        canvas.fill(20);

        let canvas = tiny_skia::PixmapMut::from_bytes(canvas, pix_width, pix_height).unwrap();

        state.backend.render(
            canvas,
            scale_f,
            state.img_transform.scale,
            state.img_transform.x,
            state.img_transform.y,
        );

        this.surface
            .attach(conn, Some(buffer.into_wl_buffer()), 0, 0);
        this.viewport
            .set_destination(conn, this.width as i32, this.height as i32);
        this.surface
            .damage(conn, 0, 0, this.width as i32, this.height as i32);

        this.surface.commit(conn);
    }

    pub fn get_int_scale(&self, state: &State) -> u32 {
        match self.scale120 {
            Some(scale120) => (scale120 + 119) / 120,
            None => state
                .outputs
                .iter()
                .filter(|o| self.outputs.contains(&o.wl.id()))
                .map(|o| o.scale)
                .max()
                .unwrap_or(1),
        }
    }

    pub fn toggle_fullscreen(&self, conn: &mut Connection<State>) {
        if self.fullscreen {
            self.xdg_toplevel.unset_fullscreen(conn);
        } else {
            self.xdg_toplevel.set_fullscreen(conn, None);
        }
    }
}

fn wl_surface_cb(ctx: EventCtx<WlSurface>) {
    assert_eq!(ctx.state.window.surface, ctx.proxy);
    match ctx.event {
        wl_surface::Event::Enter(output) => {
            ctx.state.window.outputs.insert(output);
        }
        wl_surface::Event::Leave(output) => {
            ctx.state.window.outputs.remove(&output);
        }
        wl_surface::Event::PreferredBufferScale(_scale) => {
            // TODO
        }
        _ => (),
    }
    ctx.state.window.request_frame(ctx.conn);
}

fn xdg_surface_cb(ctx: EventCtx<XdgSurface>) {
    assert_eq!(ctx.state.window.xdg_surface, ctx.proxy);
    let xdg_surface::Event::Configure(serial) = ctx.event else {
        return;
    };
    ctx.proxy.ack_configure(ctx.conn, serial);
    if ctx.state.window.mapped {
        // NOTE: this is because of a river bug: https://github.com/riverwm/river/issues/807
        // ctx.state.window.request_frame(conn);
        Window::frame(ctx.state, ctx.conn);
    } else {
        ctx.state.window.mapped = true;
        Window::frame(ctx.state, ctx.conn);
    }
}

fn fractional_scale_cb(ctx: EventCtx<WpFractionalScaleV1>) {
    assert_eq!(ctx.state.window.fractional_scale, Some(ctx.proxy));
    let wp_fractional_scale_v1::Event::PreferredScale(scale120) = ctx.event else {
        return;
    };
    if ctx.state.window.scale120 != Some(scale120) {
        ctx.state.window.scale120 = Some(scale120);
        ctx.state.window.request_frame(ctx.conn);
    }
}

fn xdg_toplevel_cb(ctx: EventCtx<XdgToplevel>) {
    assert_eq!(ctx.state.window.xdg_toplevel, ctx.proxy);
    match ctx.event {
        xdg_toplevel::Event::Configure(args) => {
            if args.width > 0 {
                ctx.state.window.width = args.width as u32;
            }
            if args.height > 0 {
                ctx.state.window.height = args.height as u32;
            }
            ctx.state.window.fullscreen = args
                .states
                .chunks_exact(4)
                .map(|x| u32::from_ne_bytes(x.try_into().unwrap()))
                .filter_map(|x| xdg_toplevel::State::try_from(x).ok())
                .any(|x| x == xdg_toplevel::State::Fullscreen);
        }
        xdg_toplevel::Event::Close => {
            ctx.state.window.closed = true;
            ctx.conn.break_dispatch_loop();
        }
        xdg_toplevel::Event::ConfigureBounds(_) => (),
        xdg_toplevel::Event::WmCapabilities(_) => (),
        _ => (),
    }
}
