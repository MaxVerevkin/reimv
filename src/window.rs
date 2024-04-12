use std::collections::HashSet;
use std::ffi::CString;

use wayrs_client::cstr;
use wayrs_client::object::ObjectId;
use wayrs_client::proxy::Proxy;
use wayrs_client::Connection;
use wayrs_protocols::viewporter::*;
use wayrs_protocols::xdg_shell::*;

use wayrs_client::protocol::*;
use wayrs_protocols::fractional_scale_v1::*;
use wayrs_protocols::xdg_decoration_unstable_v1::*;

use crate::globals::Globals;
use crate::EventCtx;
use crate::State;

pub struct Window {
    pub surface: WlSurface,
    pub xdg_surface: XdgSurface,
    pub xdg_toplevel: XdgToplevel,
    pub wl_buffer: WlBuffer,
    pub viewport: WpViewport,
    pub fractional_scale: Option<WpFractionalScaleV1>,

    pub outputs: HashSet<ObjectId>,
    pub scale120: Option<u32>,

    pub mapped: bool,
    pub throttle: Option<WlCallback>,
    pub throttled: bool,
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

        let bg_pix = u32::MAX / 256 * 20;
        let wl_buffer = globals
            .single_pixel_buffer_manager
            .create_u32_rgba_buffer(conn, bg_pix, bg_pix, bg_pix, bg_pix);

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
            wl_buffer,
            viewport,
            fractional_scale,

            scale120: None,
            outputs: HashSet::new(),

            mapped: false,
            throttle: None,
            throttled: false,
            width: 400,
            height: 300,
            fullscreen: false,
            closed: false,
        }
    }

    pub fn frame(state: &mut State, conn: &mut Connection<State>) {
        if !state.window.mapped {
            return;
        }

        if state.window.throttle.is_some() {
            state.window.throttled = true;
            return;
        }

        let scale120 = state
            .window
            .scale120
            .unwrap_or_else(|| state.window.get_int_scale(state) * 120);

        state.backend.render(
            conn,
            &mut state.shm_alloc,
            state.window.width,
            state.window.height,
            scale120,
            &state.img_transform,
        );

        state.window.viewport.set_destination(
            conn,
            state.window.width as i32,
            state.window.height as i32,
        );

        state.window.throttle = Some(state.window.surface.frame_with_cb(conn, |ctx| {
            assert_eq!(ctx.state.window.throttle, Some(ctx.proxy));
            ctx.state.window.throttle = None;
            if ctx.state.window.throttled {
                ctx.state.window.throttled = false;
                Self::frame(ctx.state, ctx.conn);
            }
        }));

        state.window.surface.commit(conn);
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
    Window::frame(ctx.state, ctx.conn);
}

fn xdg_surface_cb(ctx: EventCtx<XdgSurface>) {
    assert_eq!(ctx.state.window.xdg_surface, ctx.proxy);
    let xdg_surface::Event::Configure(serial) = ctx.event else {
        return;
    };
    ctx.proxy.ack_configure(ctx.conn, serial);
    if !ctx.state.window.mapped {
        ctx.state.window.mapped = true;
        ctx.state
            .window
            .surface
            .attach(ctx.conn, Some(ctx.state.window.wl_buffer), 0, 0);
        ctx.state.window.surface.damage(ctx.conn, 0, 0, 1, 1);
    }
    Window::frame(ctx.state, ctx.conn);
}

fn fractional_scale_cb(ctx: EventCtx<WpFractionalScaleV1>) {
    assert_eq!(ctx.state.window.fractional_scale, Some(ctx.proxy));
    let wp_fractional_scale_v1::Event::PreferredScale(scale120) = ctx.event else {
        return;
    };
    if ctx.state.window.scale120 != Some(scale120) {
        ctx.state.window.scale120 = Some(scale120);
        Window::frame(ctx.state, ctx.conn);
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
