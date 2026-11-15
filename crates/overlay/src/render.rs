//! Render backend: one D3D11 device shared by Direct2D, DirectWrite, and a DirectComposition visual tree.
//!
//! Each widget gets a [`Layer`]: a composition visual with its own surface. Drawing updates only the requested
//! rectangle of that surface, and nothing is committed unless something was drawn. All COM `unsafe` for
//! rendering lives here; widgets draw through the safe [`Canvas`].

use std::collections::HashMap;

use windows::Win32::Foundation::{HMODULE, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW,
    D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_CAP_STYLE_FLAT, D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_LINE_JOIN_ROUND,
    D2D1_STROKE_STYLE_PROPERTIES1, D2D1_STROKE_TRANSFORM_TYPE_NORMAL, D2D1CreateFactory,
    ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1, ID2D1SolidColorBrush,
    ID2D1StrokeStyle1,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDesktopDevice, IDCompositionSurface,
    IDCompositionTarget, IDCompositionVisual, IDCompositionVisual2, IDCompositionVisual3,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::{HSTRING, Interface, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::config;
use crate::platform::OverlayWindow;

/// A rendering failure. Device loss surfaces as an error; the app rebuilds the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderError {
    pub call: &'static str,
    pub code: i32,
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed with HRESULT {:#010x}", self.call, self.code)
    }
}

impl std::error::Error for RenderError {}

fn fail(call: &'static str) -> impl FnOnce(windows::core::Error) -> RenderError {
    move |e| RenderError {
        call,
        code: e.code().0,
    }
}

/// Straight (non-premultiplied) RGBA color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba(0x0000_0000);
    pub const WHITE: Self = Self::rgba(0xFFFF_FFFF);

    /// From `0xRRGGBBAA`.
    pub const fn rgba(v: u32) -> Self {
        Self {
            r: ((v >> 24) & 0xFF) as f32 / 255.0,
            g: ((v >> 16) & 0xFF) as f32 / 255.0,
            b: ((v >> 8) & 0xFF) as f32 / 255.0,
            a: (v & 0xFF) as f32 / 255.0,
        }
    }

    /// From `0xRRGGBB` and an alpha.
    pub const fn rgb(v: u32, a: f32) -> Self {
        let c = Self::rgba(v << 8);
        Self { a, ..c }
    }

    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: self.r,
            g: self.g,
            b: self.b,
            a: self.a,
        }
    }
}

/// A rectangle in layer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    fn d2d(self) -> D2D_RECT_F {
        D2D_RECT_F {
            left: self.x,
            top: self.y,
            right: self.x + self.w,
            bottom: self.y + self.h,
        }
    }
}

/// Horizontal text alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Text sizes available to widgets, in widget pixels; formats are created once per text scale, at these sizes
/// times the scale.
pub const TEXT_SIZES: [f32; 4] = [12.0, 14.0, 18.0, 26.0];

#[derive(Debug)]
struct Formats {
    /// Indexed by size, then alignment.
    by_size: Vec<[IDWriteTextFormat; 3]>,
}

impl Formats {
    /// The formats for [`TEXT_SIZES`] times `scale`.
    fn new(dwrite: &IDWriteFactory, scale: f32) -> Result<Self, RenderError> {
        let family = HSTRING::from("Segoe UI");
        let mut by_size = Vec::with_capacity(TEXT_SIZES.len());
        for size in TEXT_SIZES.map(|s| s * scale) {
            let make = |align: DWRITE_TEXT_ALIGNMENT| -> Result<IDWriteTextFormat, RenderError> {
                // SAFETY: plain factory call with static strings; the returned format is owned by us.
                unsafe {
                    let format = dwrite
                        .CreateTextFormat(
                            &family,
                            None,
                            DWRITE_FONT_WEIGHT_SEMI_BOLD,
                            DWRITE_FONT_STYLE_NORMAL,
                            DWRITE_FONT_STRETCH_NORMAL,
                            size,
                            w!("en-us"),
                        )
                        .map_err(fail("CreateTextFormat"))?;
                    format
                        .SetTextAlignment(align)
                        .map_err(fail("SetTextAlignment"))?;
                    format
                        .SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)
                        .map_err(fail("SetParagraphAlignment"))?;
                    format
                        .SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)
                        .map_err(fail("SetWordWrapping"))?;
                    Ok(format)
                }
            };
            by_size.push([
                make(DWRITE_TEXT_ALIGNMENT_LEADING)?,
                make(DWRITE_TEXT_ALIGNMENT_CENTER)?,
                make(DWRITE_TEXT_ALIGNMENT_TRAILING)?,
            ]);
        }
        Ok(Self { by_size })
    }

    /// Size index and alignment column for a request.
    fn index(size: f32, align: Align) -> (usize, usize) {
        let index = TEXT_SIZES
            .iter()
            .position(|&s| s >= size)
            .unwrap_or(TEXT_SIZES.len() - 1);
        let column = match align {
            Align::Left => 0,
            Align::Center => 1,
            Align::Right => 2,
        };
        (index, column)
    }
}

/// Characters drawn from the glyph atlas: what numbers, gaps, times, and units are made of.
const ATLAS_CHARS: &str = "0123456789.,:+-/#%kL ";

