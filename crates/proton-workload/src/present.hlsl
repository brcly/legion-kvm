cbuffer Frame : register(b0) { uint seed; uint width; uint iterations; };
struct Vertex { float4 position : SV_Position; };
Vertex vs_main(uint id : SV_VertexID) {
    Vertex v;
    v.position = float4(id == 2 ? 3.0 : -1.0, id == 1 ? 3.0 : -1.0, 0.0, 1.0);
    return v;
}
float4 color(Vertex input) {
    uint2 p = uint2(input.position.xy);
    uint load_bit = 0;
    uint rounds = iterations & 0x3fffffffu;
    if (rounds != 0) {
        uint state = seed ^ (p.x + p.y * width);
        [loop] for (uint i = 0; i < rounds; ++i)
            state = state * 1664525u + 1013904223u;
        load_bit = (state ^ (state >> 16)) & 1;
    }
    if ((iterations & 0x80000000u) != 0) {
        uint bit = min(31u, p.x * 32u / width);
        uint word = 0xa5c30000u | (seed & 65535u);
        if ((iterations & 0x40000000u) != 0 && p.y < 16u)
            word = 0xb7d10000u | (seed >> 16);
        uint c = ((word >> bit) & 1u) ? 230u : 20u;
        return float4(c + load_bit, c + load_bit, c + load_bit, 255) / 255.0;
    }
    uint r = (p.x + seed) & 255;
    uint g = (p.y + (seed >> 1)) & 255;
    uint b = ((((p.x / 64) ^ (p.y / 64) ^ (seed >> 7)) & 1) ? 192 : 32) + load_bit;
    return float4(r,g,b,255) / 255.0;
}
float4 ps_main(Vertex input) : SV_Target0 { return color(input); }
float4 ps_overlay(Vertex input) : SV_Target0 { return color(input).bgra; }
