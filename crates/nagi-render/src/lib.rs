//! D3D11 によるセルグリッド描画（ADR 0003, 0005, 0013）。
//!
//! 背景矩形・白黒グリフ・カラーグリフを 1 種類のインスタンスで表し、1 回の DrawInstanced で描く。
//! グリフは必要になった時点でラスタライズしてアトラスに詰める。


use nagi_text::{Fonts, GlyphKey};
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::core::{Interface, s};

static VS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/grid_vs.cso"));
static PS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/grid_ps.cso"));

/// アトラスの大きさ（px）。小さく始めて、入りきらなくなったら倍にする（2048 の白黒で 4 MB、1024 のカラーで 4 MB）
const MONO_START: u32 = 512;
const MONO_MAX: u32 = 2048;
const COLOR_START: u32 = 256;
const COLOR_MAX: u32 = 1024;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Instance {
    pub rect: [f32; 4],
    pub uv: [f32; 4],
    pub color: [f32; 4],
    pub kind: u32,
}

impl Instance {
    pub fn fill(x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) -> Self {
        Self { rect: [x, y, w, h], color, ..Default::default() }
    }
}

/// D3D11 デバイスと描画パイプライン。ウィンドウに依存しないので起動時に別スレッドで作れる。
pub struct Gpu {
    factory: IDXGIFactory2,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    uniform: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    pub adapter_name: String,
}

// D3D11 デバイスはフリースレッド。即時コンテキストは初期化スレッドから UI スレッドへ所有権ごと渡すだけで、同時には触らない。
unsafe impl Send for Gpu {}

impl Gpu {
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            // アダプタ 0（ディスプレイを駆動している GPU）だけを使い、他の GPU は起こさない（S4）
            let adapter = factory.EnumAdapters1(0)?;
            let desc = adapter.GetDesc1()?;
            let adapter_name = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_string();
            let (mut device, mut ctx) = (None, None);
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_SINGLETHREADED,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )?;
            let (device, ctx): (ID3D11Device, ID3D11DeviceContext) = (device.unwrap(), ctx.unwrap());
            let (mut vs, mut ps, mut layout, mut uniform, mut sampler, mut blend) = (None, None, None, None, None, None);
            device.CreateVertexShader(VS, None, Some(&mut vs))?;
            device.CreatePixelShader(PS, None, Some(&mut ps))?;
            let elem = |name, fmt, off| D3D11_INPUT_ELEMENT_DESC {
                SemanticName: name,
                SemanticIndex: 0,
                Format: fmt,
                InputSlot: 0,
                AlignedByteOffset: off,
                InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
                InstanceDataStepRate: 1,
            };
            device.CreateInputLayout(
                &[
                    elem(s!("RECT"), DXGI_FORMAT_R32G32B32A32_FLOAT, 0),
                    elem(s!("UV"), DXGI_FORMAT_R32G32B32A32_FLOAT, 16),
                    elem(s!("COLOR"), DXGI_FORMAT_R32G32B32A32_FLOAT, 32),
                    elem(s!("KIND"), DXGI_FORMAT_R32_UINT, 48),
                ],
                VS,
                Some(&mut layout),
            )?;
            device.CreateBuffer(
                &D3D11_BUFFER_DESC { ByteWidth: 32, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() },
                None,
                Some(&mut uniform),
            )?;
            device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;
            let mut bd = D3D11_BLEND_DESC::default();
            bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_SRC_ALPHA,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            device.CreateBlendState(&bd, Some(&mut blend))?;
            Ok(Self {
                factory,
                device,
                ctx,
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                layout: layout.unwrap(),
                uniform: uniform.unwrap(),
                sampler: sampler.unwrap(),
                blend: blend.unwrap(),
                adapter_name,
            })
        }
    }
}

/// アトラス上のグリフの位置と、セル左上からの描画オフセット
#[derive(Clone, Copy, Debug)]
pub struct AtlasGlyph {
    pub uv: [f32; 4],
    pub left: f32,
    pub top: f32,
    pub color: bool,
}

/// 棚詰め（shelf packing）。満杯になったら作り直す（M1 の簡易版）。
struct Shelf {
    size: u32,
    x: u32,
    y: u32,
    row_h: u32,
}