#[derive(Debug, Clone, Copy)]
struct Glyph {
    source: D2D_RECT_F,
    advance: f32,
}

/// One text size's glyphs, rendered white into a bitmap once and drawn as colored opacity masks.
#[derive(Debug)]
struct AtlasFace {
    bitmap: ID2D1Bitmap1,
    glyphs: [Option<Glyph>; 128],
    cell_height: f32,
}

impl AtlasFace {
    fn glyph(&self, c: char) -> Option<Glyph> {
        self.glyphs.get(c as usize).copied().flatten()
    }

    fn covers(&self, text: &str) -> bool {
        text.chars().all(|c| self.glyph(c).is_some())
    }
}

fn build_atlas(
    d2d: &ID2D1Device,
    dwrite: &IDWriteFactory,
    formats: &Formats,
) -> Result<Vec<AtlasFace>, RenderError> {
    let chars: Vec<char> = ATLAS_CHARS.chars().collect();
    let mut faces = Vec::with_capacity(TEXT_SIZES.len());
    for sizes in &formats.by_size {
        let format = &sizes[0];
        let mut advances = Vec::with_capacity(chars.len());
        let mut line = 0.0f32;
        for &c in &chars {
            let mut buffer = [0u16; 2];
            let unit = c.encode_utf16(&mut buffer);
            let mut metrics = DWRITE_TEXT_METRICS::default();
            // SAFETY: plain DirectWrite calls; the layout is released at the end of the iteration.
            unsafe {
                let layout = dwrite
                    .CreateTextLayout(unit, format, 1000.0, 1000.0)
                    .map_err(fail("CreateTextLayout"))?;
                layout
                    .GetMetrics(&mut metrics)
                    .map_err(fail("GetMetrics"))?;
            }
            advances.push(metrics.widthIncludingTrailingWhitespace);
            line = line.max(metrics.height);
        }
        let cell_w = advances.iter().fold(0.0f32, |a, &b| a.max(b)).ceil() + 2.0;
        let cell_h = line.ceil() + 2.0;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
            colorContext: core::mem::ManuallyDrop::new(None),
        };
        let size = D2D_SIZE_U {
            width: (cell_w * chars.len() as f32) as u32,
            height: cell_h as u32,
        };
        let mut glyphs = [None; 128];
        // SAFETY: a private device context renders into a new target bitmap between BeginDraw and EndDraw.
        let bitmap = unsafe {
            let context = d2d
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
                .map_err(fail("CreateDeviceContext"))?;
            let bitmap = context
                .CreateBitmap(size, None, 0, &props)
                .map_err(fail("CreateBitmap"))?;
            let white = context
                .CreateSolidColorBrush(&Color::WHITE.d2d(), None)
                .map_err(fail("CreateSolidColorBrush"))?;
            context.SetTarget(&bitmap);
            context.SetDpi(96.0, 96.0);
            context.BeginDraw();
            context.Clear(Some(&Color::TRANSPARENT.d2d()));
            for (n, (&c, &advance)) in chars.iter().zip(&advances).enumerate() {
                let x = n as f32 * cell_w;
                let mut buffer = [0u16; 2];
                let unit = c.encode_utf16(&mut buffer);
                let cell = D2D_RECT_F {
                    left: x,
                    top: 0.0,
                    right: x + cell_w,
                    bottom: cell_h,
                };
                context.DrawText(
                    unit,
                    format,
                    &cell,
                    &white,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                if let Some(slot) = glyphs.get_mut(c as usize) {
                    *slot = Some(Glyph {
                        source: D2D_RECT_F {
                            left: x,
                            top: 0.0,
                            right: x + advance,
                            bottom: cell_h,
                        },
                        advance,
                    });
                }
            }
            context.EndDraw(None, None).map_err(fail("EndDraw"))?;
            bitmap
        };
        faces.push(AtlasFace {
            bitmap,
            glyphs,
            cell_height: cell_h,
        });
    }
    Ok(faces)
}

/// A text's layouts, one per box (width, height) it is drawn in.
type Boxes = Vec<(f32, f32, IDWriteTextLayout)>;

/// Text layouts for strings that are not in the atlas, created on first use and reused.
#[derive(Debug, Default)]
struct LayoutCache {
    /// Per (size, alignment), text to its layouts, one per box (width, height) it is drawn in: a text drawn in two
    /// boxes, such as an arrow in two columns, keeps both instead of making one again at every draw.
    maps: Vec<HashMap<String, Boxes>>,
    /// Layouts held.
    entries: usize,
}

/// More cached layouts than this clears the cache, bounding memory if text keeps changing.
const MAX_CACHED_LAYOUTS: usize = 512;

/// Device-wide state shared by every window and layer.
#[derive(Debug)]
pub struct Renderer {
    _d3d: ID3D11Device,
    _d2d: ID2D1Device,
    /// For path geometries, which belong to the factory rather than the device.
    factory: ID2D1Factory1,
    /// Round joins and caps, for polylines.
    round: ID2D1StrokeStyle1,
    dcomp: IDCompositionDesktopDevice,
    brush: ID2D1SolidColorBrush,
    dwrite: IDWriteFactory,
    formats: Formats,
    atlas: Vec<AtlasFace>,
    layouts: LayoutCache,
    text: Vec<u16>,
    /// Whether a layer was drawn since the last commit.
    dirty: bool,
    /// `[general] background_opacity`, in percent, for [`Canvas::fill_background`].
    background: u8,
    /// `[general] text_size` as a scale ([`config::text_scale`]): the formats and the atlas are made at
    /// [`TEXT_SIZES`] times it, and the layers of widgets that scale with the text are drawn at it.
    text_scale: f32,
    pub software: bool,
}

