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
    uint kind : KIND;     // 0=塗り 1=白黒グリフ 2=カラーグリフ 3=角の丸い四角（uv.x 半径・uv.y ぼかし。影にも）
};
struct VsOut {
    float4 pos : SV_Position;
    float2 uv : UV;
    float4 color : COLOR;
    nointerpolation uint kind : KIND;
    float2 local : LOCAL;                    // 四角の中の位置（px）
    nointerpolation float4 box : BOX;        // 幅・高さ・半径・ぼかし
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
    o.local = corner * i.rect.zw;
    o.box = float4(i.rect.zw, i.uv.xy);
    return o;
}

float4 ps(VsOut i) : SV_Target {
    if (i.kind == 1) return float4(i.color.rgb, i.color.a * mono.Sample(pt, i.uv));
    if (i.kind == 2) return colr.Sample(pt, i.uv);
    if (i.kind == 3) {
        // 角の丸い四角の符号付き距離（外が正）。ぼかしの幅で縁を薄くする（1 px なら縁のなめらかさ、大きければ影）
        float2 half_size = i.box.xy * 0.5;
        float r = min(i.box.z, min(half_size.x, half_size.y));
        float soft = max(i.box.w, 1.0);
        float2 q = abs(i.local - half_size) - (half_size - r - soft * 0.5);
        float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
        float a = saturate(0.5 - d / soft);
        return float4(i.color.rgb, i.color.a * a);
    }
    return i.color;
}