impl Shelf {
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.size || h > self.size {
            return None;
        }
        if self.x + w > self.size {
            self.x = 0;
            self.y += self.row_h + 1;
            self.row_h = 0;
        }
        if self.y + h > self.size {
            return None;
        }
        let p = (self.x, self.y);
        self.x += w + 1;
        self.row_h = self.row_h.max(h);
        Some(p)
    }
}

/// アトラスの 1 枚（テクスチャと、その大きさ）
struct Tex {
    tex: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
}

struct Atlas {
    device: ID3D11Device,
    mono: Tex,
    /// カラーの字（絵文字）を初めて描くときに作る（使わなければメモリを取らない）
    color: Option<Tex>,
    mono_shelf: Shelf,
    color_shelf: Shelf,
    /// None はラスタライズできなかった（空のグリフ）
    map: nagi_text::FastMap<GlyphKey, Option<AtlasGlyph>>,
    /// 描画中にその場でラスタライズした時間と個数（計測用。`take_raster_stats` で取り出して 0 に戻す）
    raster_time: std::time::Duration,
    raster_count: u32,
}

/// 正方形のテクスチャを作る
fn new_tex(device: &ID3D11Device, size: u32, fmt: DXGI_FORMAT) -> windows::core::Result<Tex> {
    let (mut t, mut v) = (None, None);
    unsafe {
        device.CreateTexture2D(
            &D3D11_TEXTURE2D_DESC {
                Width: size,
                Height: size,
                MipLevels: 1,
                ArraySize: 1,
                Format: fmt,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                ..Default::default()
            },
            None,
            Some(&mut t),
        )?;
        let t = t.unwrap();
        device.CreateShaderResourceView(&t, None, Some(&mut v))?;
        Ok(Tex { tex: t, srv: v.unwrap() })
    }
}

impl Atlas {
    fn new(device: &ID3D11Device) -> windows::core::Result<Self> {
        Ok(Self {
            device: device.clone(),
            mono: new_tex(device, MONO_START, DXGI_FORMAT_R8_UNORM)?,
            color: None,
            mono_shelf: Shelf { size: MONO_START, x: 0, y: 0, row_h: 0 },
            color_shelf: Shelf { size: COLOR_START, x: 0, y: 0, row_h: 0 },
            map: Default::default(),
            raster_time: Default::default(),
            raster_count: 0,
        })
    }

    /// 入りきらなくなったら倍の大きさに作り直し、今までの字を左上へ写す（字の位置は px なのでそのまま使える）
    fn grow(&mut self, ctx: &ID3D11DeviceContext, color: bool) -> bool {
        let (shelf, max, fmt) = if color { (&mut self.color_shelf, COLOR_MAX, DXGI_FORMAT_R8G8B8A8_UNORM) } else { (&mut self.mono_shelf, MONO_MAX, DXGI_FORMAT_R8_UNORM) };
        if shelf.size >= max {
            return false;
        }
        let Ok(t) = new_tex(&self.device, shelf.size * 2, fmt) else { return false };
        let old = if color { self.color.replace(t) } else { Some(std::mem::replace(&mut self.mono, t)) };
        let new = if color { self.color.as_ref().unwrap() } else { &self.mono };
        if let Some(old) = old {
            unsafe { ctx.CopySubresourceRegion(&new.tex, 0, 0, 0, 0, &old.tex, 0, None) };
        }
        shelf.size *= 2;
        true
    }

    fn get(&mut self, ctx: &ID3D11DeviceContext, fonts: &mut Fonts, key: GlyphKey) -> Option<AtlasGlyph> {
        if let Some(g) = self.map.get(&key) {
            return *g;
        }
        let g = self.insert(ctx, fonts, key);
        self.map.insert(key, g);
        g
    }

    fn insert(&mut self, ctx: &ID3D11DeviceContext, fonts: &mut Fonts, key: GlyphKey) -> Option<AtlasGlyph> {
        let t = std::time::Instant::now();
        let g = fonts.rasterize(key).and_then(|img| self.upload(ctx, &img));
        self.raster_time += t.elapsed();
        self.raster_count += 1;
        g
    }