fn create_d3d(driver: D3D_DRIVER_TYPE) -> Result<ID3D11Device, RenderError> {
    let mut device = None;
    // SAFETY: standard device creation with out pointers to locals.
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .map_err(fail("D3D11CreateDevice"))?;
    device.ok_or(RenderError {
        call: "D3D11CreateDevice",
        code: 0,
    })
}

impl Renderer {
    /// Creates the device on the default GPU, falling back to the WARP software rasterizer, with its text at
    /// `text_scale` (see [`Renderer::set_text_scale`]).
    pub fn new(text_scale: f32) -> Result<Self, RenderError> {
        let (d3d, software) = match create_d3d(D3D_DRIVER_TYPE_HARDWARE) {
            Ok(device) => (device, false),
            Err(_) => (create_d3d(D3D_DRIVER_TYPE_WARP)?, true),
        };
        // SAFETY: COM calls on the live device; every returned interface is owned by `Renderer`.
        unsafe {
            let dxgi: IDXGIDevice = d3d.cast().map_err(fail("IDXGIDevice"))?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .map_err(fail("D2D1CreateFactory"))?;
            let d2d = factory
                .CreateDevice(&dxgi)
                .map_err(fail("ID2D1Factory1::CreateDevice"))?;
            let dcomp: IDCompositionDesktopDevice =
                DCompositionCreateDevice2(&d2d).map_err(fail("DCompositionCreateDevice2"))?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
                .map_err(fail("DWriteCreateFactory"))?;
            // Brushes belong to the D2D device, so one brush serves every surface's device context.
            let context = d2d
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
                .map_err(fail("CreateDeviceContext"))?;
            let brush = context
                .CreateSolidColorBrush(&Color::WHITE.d2d(), None)
                .map_err(fail("CreateSolidColorBrush"))?;
            let text_scale = valid_scale(text_scale);
            let formats = Formats::new(&dwrite, text_scale)?;
            let atlas = build_atlas(&d2d, &dwrite, &formats)?;
            let round = factory
                .CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES1 {
                        startCap: D2D1_CAP_STYLE_ROUND,
                        endCap: D2D1_CAP_STYLE_ROUND,
                        dashCap: D2D1_CAP_STYLE_FLAT,
                        lineJoin: D2D1_LINE_JOIN_ROUND,
                        miterLimit: 10.0,
                        dashStyle: D2D1_DASH_STYLE_SOLID,
                        dashOffset: 0.0,
                        transformType: D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
                    },
                    None,
                )
                .map_err(fail("CreateStrokeStyle"))?;
            Ok(Self {
                _d3d: d3d,
                _d2d: d2d,
                factory,
                round,
                dcomp,
                brush,
                dwrite,
                formats,
                atlas,
                layouts: LayoutCache {
                    maps: (0..TEXT_SIZES.len() * 3).map(|_| HashMap::new()).collect(),
                    entries: 0,
                },
                text: Vec::with_capacity(256),
                dirty: false,
                background: config::DEFAULT_BACKGROUND_OPACITY,
                text_scale,
                software,
            })
        }
    }

    /// Sets `[general] background_opacity` for what is drawn from now on. Returns whether it changed, in which case
    /// every layer needs a redraw to show it.
    pub fn set_background_opacity(&mut self, percent: u8) -> bool {
        let changed = self.background != percent;
        self.background = percent;
        changed
    }

    /// The text scale the formats and the atlas are made at.
    pub fn text_scale(&self) -> f32 {
        self.text_scale
    }

    /// Makes the text formats and the glyph atlas again at `scale` (see [`config::text_scale`]) and forgets the
    /// cached layouts, which belong to the old sizes. Returns whether the scale changed, in which case every layer
    /// needs a redraw. Allocates; nothing is made again while the scale stays.
    pub fn set_text_scale(&mut self, scale: f32) -> Result<bool, RenderError> {
        let scale = valid_scale(scale);
        if scale == self.text_scale {
            return Ok(false);
        }
        let formats = Formats::new(&self.dwrite, scale)?;
        let atlas = build_atlas(&self._d2d, &self.dwrite, &formats)?;
        self.formats = formats;
        self.atlas = atlas;
        self.layouts.maps.iter_mut().for_each(HashMap::clear);
        self.layouts.entries = 0;
        self.text_scale = scale;
        Ok(true)
    }

    /// Attaches a composition tree to `window`.
    pub fn attach(&self, window: &OverlayWindow) -> Result<WindowTarget, RenderError> {
        // SAFETY: the window is alive and owned by this thread; returned objects are owned by `WindowTarget`.
        unsafe {
            let target = self
                .dcomp
                .CreateTargetForHwnd(window.hwnd(), true)
                .map_err(fail("CreateTargetForHwnd"))?;
            let root = self.dcomp.CreateVisual().map_err(fail("CreateVisual"))?;
            target.SetRoot(&root).map_err(fail("SetRoot"))?;
            Ok(WindowTarget {
                _target: target,
                root,
            })
        }
    }

    /// Creates a `width` x `height` layer at (`x`, `y`) in `window`. A scrolling layer treats its surface as a
    /// ring of columns and shows it through two visuals; see [`Renderer::scroll`].
    pub fn layer(
        &self,
        window: &WindowTarget,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        scrolling: bool,
    ) -> Result<Layer, RenderError> {
        // SAFETY: COM calls on live device objects; the visuals and surface are owned by `Layer`.
        unsafe {
            let surface = self
                .dcomp
                .CreateSurface(
                    width.max(1),
                    height.max(1),
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_ALPHA_MODE_PREMULTIPLIED,
                )
                .map_err(fail("CreateSurface"))?;
            let mut visuals = Vec::with_capacity(2);
            for _ in 0..if scrolling { 2 } else { 1 } {
                let visual = self.dcomp.CreateVisual().map_err(fail("CreateVisual"))?;
                visual.SetContent(&surface).map_err(fail("SetContent"))?;
                visual.SetOffsetX2(x as f32).map_err(fail("SetOffsetX"))?;
                visual.SetOffsetY2(y as f32).map_err(fail("SetOffsetY"))?;
                window
                    .root
                    .AddVisual(&visual, false, None::<&IDCompositionVisual>)
                    .map_err(fail("AddVisual"))?;
                visuals.push(visual);
            }
            let mut layer = Layer {
                visuals,
                surface,
                x,
                width,
                height,
                head: 0,
                visible: true,
            };
            if scrolling {
                self.place_scroll(&mut layer, 0)?;
            }
            Ok(layer)
        }
    }

    /// Shows a scrolling layer starting at column `head`: columns `head..width` on the left, then `0..head`.
    /// Moving the head is a visual tree change; nothing is redrawn.
    pub fn scroll(&mut self, layer: &mut Layer, head: u32) -> Result<(), RenderError> {
        if layer.visuals.len() == 2 {
            self.place_scroll(layer, head % layer.width.max(1))?;
            self.dirty = true;
        }
        Ok(())
    }

    /// Gives a layer a new surface of `width` x `height`, shown by the same visuals. The surface's content is
    /// undefined until it is drawn, so the caller draws all of it before the next commit. A composition surface
    /// cannot change its size; only resizing a widget in *Move widgets* makes new ones.
    pub fn resize_layer(
        &mut self,
        layer: &mut Layer,
        width: u32,
        height: u32,
    ) -> Result<(), RenderError> {
        if layer.size() == (width, height) {
            return Ok(());
        }
        // SAFETY: COM calls on live device objects; the new surface replaces the old one in `layer`, which
        // releases it, and the visuals hold their own references.
        unsafe {
            let surface = self
                .dcomp
                .CreateSurface(
                    width.max(1),
                    height.max(1),
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_ALPHA_MODE_PREMULTIPLIED,
                )
                .map_err(fail("CreateSurface"))?;
            for visual in &layer.visuals {
                visual.SetContent(&surface).map_err(fail("SetContent"))?;
            }
            layer.surface = surface;
        }
        (layer.width, layer.height) = (width, height);
        if layer.visuals.len() == 2 {
            self.place_scroll(layer, layer.head % width.max(1))?;
        }
        self.dirty = true;
        Ok(())
    }

    /// Moves a layer to (`x`, `y`) in its window without redrawing it.
    pub fn move_layer(&mut self, layer: &mut Layer, x: i32, y: i32) -> Result<(), RenderError> {
        layer.x = x;
        for visual in &layer.visuals {
            // SAFETY: the visual is alive and owned by `layer`.
            unsafe {
                visual.SetOffsetX2(x as f32).map_err(fail("SetOffsetX"))?;
                visual.SetOffsetY2(y as f32).map_err(fail("SetOffsetY"))?;
            }
        }
        if layer.visuals.len() == 2 {
            // A scrolling layer's two visuals sit at offsets from `x` that depend on the head.
            self.place_scroll(layer, layer.head)?;
        }
        self.dirty = true;
        Ok(())
    }

    fn place_scroll(&self, layer: &mut Layer, head: u32) -> Result<(), RenderError> {
        layer.head = head;
        let (x, w, h) = (layer.x as f32, layer.width as f32, layer.height as f32);
        let head = head as f32;
        // Clips are in the visual's own (surface) coordinates, before its offset.
        let parts = [(x - head, head, w), (x + w - head, 0.0, head)];
        for (visual, (offset, from, to)) in layer.visuals.iter().zip(parts) {
            let clip = D2D_RECT_F {
                left: from,
                top: 0.0,
                right: to,
                bottom: h,
            };
            // SAFETY: the visual is alive and owned by `layer`.
            unsafe {
                visual.SetOffsetX2(offset).map_err(fail("SetOffsetX"))?;
                visual.SetClip2(&clip).map_err(fail("SetClip"))?;
            }
        }
        Ok(())
    }

    /// Draws `area` of `layer` (the whole layer when `None`) with `paint`, which receives a canvas cleared to
    /// transparent and clipped to the area. With `scaled`, the canvas and `area` are in widget pixels, which are
    /// [`Renderer::text_scale`] layer pixels each; without, widget pixels are layer pixels.
    pub fn draw(
        &mut self,
        layer: &Layer,
        area: Option<Rect>,
        scaled: bool,
        paint: impl FnOnce(&mut Canvas<'_>),
    ) -> Result<(), RenderError> {
        let scale = if scaled { self.text_scale } else { 1.0 };
        let area = area.map_or(
            Rect::new(0.0, 0.0, layer.width as f32, layer.height as f32),
            |a| Rect::new(a.x * scale, a.y * scale, a.w * scale, a.h * scale),
        );
        let update = RECT {
            left: area.x.floor().max(0.0) as i32,
            top: area.y.floor().max(0.0) as i32,
            right: ((area.x + area.w).ceil() as i32).min(layer.width as i32),
            bottom: ((area.y + area.h).ceil() as i32).min(layer.height as i32),
        };
        if update.right <= update.left || update.bottom <= update.top {
            return Ok(());
        }
        let mut offset = POINT::default();
        // SAFETY: the surface is alive; BeginDraw returns a device context targeting the update area at
        // `offset`, valid until EndDraw, which is always called below.
        unsafe {
            let context: ID2D1DeviceContext = layer
                .surface
                .BeginDraw(Some(&update), &mut offset)
                .map_err(fail("BeginDraw"))?;
            // Work in layer pixels: shift the layer's update origin to where the surface placed it.
            context.SetDpi(96.0, 96.0);
            context.SetTransform(&Matrix3x2 {
                M11: 1.0,
                M12: 0.0,
                M21: 0.0,
                M22: 1.0,
                M31: (offset.x - update.left) as f32,
                M32: (offset.y - update.top) as f32,
            });
            let clip = D2D_RECT_F {
                left: update.left as f32,
                top: update.top as f32,
                right: update.right as f32,
                bottom: update.bottom as f32,
            };
            context.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            context.Clear(Some(&Color::TRANSPARENT.d2d()));
            {
                let mut canvas = Canvas {
                    context: &context,
                    brush: &self.brush,
                    factory: &self.factory,
                    round: &self.round,
                    dwrite: &self.dwrite,
                    formats: &self.formats,
                    atlas: &self.atlas,
                    layouts: &mut self.layouts,
                    text: &mut self.text,
                    background: self.background,
                    scale,
                    bounds: (layer.width as f32 / scale, layer.height as f32 / scale),
                    area: Rect::new(
                        update.left as f32 / scale,
                        update.top as f32 / scale,
                        (update.right - update.left) as f32 / scale,
                        (update.bottom - update.top) as f32 / scale,
                    ),
                };
                paint(&mut canvas);
            }
            context.PopAxisAlignedClip();
            layer.surface.EndDraw().map_err(fail("EndDraw"))?;
        }
        self.dirty = true;
        Ok(())
    }

    /// Shows or hides a layer without redrawing it.
    pub fn set_visible(&mut self, layer: &mut Layer, visible: bool) -> Result<(), RenderError> {
        if layer.visible == visible {
            return Ok(());
        }
        for visual in &layer.visuals {
            // SAFETY: the visual is alive; IDCompositionVisual3 is available on Windows 8.1 and later.
            unsafe {
                let visual: IDCompositionVisual3 =
                    visual.cast().map_err(fail("IDCompositionVisual3"))?;
                visual.SetVisible(visible).map_err(fail("SetVisible"))?;
            }
        }
        layer.visible = visible;
        self.dirty = true;
        Ok(())
    }

    /// Commits pending changes to the compositor. Does nothing when nothing changed.
    pub fn commit(&mut self) -> Result<bool, RenderError> {
        if !self.dirty {
            return Ok(false);
        }
        // SAFETY: commits the batch of this device.
        unsafe { self.dcomp.Commit() }.map_err(fail("Commit"))?;
        self.dirty = false;
        Ok(true)
    }

    /// Commits the initial, empty trees so windows show nothing until widgets draw.
    pub fn commit_now(&mut self) -> Result<(), RenderError> {
        self.dirty = true;
        self.commit().map(|_| ())
    }
}

