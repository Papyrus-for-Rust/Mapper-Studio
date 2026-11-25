struct Parameters {
    side: u32,
    cells: u32,
    low_energy: u32,
    high_energy: u32,
}

@group(0) @binding(0) var<uniform> params: Parameters;
@group(0) @binding(1) var<storage, read> heights: array<u32>;
@group(0) @binding(2) var<storage, read_write> decisions: array<u32>;

fn height(index: u32) -> i32 {
    return i32((heights[index / 2u] >> ((index % 2u) * 16u)) & 65535u);
}

@compute @workgroup_size(128)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let first = id.x * 16u;
    if (first >= params.cells) {
        return;
    }
    var word = 0u;
    for (var lane = 0u; lane < 16u; lane++) {
        let i = first + lane;
        if (i >= params.cells) {
            break;
        }
        let x = i % params.side;
        let z = i / params.side;
        if (x == 0u || x + 1u == params.side || z == 0u || z + 1u == params.side) {
            continue;
        }
        let base = height(i - params.side - 1u);
        let dx = height(i - params.side + 1u) - base;
        let dz = height(i + params.side - 1u) - base;
        let energy = u32(dx * dx + dz * dz);
        if (energy > params.high_energy) {
            word |= 1u << (lane * 2u);
        } else if (energy >= params.low_energy) {
            word |= 2u << (lane * 2u);
        }
    }
    decisions[id.x] = word;
}
