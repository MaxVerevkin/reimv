use anyhow::{Context, Result};
use std::path::Path;

use resvg::{tiny_skia, usvg};
use tiny_skia::{Pixmap, PixmapMut};
use usvg::{fontdb, TreeParsing, TreeTextToPath};

pub enum Image {
    Svg { tree: resvg::Tree },
    Image { pixmap: Pixmap },
}

impl Image {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
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

                Ok(Self::Svg {
                    tree: resvg::Tree::from_usvg(&tree),
                })
            }
            _ => {
                let image = image::io::Reader::open(path)
                    .context("could not open file")?
                    .decode()
                    .context("could not decode image")?
                    .into_rgba8();
                let w = image.width();
                let h = image.height();
                let pixmap =
                    Pixmap::from_vec(image.into_vec(), tiny_skia::IntSize::from_wh(w, h).unwrap())
                        .unwrap();
                Ok(Self::Image { pixmap })
            }
        }
    }

    pub fn render(&mut self, mut canvas: PixmapMut, gui_scale: f64, scale: f64, x: f64, y: f64) {
        let transform = tiny_skia::Transform::identity()
            .post_scale(scale as f32, scale as f32)
            .post_translate(x as f32, y as f32)
            .post_scale(gui_scale as f32, gui_scale as f32);
        match self {
            Self::Svg { tree } => {
                tree.render(transform, &mut canvas);
            }
            Self::Image { pixmap } => {
                canvas.draw_pixmap(
                    0,
                    0,
                    pixmap.as_ref(),
                    &tiny_skia::PixmapPaint::default(),
                    transform,
                    None,
                );
            }
        }
    }
}