    fn upload(&mut self, ctx: &ID3D11DeviceContext, img: &nagi_text::GlyphImage) -> Option<AtlasGlyph> {
        if img.width == 0 || img.height == 0 {
            return None;
        }
        if img.color && self.color.is_none() {
            self.color = Some(new_tex(&self.device, COLOR_START, DXGI_FORMAT_R8G8B8A8_UNORM).ok()?);
        }
        let (x, y) = loop {
            let shelf = if img.color { &mut self.color_shelf } else { &mut self.mono_shelf };
            if let Some(p) = shelf.alloc(img.width, img.height) {
                break p;
            }
            if !self.grow(ctx, img.color) {
                return None;
            }
        };
        let (tex, bpp) = if img.color { (&self.color.as_ref()?.tex, 4) } else { (&self.mono.tex, 1) };
        let bx = D3D11_BOX { left: x, top: y, front: 0, right: x + img.width, bottom: y + img.height, back: 1 };
        unsafe { ctx.UpdateSubresource(tex, 0, Some(&bx), img.data.as_ptr().cast(), img.width * bpp, 0) };
        Some(AtlasGlyph {
            uv: [x as f32, y as f32, img.width as f32, img.height as f32],
            left: img.left as f32,
            top: img.top as f32,
            color: img.color,
        })
    }
}

/// ウィンドウ 1 枚分の描画先（スワップチェーン）とアトラス
pub struct Renderer {
    gpu: Gpu,
    swap_chain: IDXGISwapChain2,
    waitable: HANDLE,
    flags: DXGI_SWAP_CHAIN_FLAG,
    rtv: Option<ID3D11RenderTargetView>,
    size: (u32, u32),
    instances: Option<ID3D11Buffer>,
    capacity: usize,
    atlas: Atlas,
}