/// A window's composition target and root visual.
#[derive(Debug)]
pub struct WindowTarget {
    _target: IDCompositionTarget,
    root: IDCompositionVisual2,
}

/// A surface shown by one visual, or by two for a scrolling layer.
#[derive(Debug)]
pub struct Layer {
    visuals: Vec<IDCompositionVisual2>,
    surface: IDCompositionSurface,
    x: i32,
    width: u32,
    height: u32,
    /// First column shown, for a scrolling layer.
    head: u32,
    visible: bool,
}

impl Layer {
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Whether it is a scrolling layer, shown by two visuals.
    pub fn scrolling(&self) -> bool {
        self.visuals.len() == 2
    }
}

/// The scale the renderer accepts: [`config::text_scale`]'s range, and 1 for anything that is not a number.
fn valid_scale(scale: f32) -> f32 {
    let (min, max) = (
        config::text_scale(config::MIN_TEXT_SIZE),
        config::text_scale(config::MAX_TEXT_SIZE),
    );
    if scale.is_finite() {
        scale.clamp(min, max)
    } else {
        1.0
    }
}

/// `v` widget pixels in layer pixels at `scale`. A whole number of widget pixels lands on a whole layer pixel, so
/// edges that are sharp at the default text size stay sharp and edges that meet stay together.
fn scaled_edge(v: f32, scale: f32) -> f32 {
    let p = v * scale;
    if v == v.round() { p.round() } else { p }
}

