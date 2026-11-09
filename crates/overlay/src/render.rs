//! Render backend: one D3D11 device shared by Direct2D, DirectWrite, and a DirectComposition visual tree.
//!
//! Each widget gets a [`Layer`]: a composition visual with its own surface. Drawing updates only the requested
//! rectangle of that surface, and nothing is committed unless something was drawn. All COM `unsafe` for
//! rendering lives here; widgets draw through the safe [`Canvas`].

use windows::Win32::Foundation::{HMODULE, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext,
    ID2D1Factory1, ID2D1SolidColorBrush,
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
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
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

    fn get(&self, size: f32, align: Align) -> &IDWriteTextFormat {
        let index = TEXT_SIZES
            .iter()
            .position(|&s| s >= size)
            .unwrap_or(TEXT_SIZES.len() - 1);
        let column = match align {
            Align::Left => 0,
            Align::Center => 1,
            Align::Right => 2,
        };
        &self.by_size[index][column]
    }
}

/// Device-wide state shared by every window and layer.
#[derive(Debug)]
pub struct Renderer {
    _d3d: ID3D11Device,
    _d2d: ID2D1Device,
    dcomp: IDCompositionDesktopDevice,
    brush: ID2D1SolidColorBrush,
    formats: Formats,
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
            Ok(Self {
                _d3d: d3d,
                _d2d: d2d,
                dcomp,
                brush,
                formats: Formats::new(&dwrite)?,
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

    /// Creates a `width` x `height` layer at (`x`, `y`) in `window`.
    pub fn layer(
        &self,
        window: &WindowTarget,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> Result<Layer, RenderError> {
        // SAFETY: COM calls on live device objects; the visual and surface are owned by `Layer`.
        unsafe {
            let visual = self.dcomp.CreateVisual().map_err(fail("CreateVisual"))?;
            let surface = self
                .dcomp
                .CreateSurface(
                    width.max(1),
                    height.max(1),
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_ALPHA_MODE_PREMULTIPLIED,
                )
                .map_err(fail("CreateSurface"))?;
            visual.SetContent(&surface).map_err(fail("SetContent"))?;
            visual.SetOffsetX2(x as f32).map_err(fail("SetOffsetX"))?;
            visual.SetOffsetY2(y as f32).map_err(fail("SetOffsetY"))?;
            window
                .root
                .AddVisual(&visual, false, None::<&IDCompositionVisual>)
                .map_err(fail("AddVisual"))?;
            Ok(Layer {
                visual,
                surface,
                width,
                height,
                visible: true,
            })
        }
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
                    formats: &self.formats,
                    text: &mut self.text,
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
        // SAFETY: the visual is alive; IDCompositionVisual3 is available on Windows 8.1 and later.
        unsafe {
            let visual: IDCompositionVisual3 =
                layer.visual.cast().map_err(fail("IDCompositionVisual3"))?;
            visual.SetVisible(visible).map_err(fail("SetVisible"))?;
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

/// One widget's visual and surface.
#[derive(Debug)]
pub struct Layer {
    visual: IDCompositionVisual2,
    surface: IDCompositionSurface,
    width: u32,
    height: u32,
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
    formats: &'a Formats,
    text: &'a mut Vec<u16>,
}

impl Canvas<'_> {
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

    /// Draws one line of text vertically centered in `rect`. Text that does not fit is clipped.
    pub fn text(&mut self, text: &str, rect: Rect, size: f32, align: Align, color: Color) {
        self.text.clear();
        self.text.extend(text.encode_utf16());
        // SAFETY: drawing calls on a device context inside BeginDraw/EndDraw with a live brush and format.
        unsafe {
            self.brush.SetColor(&color.d2d());
            self.context.DrawText(
                self.text,
                self.formats.get(size, align),
                &rect.d2d(),
                self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}
