//! Render backend: one D3D11 device shared by Direct2D, DirectWrite, and a DirectComposition visual tree.
//!
//! Each widget gets a [`Layer`]: a composition visual with its own surface. Drawing updates only the requested
//! rectangle of that surface, and nothing is committed unless something was drawn. All COM `unsafe` for
//! rendering lives here; widgets draw through the safe [`Canvas`].

use std::collections::HashMap;

use windows::Win32::Foundation::{HMODULE, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1,
    ID2D1SolidColorBrush,
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

/// Text sizes available to widgets; formats are created once.
pub const TEXT_SIZES: [f32; 4] = [12.0, 14.0, 18.0, 26.0];

#[derive(Debug)]
struct Formats {
    /// Indexed by size, then alignment.
    by_size: Vec<[IDWriteTextFormat; 3]>,
}

impl Formats {
    fn new(dwrite: &IDWriteFactory) -> Result<Self, RenderError> {
        let family = HSTRING::from("Segoe UI");
        let mut by_size = Vec::with_capacity(TEXT_SIZES.len());
        for &size in &TEXT_SIZES {
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

/// Text layouts for strings that are not in the atlas, created on first use and reused.
#[derive(Debug, Default)]
struct LayoutCache {
    /// Per (size, alignment), text to (layout box width, height, layout).
    maps: Vec<HashMap<String, (f32, f32, IDWriteTextLayout)>>,
    entries: usize,
}

/// More cached layouts than this clears the cache, bounding memory if text keeps changing.
const MAX_CACHED_LAYOUTS: usize = 512;

/// Device-wide state shared by every window and layer.
#[derive(Debug)]
pub struct Renderer {
    _d3d: ID3D11Device,
    _d2d: ID2D1Device,
    dcomp: IDCompositionDesktopDevice,
    brush: ID2D1SolidColorBrush,
    dwrite: IDWriteFactory,
    formats: Formats,
    atlas: Vec<AtlasFace>,
    layouts: LayoutCache,
    text: Vec<u16>,
    /// Whether a layer was drawn since the last commit.
    dirty: bool,
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
    /// Creates the device on the default GPU, falling back to the WARP software rasterizer.
    pub fn new() -> Result<Self, RenderError> {
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
            let formats = Formats::new(&dwrite)?;
            let atlas = build_atlas(&d2d, &dwrite, &formats)?;
            Ok(Self {
                _d3d: d3d,
                _d2d: d2d,
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
                software,
            })
        }
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
    /// transparent and clipped to the area.
    pub fn draw(
        &mut self,
        layer: &Layer,
        area: Option<Rect>,
        paint: impl FnOnce(&mut Canvas<'_>),
    ) -> Result<(), RenderError> {
        let area = area.unwrap_or(Rect::new(0.0, 0.0, layer.width as f32, layer.height as f32));
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
                    dwrite: &self.dwrite,
                    formats: &self.formats,
                    atlas: &self.atlas,
                    layouts: &mut self.layouts,
                    text: &mut self.text,
                    area: Rect::new(
                        update.left as f32,
                        update.top as f32,
                        (update.right - update.left) as f32,
                        (update.bottom - update.top) as f32,
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
}

/// Safe drawing API over a surface's device context, in layer pixels.
pub struct Canvas<'a> {
    context: &'a ID2D1DeviceContext,
    brush: &'a ID2D1SolidColorBrush,
    dwrite: &'a IDWriteFactory,
    formats: &'a Formats,
    atlas: &'a [AtlasFace],
    layouts: &'a mut LayoutCache,
    text: &'a mut Vec<u16>,
    area: Rect,
}

impl Canvas<'_> {
    /// The area being redrawn, in layer pixels; drawing outside it is clipped.
    pub fn area(&self) -> Rect {
        self.area
    }

    pub fn fill(&mut self, rect: Rect, color: Color) {
        // SAFETY: drawing calls on a device context inside BeginDraw/EndDraw with a live brush.
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.context.FillRectangle(&rect.d2d(), self.brush);
        }
    }

    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Color) {
        // SAFETY: drawing calls on a device context inside BeginDraw/EndDraw with a live brush.
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.context.DrawLine(
                Vector2 {
                    X: from.0,
                    Y: from.1,
                },
                Vector2 { X: to.0, Y: to.1 },
                self.brush,
                width,
                None,
            );
        }
    }

    /// Strokes a circle around `center`.
    pub fn circle(&mut self, center: (f32, f32), radius: f32, width: f32, color: Color) {
        let ellipse = D2D1_ELLIPSE {
            point: Vector2 {
                X: center.0,
                Y: center.1,
            },
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: drawing calls on a device context inside BeginDraw/EndDraw with a live brush.
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.context.DrawEllipse(&ellipse, self.brush, width, None);
        }
    }

    /// Draws one line of text vertically centered in `rect`, clipped to it.
    ///
    /// Numbers and units come from the glyph atlas without DirectWrite work. Stable text such as names and labels
    /// uses a cached layout created on first use. Text that mixes digits with letters (counters, "46.0 laps")
    /// changes too often to cache and is drawn directly. No path allocates once warmed up.
    pub fn text(&mut self, text: &str, rect: Rect, size: f32, align: Align, color: Color) {
        if text.is_empty() {
            return;
        }
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
        let fits = |entry: &(f32, f32, IDWriteTextLayout)| entry.0 == rect.w && entry.1 == rect.h;
        if !self.layouts.maps[key].get(text).is_some_and(fits) {
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
            if self.layouts.maps[key]
                .insert(text.to_owned(), entry)
                .is_none()
            {
                self.layouts.entries += 1;
            }
        }
        if let Some((_, _, layout)) = self.layouts.maps[key].get(text) {
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

    fn atlas_text(&mut self, face: &AtlasFace, text: &str, rect: Rect, align: Align) {
        let width: f32 = text
            .chars()
            .filter_map(|c| face.glyph(c))
            .map(|g| g.advance)
            .sum();
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
