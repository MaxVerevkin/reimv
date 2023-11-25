use std::path::Path;

use wayrs_client::protocol::*;
use wayrs_client::wire::Fixed;
use wayrs_client::Connection;
use wayrs_protocols::viewporter::*;
use wayrs_utils::shm_alloc::{BufferSpec, ShmAlloc};

use anyhow::{Context, Result};
use resvg::{tiny_skia, usvg};
use usvg::{fontdb, TreeParsing, TreeTextToPath};

use crate::globals::Globals;
use crate::State;

pub struct Image {
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
    kind: ImageKind,
}

enum ImageKind {
    Svg { tree: resvg::Tree },
    Image { width: u32, height: u32 },
}

#[derive(Debug, Clone, Copy)]
pub struct ImageTransform {
    /// Y-offset in surface local coordinates
    pub x: f32,
    /// Y-offset in surface local coordinates
    pub y: f32,
    /// Scale
    pub scale: f32,
}

impl Image {
    pub fn from_file(
        path: impl AsRef<Path>,
        main_surface: WlSurface,
        globals: &Globals,
        shm: &mut ShmAlloc,
        conn: &mut Connection<State>,
    ) -> Result<Self> {
        let surface = globals.wl_compositor.create_surface(conn);
        let subsurface = globals
            .wl_subcompositor
            .get_subsurface(conn, surface, main_surface);
        let viewport = globals.wp_viewporter.get_viewport(conn, surface);

        let empty_reg = globals.wl_compositor.create_region(conn);
        surface.set_input_region(conn, Some(empty_reg));
        empty_reg.destroy(conn);

        match path.as_ref().extension().and_then(|ext| ext.to_str()) {
            Some("svg") => {
                let mut opt = usvg::Options::default();
                opt.resources_dir = std::fs::canonicalize(path.as_ref())
                    .ok()
                    .and_then(|p| p.parent().map(Into::into));

                let buf = std::fs::read(path).context("could not read file")?;
                let mut tree = usvg::Tree::from_data(&buf, &usvg::Options::default())?;

                let mut fontdb = fontdb::Database::new();
                fontdb.load_system_fonts();
                tree.convert_text(&fontdb);

                Ok(Self {
                    surface,
                    subsurface,
                    viewport,
                    kind: ImageKind::Svg {
                        tree: resvg::Tree::from_usvg(&tree),
                    },
                })
            }
            _ => {
                let image = image::io::Reader::open(path)
                    .context("could not open file")?
                    .decode()
                    .context("could not decode image")?
                    .into_rgba8();
                let width = image.width();
                let height = image.height();

                let (buffer, canvas) = shm.alloc_buffer(
                    conn,
                    BufferSpec {
                        width,
                        height,
                        stride: width * 4,
                        format: wl_shm::Format::Abgr8888,
                    },
                );
                canvas.copy_from_slice(image.as_raw());
                surface.attach(conn, Some(buffer.into_wl_buffer()), 0, 0);

                Ok(Self {
                    surface,
                    subsurface,
                    viewport,
                    kind: ImageKind::Image { width, height },
                })
            }
        }
    }

    pub fn render(
        &mut self,
        conn: &mut Connection<State>,
        shm: &mut ShmAlloc,
        win_width: u32,
        win_height: u32,
        ui_scale120: u32,
        img_transform: &ImageTransform,
    ) {
        match &mut self.kind {
            ImageKind::Svg { tree } => {
                let transform = tiny_skia::Transform::identity()
                    .post_scale(img_transform.scale, img_transform.scale)
                    .post_translate(img_transform.x, img_transform.y)
                    .post_scale(ui_scale120 as f32 / 120.0, ui_scale120 as f32 / 120.0);

                // Round halfway away from zero
                let pix_width = (win_width * ui_scale120 + 60) / 120;
                let pix_height = (win_height * ui_scale120 + 60) / 120;

                let (buffer, canvas) = shm.alloc_buffer(
                    conn,
                    BufferSpec {
                        width: pix_width,
                        height: pix_height,
                        stride: pix_width * 4,
                        format: wl_shm::Format::Abgr8888,
                    },
                );
                canvas.fill(20);

                let mut canvas =
                    tiny_skia::PixmapMut::from_bytes(canvas, pix_width, pix_height).unwrap();

                tree.render(transform, &mut canvas);

                self.surface
                    .attach(conn, Some(buffer.into_wl_buffer()), 0, 0);
                self.viewport
                    .set_destination(conn, win_width as i32, win_height as i32);
                self.surface.damage(conn, 0, 0, i32::MAX, i32::MAX);
            }
            ImageKind::Image { width, height } => {
                let transform = tiny_skia::Transform::identity()
                    .post_scale(img_transform.scale, img_transform.scale)
                    .post_translate(img_transform.x, img_transform.y);
                let transform_inv = tiny_skia::Transform::identity()
                    .pre_scale(img_transform.scale.recip(), img_transform.scale.recip())
                    .pre_translate(-img_transform.x, -img_transform.y);

                let window =
                    tiny_skia::Rect::from_xywh(0.0, 0.0, win_width as f32, win_height as f32)
                        .unwrap();

                let dst = tiny_skia::Rect::from_xywh(0.0, 0.0, *width as f32, *height as f32)
                    .unwrap()
                    .transform(transform)
                    .unwrap()
                    .intersect(&window);

                match dst {
                    Some(dst) if dst.width() >= 1.0 && dst.height() >= 1.0 => {
                        let src = dst.transform(transform_inv).unwrap();
                        self.subsurface
                            .set_position(conn, dst.x() as i32, dst.y() as i32);
                        self.viewport.set_destination(
                            conn,
                            dst.width() as i32,
                            dst.height() as i32,
                        );
                        self.viewport.set_source(
                            conn,
                            // TODO: upstream float -> fixed conversion to wayrs-client
                            Fixed((src.x() * 256.0) as i32),
                            Fixed((src.y() * 256.0) as i32),
                            Fixed((src.width() * 256.0) as i32),
                            Fixed((src.height() * 256.0) as i32),
                        );
                        self.surface.commit(conn);
                    }
                    _ => {
                        // HACK
                        self.subsurface.set_position(conn, 0, 0);
                        self.viewport.set_destination(conn, 1, 1);
                    }
                }
            }
        }

        self.surface.commit(conn);
    }
}
