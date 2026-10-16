// セルグリッド: 背景矩形・白黒グリフ（R8 アトラス）・カラーグリフ（RGBA8 アトラス）を 1 回のインスタンス描画で描く
cbuffer Screen : register(b0) {
    float2 screen;      // 画面サイズ（px）
    float2 mono_size;   // R8 アトラスのサイズ
    float2 color_size;  // RGBA8 アトラスのサイズ
    float2 pad;
};
Texture2D<float> mono : register(t0);
Texture2D<float4> colr : register(t1);
SamplerState pt : register(s0);

struct Inst {
    float4 rect : RECT;   // x, y, w, h（px）
    float4 uv : UV;       // アトラス上の x, y, w, h（px）
    float4 color : COLOR; // sRGB の straight alpha
    uint kind : KIND;     // 0=塗り 1=白黒グリフ 2=カラーグリフ
};
struct VsOut {
    float4 pos : SV_Position;
    float2 uv : UV;
    float4 color : COLOR;
    nointerpolation uint kind : KIND;
};

VsOut vs(uint vi : SV_VertexID, Inst i) {
    float2 corner = float2(vi & 1, vi >> 1);
    float2 px = i.rect.xy + corner * i.rect.zw;
    VsOut o;
    o.pos = float4(px.x / screen.x * 2.0 - 1.0, 1.0 - px.y / screen.y * 2.0, 0.0, 1.0);
    float2 size = i.kind == 2 ? color_size : mono_size;
    o.uv = (i.uv.xy + corner * i.uv.zw) / size;
    o.color = i.color;
    o.kind = i.kind;
    return o;
}

float4 ps(VsOut i) : SV_Target {
    if (i.kind == 1) return float4(i.color.rgb, i.color.a * mono.Sample(pt, i.uv));
    if (i.kind == 2) return colr.Sample(pt, i.uv);
    return i.color;
}