/// The edges `low` and `high` (`low <= high`) of a span in widget pixels, in layer pixels at `scale`: see
/// [`scaled_edge`]. A span of at least a pixel keeps at least a pixel, so hairlines do not vanish.
fn scaled_span(low: f32, high: f32, scale: f32) -> (f32, f32) {
    let (a, b) = (scaled_edge(low, scale), scaled_edge(high, scale));
    if high - low >= 1.0 && b - a < 1.0 {
        (a, a + 1.0)
    } else {
        (a, b)
    }
}

/// Safe drawing API over a surface's device context. Widgets draw in widget pixels: layer pixels at the default
/// text size, and [`Renderer::text_scale`] layer pixels each for a widget that scales with the text, so its text,
/// rows, and spacing grow together within its layer. At a scale other than 1, text that does not fit is left out
/// whole instead of cut: text outside the layer, and atlas text (numbers) wider than its box.
pub struct Canvas<'a> {
    context: &'a ID2D1DeviceContext,
    brush: &'a ID2D1SolidColorBrush,
    factory: &'a ID2D1Factory1,
    round: &'a ID2D1StrokeStyle1,
    dwrite: &'a IDWriteFactory,
    formats: &'a Formats,
    atlas: &'a [AtlasFace],
    layouts: &'a mut LayoutCache,
    text: &'a mut Vec<u16>,
    /// `[general] background_opacity`, in percent.
    background: u8,
    /// Layer pixels per widget pixel.
    scale: f32,
    /// The layer's width and height in widget pixels.
    bounds: (f32, f32),
    /// The area being redrawn, in widget pixels.
    area: Rect,
}

