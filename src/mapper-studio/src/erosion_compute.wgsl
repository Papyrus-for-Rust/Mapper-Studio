struct Policy {
    n: u32,
    dt: f32,
    fill: f32,
    evaporation: f32,
    gravity: f32,
    pipe_area: f32,
    pipe_length: f32,
    cell_area: f32,
    normal_y: f32,
    ocean: f32,
    minimum_angle: f32,
    capacity: f32,
    dissolve: f32,
    depth_fade: f32,
}

@group(0) @binding(0) var<uniform> policy: Policy;
@group(0) @binding(1) var<storage, read> original: array<f32>;
@group(0) @binding(2) var<storage, read> floor_height: array<f32>;
@group(0) @binding(3) var<storage, read_write> height: array<f32>;
@group(0) @binding(4) var<storage, read_write> next_height: array<f32>;
@group(0) @binding(5) var<storage, read_write> water: array<f32>;
@group(0) @binding(6) var<storage, read_write> velocity: array<vec2<f32>>;
@group(0) @binding(7) var<storage, read_write> flux: array<vec4<f32>>;
@group(0) @binding(8) var<storage, read_write> sediment: array<f32>;
@group(0) @binding(9) var<storage, read_write> next_sediment: array<f32>;

fn interior(p: vec2<u32>) -> bool {
    return all(p > vec2(0u)) && all(p < vec2(policy.n - 1u));
}

fn sum4(v: vec4<f32>) -> f32 { return ((v.x + v.y) + v.z) + v.w; }
fn surface(i: u32) -> f32 { return height[i] + water[i]; }
fn finite(value: f32) -> bool { return (bitcast<u32>(value) & 0x7f800000u) != 0x7f800000u; }

fn advected(p: vec2<u32>, speed: vec2<f32>) -> u32 {
    let xy = vec2<u32>(clamp(vec2<i32>(vec2<f32>(p) - policy.dt * speed),
                            vec2(0), vec2(i32(policy.n - 1u))));
    return xy.y * policy.n + xy.x;
}

@compute @workgroup_size(16, 8)
fn initialize(@builtin(global_invocation_id) p: vec3<u32>) {
    if any(p.xy >= vec2(policy.n)) { return; }
    let i = p.y * policy.n + p.x;
    height[i] = original[i];
    water[i] = select(0.0, policy.ocean - original[i], original[i] <= policy.ocean) + policy.fill;
}

@compute @workgroup_size(16, 8)
fn fill_flux(@builtin(global_invocation_id) p: vec3<u32>) {
    if !interior(p.xy) { return; }
    let n = policy.n;
    let i = p.y * n + p.x;
    let around = vec4(surface(i-1u), surface(i+1u), surface(i+n), surface(i-n));
    let q = max(vec4(0.0), flux[i] + (policy.dt * policy.pipe_area)
        * ((policy.gravity * (vec4(surface(i)) - around)) / policy.pipe_length));
    let total = sum4(q) * policy.dt;
    var factor = 1.0;
    if total != 0.0 { factor = min(water[i] * policy.cell_area / total, 1.0); }
    flux[i] = q * factor;
}

fn lower(p: vec2<u32>, refill: bool) {
    let n = policy.n;
    if any(p >= vec2(n)) { return; }
    let i = p.y * n + p.x;
    var speed = velocity[i];
    var carried = sediment[advected(p, speed)];
    var w = water[i];
    let inside = interior(p);
    if inside {
        let q = flux[i];
        let incoming = vec4(flux[i-1u].y, flux[i+1u].x, flux[i+n].w, flux[i-n].z);
        w += policy.dt * (sum4(incoming) - sum4(q)) * (1.0 / policy.cell_area);
        speed += vec2((incoming.x-q.x+q.y-incoming.y)*0.5,
                      (incoming.z-q.z+q.w-incoming.w)*0.5) * policy.dt;
        velocity[i] = speed;
    }
    let h = height[i];
    var next = h;
    if w < policy.depth_fade || h == 0.0 {
        let depth = clamp(w / policy.depth_fade, 0.0, 1.0);
        let shallow = 1.0 - depth * depth * (3.0 - 2.0 * depth);
        var amount = 0.0;
        if shallow != 0.0 || h == 0.0 {
            var angle = 0.0;
            if inside {
                let x = (height[i-n+1u] - height[i-n-1u]) * -0.5;
                let z = (height[i+n-1u] - height[i-n-1u]) * -0.5;
                let y = policy.normal_y;
                let normal = vec3(x, y, z) * (1.0 / sqrt(x*x + y*y + z*z));
                let square = normal.x*normal.x + normal.y*normal.y + normal.z*normal.z;
                angle = acos(clamp(normal.y / sqrt(square) / square, -1.0, 1.0));
            }
            let capacity = (policy.capacity * sin(max(angle, policy.minimum_angle)))
                * sqrt(speed.x*speed.x + speed.y*speed.y);
            amount = abs(policy.dt * policy.dissolve * (capacity - carried) * shallow);
        }
        let lowered = h - amount;
        next = max(lowered, floor_height[i]);
        let returned = next - lowered;
        carried += returned;
        w += returned;
    }
    next_height[i] = next;
    next_sediment[i] = carried;
    water[i] = select(w * policy.evaporation, policy.ocean - next, refill && original[i] <= policy.ocean)
        + select(0.0, policy.fill, refill);
}

@compute @workgroup_size(16, 8)
fn lower_height(@builtin(global_invocation_id) p: vec3<u32>) { lower(p.xy, true); }

@compute @workgroup_size(16, 8)
fn finish_height(@builtin(global_invocation_id) p: vec3<u32>) { lower(p.xy, false); }

@compute @workgroup_size(16, 8)
fn validate(@builtin(global_invocation_id) p: vec3<u32>) {
    if any(p.xy >= vec2(policy.n)) { return; }
    let i = p.y * policy.n + p.x;
    let speed = velocity[i];
    let q = flux[i];
    if !finite(speed.x) || !finite(speed.y) || !finite(water[i]) ||
        !finite(q.x) || !finite(q.y) || !finite(q.z) || !finite(q.w) ||
        !finite(sediment[advected(p.xy, speed)]) {
        next_height[i] = bitcast<f32>(0x7fc00000u);
    } else {
        next_height[i] = height[i];
    }
}
