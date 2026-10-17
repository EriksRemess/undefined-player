#include <metal_stdlib>
using namespace metal;

// The matching C struct uses only four-byte scalars, avoiding vector alignment
// differences between the host and Metal ABI.
struct ColorParams {
    uint depth, full_range, transfer, primaries;
    float kr, kb, peak_nits, white_nits;
    uint field, padding0, padding1, padding2;
};

float3 pq_linear(float3 value)
{
    // SMPTE ST 2084 EOTF, in absolute cd/m2.
    const float m1 = 2610.0 / 16384.0, m2 = 2523.0 / 32.0;
    const float c1 = 3424.0 / 4096.0, c2 = 2413.0 / 128.0, c3 = 2392.0 / 128.0;
    float3 p = pow(clamp(value, 0.0, 1.0), 1.0 / m2);
    return 10000.0 * pow(max(p - c1, 0.0) / max(c2 - c3 * p, 1e-6), 1.0 / m1);
}

float3 hlg_scene(float3 value)
{
    // Inverse ARIB STD-B67 OETF. The luminance-dependent OOTF is applied below.
    const float a = 0.17883277, b = 0.28466892, c = 0.55991073;
    return select(value * value / 3.0, (exp((value - c) / a) + b) / 12.0, value > 0.5);
}

float3 linear_709(float3 rgb, uint primaries)
{
    if (primaries == 9) { // BT.2020 -> D65 linear BT.709/scRGB
        return float3(dot(rgb, float3(1.6604910, -0.5876411, -0.0728499)),
                      dot(rgb, float3(-0.1245505, 1.1328999, -0.0083494)),
                      dot(rgb, float3(-0.0181508, -0.1005789, 1.1187297)));
    }
    if (primaries == 12) { // Display P3 -> D65 linear BT.709
        return float3(dot(rgb, float3(1.2249402, -0.2249402, 0.0)),
                      dot(rgb, float3(-0.0420570, 1.0420570, 0.0)),
                      dot(rgb, float3(-0.0196376, -0.0786360, 1.0982736)));
    }
    return rgb;
}

float3 sample_yuv(texture2d<float> luma, texture2d<float> chroma,
                  float2 pixel, constant ColorParams &p)
{
    constexpr sampler linear_sampler(coord::normalized, address::clamp_to_edge, filter::linear);
    const float2 coordinate = (pixel + 0.5) / float2(luma.get_width(), luma.get_height());
    float y = luma.sample(linear_sampler, coordinate).r;
    float2 uv = chroma.sample(linear_sampler, coordinate).rg;
    // P010 stores 10-bit samples in the high bits of a 16-bit UNORM plane.
    if (p.depth == 10) { y *= 65535.0 / 65472.0; uv *= 65535.0 / 65472.0; }
    float maximum = p.depth == 10 ? 1023.0 : 255.0;
    float scale = p.depth == 10 ? 4.0 : 1.0;
    float cb, cr;
    if (p.full_range) {
        cb = uv.x - (128.0 * scale / maximum);
        cr = uv.y - (128.0 * scale / maximum);
    } else {
        y = (y * maximum - 16.0 * scale) / (219.0 * scale);
        cb = (uv.x * maximum - 128.0 * scale) / (224.0 * scale);
        cr = (uv.y * maximum - 128.0 * scale) / (224.0 * scale);
    }
    float red = y + 2.0 * (1.0 - p.kr) * cr;
    float blue = y + 2.0 * (1.0 - p.kb) * cb;
    float green = (y - p.kr * red - p.kb * blue) / (1.0 - p.kr - p.kb);
    return max(float3(red, green, blue), 0.0);
}

float3 video_rgb(texture2d<float> luma, texture2d<float> chroma,
                 float2 position, constant ColorParams &p)
{
    float3 rgb;
    if (p.field && luma.get_height() > 1) {
        // Interpolate on the selected field's two-row grid. Sampling the
        // original image first at fractional y coordinates mixes the other
        // field back in whenever the video is resized.
        const float parity = float(p.field - 1);
        const float field_y = (position.y - parity) * 0.5;
        const float last = parity + 2 * floor((float(luma.get_height() - 1) - parity) * 0.5);
        const float above = clamp(2 * floor(field_y) + parity, parity, last);
        const float below = clamp(2 * floor(field_y) + parity + 2, parity, last);
        rgb = mix(sample_yuv(luma, chroma, float2(position.x, above), p),
                  sample_yuv(luma, chroma, float2(position.x, below), p), fract(field_y));
    } else {
        rgb = sample_yuv(luma, chroma, position, p);
    }
    if (p.transfer == 16) {
        rgb = pq_linear(rgb) / p.white_nits;
    } else if (p.transfer == 18) {
        rgb = hlg_scene(rgb);
        float luminance = max(dot(rgb, float3(p.kr, 1.0 - p.kr - p.kb, p.kb)), 1e-6);
        float gamma = 1.2 + 0.42 * log10(p.peak_nits / 1000.0);
        rgb *= pow(luminance, gamma - 1.0) * p.peak_nits / p.white_nits;
    } else if (p.transfer == 13) {
        rgb = select(rgb / 12.92, pow((rgb + 0.055) / 1.055, 2.4), rgb > 0.04045);
    } else {
        rgb = pow(rgb, 2.4); // BT.1886 display EOTF for SDR video.
    }
    return rgb;
}

kernel void video_linear(texture2d<float> luma [[texture(0)]],
                          texture2d<float> chroma [[texture(1)]],
                          texture2d<half, access::write> output [[texture(2)]],
                          constant ColorParams &p [[buffer(0)]], uint2 pixel [[thread_position_in_grid]])
{
    if (pixel.x >= output.get_width() || pixel.y >= output.get_height()) return;
    float2 position = (float2(pixel) + 0.5) * float2(luma.get_width(), luma.get_height()) /
                       float2(output.get_width(), output.get_height()) - 0.5;
    output.write(half4(half3(linear_709(video_rgb(luma, chroma, position, p), p.primaries)), 1.0h), pixel);
}

struct VideoVertex { float4 position [[position]]; float2 pixel; };
vertex VideoVertex video_vertex(uint index [[vertex_id]], constant float4 *vertices [[buffer(0)]])
{
    VideoVertex v;
    v.position = float4(vertices[index].xy, 0, 1);
    v.pixel = vertices[index].zw;
    return v;
}

fragment float4 video_fragment(VideoVertex v [[stage_in]], texture2d<float> luma [[texture(0)]],
                                texture2d<float> chroma [[texture(1)]],
                                constant ColorParams &p [[buffer(0)]],
                                constant float2 &tone [[buffer(1)]])
{
    float3 rgb = video_rgb(luma, chroma, v.pixel, p);
    // Same hue-preserving headroom curve as SDL's floating-point compositor.
    // Tone map in BT.2020, then retain extended/negative linear BT.709 values
    // for the EDR layer's display color management.
    rgb = linear_709(rgb, p.primaries);
    if (tone.y > 0) {
        float3 wide = float3(dot(rgb, float3(0.627404, 0.329283, 0.043313)),
                            dot(rgb, float3(0.069097, 0.919541, 0.011362)),
                            dot(rgb, float3(0.016391, 0.088013, 0.895595)));
        float peak = max(wide.r, max(wide.g, wide.b));
        if (peak > 0) wide *= (1 + tone.x * peak) / (1 + tone.y * peak);
        rgb = linear_709(wide, 9);
    }
    return float4(rgb, 1);
}