impl Renderer {
    /// `hwnd` は winit のウィンドウハンドル（isize）
    pub fn new(gpu: Gpu, hwnd: isize, width: u32, height: u32) -> windows::core::Result<Self> {
        let flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width.max(1),
            Height: height.max(1),
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_NONE,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: flags.0 as u32,
        };
        let sc1 = unsafe { gpu.factory.CreateSwapChainForHwnd(&gpu.device, HWND(hwnd as *mut _), &desc, None, None)? };
        let swap_chain: IDXGISwapChain2 = sc1.cast()?;
        unsafe { swap_chain.SetMaximumFrameLatency(1)? };
        let waitable = unsafe { swap_chain.GetFrameLatencyWaitableObject() };
        let atlas = Atlas::new(&gpu.device)?;
        let mut r = Self { gpu, swap_chain, waitable, flags, rtv: None, size: (desc.Width, desc.Height), instances: None, capacity: 0, atlas };
        r.create_rtv()?;
        Ok(r)
    }

    fn create_rtv(&mut self) -> windows::core::Result<()> {
        let back: ID3D11Texture2D = unsafe { self.swap_chain.GetBuffer(0)? };
        let mut rtv = None;
        unsafe { self.gpu.device.CreateRenderTargetView(&back, None, Some(&mut rtv))? };
        self.rtv = rtv;
        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn adapter_name(&self) -> &str {
        &self.gpu.adapter_name
    }

    pub fn resize(&mut self, width: u32, height: u32) -> windows::core::Result<()> {
        if width == 0 || height == 0 || (width, height) == self.size {
            return Ok(());
        }
        self.rtv = None;
        unsafe {
            self.gpu.ctx.OMSetRenderTargets(None, None);
            self.swap_chain.ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, self.flags)?;
        }
        self.size = (width, height);
        self.create_rtv()
    }

    /// グリフをアトラスに用意して位置を返す
    pub fn glyph(&mut self, fonts: &mut Fonts, key: GlyphKey) -> Option<AtlasGlyph> {
        self.atlas.get(&self.gpu.ctx, fonts, key)
    }

    /// アトラスを空にする（文字の大きさが変わったとき）。テクスチャはそのまま上書きして使う
    pub fn clear_glyphs(&mut self) {
        let a = &mut self.atlas;
        a.map.clear();
        for shelf in [&mut a.mono_shelf, &mut a.color_shelf] {
            (shelf.x, shelf.y, shelf.row_h) = (0, 0, 0);
        }
    }

    /// 前回取り出してから、描画中にその場でラスタライズした (時間, 個数)
    pub fn take_raster_stats(&mut self) -> (std::time::Duration, u32) {
        let r = (self.atlas.raster_time, self.atlas.raster_count);
        self.atlas.raster_time = Default::default();
        self.atlas.raster_count = 0;
        r
    }

    /// まだアトラスに無いグリフを並列にラスタライズしてまとめて載せる
    pub fn prefetch(&mut self, fonts: &mut Fonts, keys: &[GlyphKey]) {
        let mut missing: Vec<GlyphKey> = keys.iter().copied().filter(|k| !self.atlas.map.contains_key(k)).collect();
        missing.sort_by_key(|k| (k.face, k.glyph, k.width));
        missing.dedup();
        let images = fonts.rasterize_many(&missing);
        self.insert_rasterized(images);
    }

    /// 先に（GPU の準備を待つ間に）ラスタライズしておいたグリフをアトラスに載せる
    pub fn insert_rasterized(&mut self, images: Vec<(GlyphKey, Option<nagi_text::GlyphImage>)>) {
        for (key, img) in images {
            if self.atlas.map.contains_key(&key) {
                continue;
            }
            let g = img.and_then(|img| self.atlas.upload(&self.gpu.ctx, &img));
            self.atlas.map.insert(key, g);
        }
    }

    /// waitable object を待つ（前フレームの表示が進むまで）。入力処理の前に呼ぶと入力の締め切りを遅らせられる。
    pub fn wait_frame(&self) {
        unsafe { WaitForSingleObjectEx(self.waitable, 100, true) };
    }

    pub fn draw(&mut self, insts: &[Instance], clear: [f32; 4]) -> windows::core::Result<()> {
        let ctx = &self.gpu.ctx;
        let stride = size_of::<Instance>() as u32;
        unsafe {
            if insts.len() > self.capacity {
                let cap = insts.len().next_power_of_two().max(4096);
                let mut buf = None;
                self.gpu.device.CreateBuffer(
                    &D3D11_BUFFER_DESC {
                        ByteWidth: (cap * size_of::<Instance>()) as u32,
                        Usage: D3D11_USAGE_DYNAMIC,
                        BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
                        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut buf),
                )?;
                self.instances = buf;
                self.capacity = cap;
            }
            let Some(buf) = &self.instances else { return Ok(()) };
            if !insts.is_empty() {
                let mut m = D3D11_MAPPED_SUBRESOURCE::default();
                ctx.Map(buf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m))?;
                std::ptr::copy_nonoverlapping(insts.as_ptr(), m.pData.cast(), insts.len());
                ctx.Unmap(buf, 0);
            }
            let screen = [
                self.size.0 as f32,
                self.size.1 as f32,
                self.atlas.mono_shelf.size as f32,
                self.atlas.mono_shelf.size as f32,
                self.atlas.color_shelf.size as f32,
                self.atlas.color_shelf.size as f32,
                0.0,
                0.0,
            ];
            ctx.UpdateSubresource(&self.gpu.uniform, 0, None, screen.as_ptr().cast(), 0, 0);
            let Some(rtv) = &self.rtv else { return Ok(()) };
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.OMSetBlendState(&self.gpu.blend, None, 0xffff_ffff);
            ctx.ClearRenderTargetView(rtv, &clear);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { Width: self.size.0 as f32, Height: self.size.1 as f32, MaxDepth: 1.0, ..Default::default() }]));
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.IASetInputLayout(&self.gpu.layout);
            ctx.IASetVertexBuffers(0, 1, Some(&self.instances), Some(&stride), Some(&0));
            ctx.VSSetShader(&self.gpu.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.gpu.uniform.clone())]));
            ctx.PSSetShader(&self.gpu.ps, None);
            ctx.PSSetShaderResources(0, Some(&[Some(self.atlas.mono.srv.clone()), self.atlas.color.as_ref().map(|c| c.srv.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.gpu.sampler.clone())]));
            ctx.DrawInstanced(4, insts.len() as u32, 0, 0);
            self.swap_chain.Present(0, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }
}