impl Canvas<'_> {
    /// The area being redrawn, in widget pixels; drawing outside it is clipped.
    pub fn area(&self) -> Rect {
        self.area
    }

    /// `rect` in layer pixels: see [`scaled_edge`].
    fn layer_rect(&self, rect: Rect) -> Rect {
        if self.scale == 1.0 {
            return rect;
        }
        let (x0, x1) = scaled_span(rect.x, rect.x + rect.w, self.scale);
        let (y0, y1) = scaled_span(rect.y, rect.y + rect.h, self.scale);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// A point in layer pixels.
    fn layer_point(&self, (x, y): (f32, f32)) -> Vector2 {
        Vector2 {
            X: x * self.scale,
            Y: y * self.scale,
        }
    }

    pub fn fill(&mut self, rect: Rect, color: Color) {
        let rect = self.layer_rect(rect);
        // SAFETY: drawing calls on a device context inside BeginDraw/EndDraw with a live brush.
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.context.FillRectangle(&rect.d2d(), self.brush);
        }
    }

    /// Fills `rect` with a widget's background: its panel, or a band behind content such as a header. `color`'s
    /// alpha is the background's own (from the widget's `opacity`), which `[general] background_opacity` scales
    /// (see [`config::background_alpha`]). Text, graphs, lines, and markers use [`Canvas::fill`] and stay as they
    /// are.
    pub fn fill_background(&mut self, rect: Rect, color: Color) {
        let a = config::background_alpha(color.a, self.background);
        if a > 0.0 {
            self.fill(rect, Color { a, ..color });
        }
    }

    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Color) {
        let (from, to, width) = self.layer_line(from, to, width);
        // SAFETY: drawing calls on a device context inside BeginDraw/EndDraw with a live brush.
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.context.DrawLine(from, to, self.brush, width, None);
        }
    }

    /// A line's ends and width in layer pixels. A level or upright line keeps its edges on whole pixels when they
    /// were (see [`scaled_edge`]), so a hairline stays sharp.
    fn layer_line(&self, from: (f32, f32), to: (f32, f32), width: f32) -> (Vector2, Vector2, f32) {
        let s = self.scale;
        if s == 1.0 {
            let point = |(x, y): (f32, f32)| Vector2 { X: x, Y: y };
            return (point(from), point(to), width);
        }
        let half = width.abs() / 2.0;
        if from.1 == to.1 {
            let (top, bottom) = scaled_span(from.1 - half, from.1 + half, s);
            let y = (top + bottom) / 2.0;
            let (x0, x1) = (scaled_edge(from.0, s), scaled_edge(to.0, s));
            (
                Vector2 { X: x0, Y: y },
                Vector2 { X: x1, Y: y },
                bottom - top,
            )
        } else if from.0 == to.0 {
            let (left, right) = scaled_span(from.0 - half, from.0 + half, s);
            let x = (left + right) / 2.0;
            let (y0, y1) = (scaled_edge(from.1, s), scaled_edge(to.1, s));
            (
                Vector2 { X: x, Y: y0 },
                Vector2 { X: x, Y: y1 },
                right - left,
            )
        } else {
            (self.layer_point(from), self.layer_point(to), width * s)
        }
    }

    /// Strokes a circle around `center`.
    pub fn circle(&mut self, center: (f32, f32), radius: f32, width: f32, color: Color) {
        let ellipse = D2D1_ELLIPSE {
            point: self.layer_point(center),
            radiusX: radius * self.scale,
            radiusY: radius * self.scale,
        };
        let width = width * self.scale;
        // SAFETY: drawing calls on a device context inside BeginDraw/EndDraw with a live brush.
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.context.DrawEllipse(&ellipse, self.brush, width, None);
        }
    }

    /// Fills a circle around `center`.
    pub fn fill_circle(&mut self, center: (f32, f32), radius: f32, color: Color) {
        let ellipse = D2D1_ELLIPSE {
            point: self.layer_point(center),
            radiusX: radius * self.scale,
            radiusY: radius * self.scale,
        };
        // SAFETY: drawing calls on a device context inside BeginDraw/EndDraw with a live brush.
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.context.FillEllipse(&ellipse, self.brush);
        }
    }

    /// Strokes the line through `points` with round joins, back to the first point when `closed`. Builds a path
    /// geometry on each call, so it suits layers drawn once, not every frame.
    pub fn polyline(&mut self, points: &[[f32; 2]], closed: bool, width: f32, color: Color) {
        let Some((first, rest)) = points.split_first() else {
            return;
        };
        let point = |p: &[f32; 2]| self.layer_point((p[0], p[1]));
        let width = width * self.scale;
        // SAFETY: factory and drawing calls inside BeginDraw/EndDraw; the geometry and its sink are owned here
        // and the sink is closed before the geometry is drawn.
        unsafe {
            let Ok(geometry) = self.factory.CreatePathGeometry() else {
                return;
            };
            let Ok(sink) = geometry.Open() else {
                return;
            };
            sink.BeginFigure(point(first), D2D1_FIGURE_BEGIN_HOLLOW);
            for p in rest {
                sink.AddLine(point(p));
            }
            sink.EndFigure(if closed {
                D2D1_FIGURE_END_CLOSED
            } else {
                D2D1_FIGURE_END_OPEN
            });
            if sink.Close().is_err() {
                return;
            }
            self.brush.SetColor(&color.d2d());
            self.context
                .DrawGeometry(&geometry, self.brush, width, self.round);
        }
    }

    /// Draws one line of text vertically centered in `rect`, clipped to it. `size` is in widget pixels, from the
    /// next of [`TEXT_SIZES`] up.
    ///
    /// Numbers and units come from the glyph atlas without DirectWrite work. Stable text such as names and labels
    /// uses a cached layout created on first use. Text that mixes digits with letters (counters, "46.0 laps")
    /// changes too often to cache and is drawn directly. No path allocates once warmed up.
    ///
    /// At a text scale other than 1, a `rect` that reaches outside the layer draws nothing, so text that does not
    /// fit a widget at a large text size is left out whole rather than cut at the widget's edge.
    pub fn text(&mut self, text: &str, rect: Rect, size: f32, align: Align, color: Color) {
        if text.is_empty() {
            return;
        }
        if self.scale != 1.0 && !self.inside(rect) {
            return;
        }
        let rect = self.layer_rect(rect);
        let (index, column) = Formats::index(size, align);
        // SAFETY: sets the color of a live brush.
        unsafe { self.brush.SetColor(&color.d2d()) };
        if let Some(face) = self.atlas.get(index)
            && face.covers(text)
        {
            self.atlas_text(face, text, rect, align);
            return;
        }
        if text.bytes().any(|b| b.is_ascii_digit()) {
            self.text.clear();
            self.text.extend(text.encode_utf16());
            // SAFETY: drawing call inside BeginDraw/EndDraw with a live format and brush.
            unsafe {
                self.context.DrawText(
                    self.text,
                    &self.formats.by_size[index][column],
                    &rect.d2d(),
                    self.brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            return;
        }
        let key = index * 3 + column;
        let fits = |entry: &&(f32, f32, IDWriteTextLayout)| entry.0 == rect.w && entry.1 == rect.h;
        let cached = |maps: &[HashMap<String, Boxes>]| {
            maps[key]
                .get(text)
                .and_then(|boxes| boxes.iter().find(fits))
                .is_some()
        };
        if !cached(&self.layouts.maps) {
            if self.layouts.entries >= MAX_CACHED_LAYOUTS {
                self.layouts.maps.iter_mut().for_each(HashMap::clear);
                self.layouts.entries = 0;
            }
            self.text.clear();
            self.text.extend(text.encode_utf16());
            let format = &self.formats.by_size[index][column];
            // SAFETY: plain DirectWrite call; the layout is owned by the cache.
            let Ok(layout) = (unsafe {
                self.dwrite
                    .CreateTextLayout(self.text, format, rect.w, rect.h)
            }) else {
                return;
            };
            let entry = (rect.w, rect.h, layout);
            match self.layouts.maps[key].get_mut(text) {
                Some(boxes) => boxes.push(entry),
                None => {
                    self.layouts.maps[key].insert(text.to_owned(), vec![entry]);
                }
            }
            self.layouts.entries += 1;
        }
        let layout = self.layouts.maps[key]
            .get(text)
            .and_then(|boxes| boxes.iter().find(fits));
        if let Some((_, _, layout)) = layout {
            // SAFETY: drawing call inside BeginDraw/EndDraw with a live layout and brush.
            unsafe {
                self.context.DrawTextLayout(
                    Vector2 {
                        X: rect.x,
                        Y: rect.y,
                    },
                    layout,
                    self.brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                );
            }
        }
    }

    /// Whether `text` at `size` fits the width of `rect` (both in widget pixels), for text the glyph atlas draws:
    /// numbers and units. Other text is not measured and counts as fitting.
    pub fn fits(&self, text: &str, rect: Rect, size: f32) -> bool {
        let (index, _) = Formats::index(size, Align::Left);
        match self.atlas.get(index) {
            Some(face) if face.covers(text) => {
                let width: f32 = text
                    .chars()
                    .filter_map(|c| face.glyph(c))
                    .map(|g| g.advance)
                    .sum();
                width <= self.layer_rect(rect).w + 0.5
            }
            _ => true,
        }
    }

    /// Whether `rect`, in widget pixels, lies within the layer. A pixel of slack covers the layer's size in widget
    /// pixels, which is rarely whole.
    fn inside(&self, rect: Rect) -> bool {
        let (w, h) = self.bounds;
        rect.x >= -1.0 && rect.y >= -1.0 && rect.x + rect.w <= w + 1.0 && rect.y + rect.h <= h + 1.0
    }

    /// Draws `text` from the atlas in `rect`, in layer pixels.
    fn atlas_text(&mut self, face: &AtlasFace, text: &str, rect: Rect, align: Align) {
        let width: f32 = text
            .chars()
            .filter_map(|c| face.glyph(c))
            .map(|g| g.advance)
            .sum();
        // At a text scale other than 1, a number wider than its box is left out rather than shown with digits
        // missing.
        if self.scale != 1.0 && width > rect.w + 0.5 {
            return;
        }
        let mut x = match align {
            Align::Left => rect.x,
            Align::Center => rect.x + (rect.w - width) / 2.0,
            Align::Right => rect.x + rect.w - width,
        };
        let y = (rect.y + (rect.h - face.cell_height) / 2.0).round();
        // SAFETY: drawing calls inside BeginDraw/EndDraw; opacity masks require aliased antialiasing, which is
        // restored afterwards.
        unsafe {
            self.context.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            for glyph in text.chars().filter_map(|c| face.glyph(c)) {
                let left = x.round();
                let target = D2D_RECT_F {
                    left,
                    top: y,
                    right: left + glyph.advance,
                    bottom: y + face.cell_height,
                };
                // Stay inside the text box, as the layout path clips.
                if left >= rect.x - 0.5 && left + glyph.advance <= rect.x + rect.w + 0.5 {
                    self.context.FillOpacityMask(
                        &face.bitmap,
                        self.brush,
                        Some(&target),
                        Some(&glyph.source),
                    );
                }
                x += glyph.advance;
            }
            self.context
                .SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_widget_pixels_land_on_whole_layer_pixels() {
        for scale in [0.7f32, 0.75, 1.15, 1.25, 1.35, 1.5] {
            for v in 0..400 {
                let p = scaled_edge(v as f32, scale);
                assert_eq!(p, p.round(), "{v} at {scale}");
                assert!((p - v as f32 * scale).abs() <= 0.5, "{v} at {scale}");
            }
        }
        // Between pixels, a coordinate is scaled as it is.
        assert_eq!(scaled_edge(10.5, 1.5), 15.75);
        assert_eq!(scaled_edge(33.25, 1.0), 33.25);
        // At the default text size nothing moves.
        assert_eq!(scaled_edge(17.0, 1.0), 17.0);
    }

    #[test]
    fn spans_keep_their_edges_together_and_hairlines_visible() {
        // Two rows that meet still meet.
        let (_, end) = scaled_span(0.0, 24.0, 1.15);
        let (start, _) = scaled_span(24.0, 48.0, 1.15);
        assert_eq!(end, start);
        // A one pixel line keeps a whole pixel at any scale.
        for scale in [0.7f32, 0.75, 1.15, 1.5] {
            for at in 0..100 {
                let (a, b) = scaled_span(at as f32, at as f32 + 1.0, scale);
                assert!(b - a >= 1.0, "{at} at {scale}: {a}..{b}");
                assert_eq!((a, b), (a.round(), b.round()), "sharp");
            }
        }
        assert_eq!(scaled_span(2.0, 3.0, 0.75), (2.0, 3.0), "not collapsed");
        assert_eq!(
            scaled_span(0.25, 0.75, 0.75),
            (0.1875, 0.5625),
            "thinner lines as they are"
        );
    }

    #[test]
    fn scales_outside_the_setting_are_brought_into_it() {
        assert_eq!(valid_scale(1.0), 1.0);
        assert_eq!(valid_scale(1.25), 1.25);
        assert_eq!(valid_scale(0.2), 0.7);
        assert_eq!(valid_scale(9.0), 1.5);
        assert_eq!(valid_scale(f32::NAN), 1.0);
        assert_eq!(valid_scale(f32::INFINITY), 1.0);
    }

    #[test]
    fn requested_text_sizes_pick_the_next_size_up() {
        assert_eq!(Formats::index(11.0, Align::Left), (0, 0));
        assert_eq!(Formats::index(13.0, Align::Center), (1, 1));
        assert_eq!(Formats::index(24.0, Align::Right), (3, 2));
        assert_eq!(Formats::index(40.0, Align::Left), (3, 0), "the largest");
    }
}
