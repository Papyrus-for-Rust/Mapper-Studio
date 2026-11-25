// Derived cartography only: never changes height, splat, topology or seeds.
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read> material: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<u32>;
fn color(i: u32) -> vec4<f32> {
    return vec4<f32>(material[4u*i],material[4u*i+1u],material[4u*i+2u],material[4u*i+3u]);
}
fn byte_at(offset:u32,i:u32)->f32 { return f32((src[offset+i/4u] >> ((i%4u)*8u)) & 255u); }
fn decode(v:u32)->f32 { return f32(v)/32766.0*1000.0-500.0; }
fn elevation(uv:vec2<f32>)->f32 {
    if material[51]!=0.0 && src[8]!=0u {return coarse_channel(uv,0u);}
    let n=src[0]; let p=clamp(uv,vec2(0.0),vec2(1.0))*f32(n-1u);
    let a=vec2<u32>(floor(p)); let b=min(a+vec2(1u),vec2(n-1u)); let t=fract(p);
    // Same four-sample sum and encoded-height conversion as the CPU path.
    let h=f32(src[18u+a.y*n+a.x])*(1.0-t.x)*(1.0-t.y)
        +f32(src[18u+a.y*n+b.x])*t.x*(1.0-t.y)
        +f32(src[18u+b.y*n+a.x])*(1.0-t.x)*t.y
        +f32(src[18u+b.y*n+b.x])*t.x*t.y;
    return h/32766.0*1000.0-500.0;
}
fn vertex(p:vec2<u32>)->vec3<f32> {
    let n=src[0]; let l=vec2<u32>(max(vec2<i32>(p)-vec2(1),vec2(0)));
    let r=min(p+vec2(1u),vec2(n-1u)); let d=f32(n-1u);
    let base=elevation(vec2<f32>(l)/d);
    return normalize(vec3(-(elevation(vec2(f32(r.x),f32(l.y))/d)-base)*0.5,
        f32(src[3])/f32(n),-(elevation(vec2(f32(l.x),f32(r.y))/d)-base)*0.5));
}
fn spherical(a:vec3<f32>,b:vec3<f32>,t:f32)->vec3<f32> {
    let d=clamp(dot(a,b),-1.0,1.0);
    if abs(d)>0.9995 {return normalize(a+(b-a)*t);}
    let angle=acos(d); let div=sin(angle);
    return normalize(a*(sin((1.0-t)*angle)/div)+b*(sin(t*angle)/div));
}
fn normal(uv:vec2<f32>)->vec3<f32> {
    let n=src[0];
    // Preserve the CPU's map-world-map coordinate round trip.
    let world=(uv-vec2(0.5))*f32(src[3]);
    let canonical=world/f32(src[3])+vec2(0.5);
    if material[50]!=0.0 && src[10]!=0u {return packed_normal(canonical);}
    let p=clamp(canonical,vec2(0.0),vec2(1.0))*f32(n-1u);
    let a=vec2<u32>(p); let b=min(a+vec2(1u),vec2(n-1u)); let t=fract(p);
    return spherical(spherical(vertex(a),vertex(vec2(b.x,a.y)),t.x),
        spherical(vertex(vec2(a.x,b.y)),vertex(b),t.x),t.y);
}
fn normal_texel(p:vec2<u32>,n:u32,offset:u32)->vec4<f32> {
    let word=src[src[10]+offset+p.y*n+p.x];
    return vec4<f32>(f32(word&255u),f32((word>>8u)&255u),f32((word>>16u)&255u),f32(word>>24u))/255.0;
}
fn sampled_normal(uv:vec2<f32>,n:u32,offset:u32)->vec4<f32> {
    let p=clamp(uv*f32(n)-vec2(0.5),vec2(0.0),vec2(f32(n-1u)));
    let a=vec2<u32>(p);let b=min(a+vec2(1u),vec2(n-1u));let t=fract(p);
    let aa=normal_texel(a,n,offset);let ab=normal_texel(vec2(b.x,a.y),n,offset);
    let ba=normal_texel(vec2(a.x,b.y),n,offset);let bb=normal_texel(b,n,offset);
    let lo=aa+(ab-aa)*t.x;let hi=ba+(bb-ba)*t.x;
    return lo+(hi-lo)*t.y;
}
fn packed_normal(uv:vec2<f32>)->vec3<f32> {
    let encoded=sampled_normal(uv,src[11],0u);
    let x=encoded.r*encoded.a*2.0-1.0;let z=encoded.g*2.0-1.0;
    // Preserve the source's nonunit result outside the XY unit circle.
    return vec3(x,sqrt(1.0-min(x*x+z*z,1.0)),z);
}
fn ground(uv:vec2<f32>)->vec4<f32> {
    let n=src[1]; let p=clamp(uv*f32(n)-vec2(0.5),vec2(0.0),vec2(f32(n-1u)));
    let a=vec2<u32>(floor(p)); let b=min(a+vec2(1u),vec2(n-1u)); let t=fract(p);
    let ids=array<u32,4>(a.y*n+a.x,a.y*n+b.x,b.y*n+a.x,b.y*n+b.x);
    let weights=array<f32,4>((1.0-t.x)*(1.0-t.y),t.x*(1.0-t.y),(1.0-t.x)*t.y,t.x*t.y);
    let order=array<u32,8>(7u,6u,3u,0u,4u,5u,2u,1u);
    var result=color(8u);
    for(var j=0u;j<8u;j++) {
        let channel=order[j]; var value=0.0;
        for(var k=0u;k<4u;k++) {value+=byte_at(src[4],channel*n*n+ids[k])*weights[k]/255.0;}
        let c=color(channel); let blend=clamp(value*c.a,0.0,1.0);
        result+=blend*(c-result);
    }
    return result;
}
// The older client explicitly reads height/slope with SampleLevel(..., 0).
fn coarse_channel(uv:vec2<f32>,channel:u32)->f32 {
    if src[8]==0u {return 0.0;}
    let n=src[9];let p=clamp(uv*f32(n)-vec2(0.5),vec2(0.0),vec2(f32(n-1u)));
    let a=vec2<u32>(p);let b=min(a+vec2(1u),vec2(n-1u));let t=fract(p);
    let aa=unpack2x16float(src[src[8]+a.y*n+a.x])[channel];
    let ab=unpack2x16float(src[src[8]+a.y*n+b.x])[channel];
    let ba=unpack2x16float(src[src[8]+b.y*n+a.x])[channel];
    let bb=unpack2x16float(src[src[8]+b.y*n+b.x])[channel];
    let lo=aa+(ab-aa)*t.x;let hi=ba+(bb-ba)*t.x;
    return lo+(hi-lo)*t.y;
}
fn surface_slope(uv:vec2<f32>)->f32 {
    if material[51]!=0.0 {
        if src[10]==0u {return 0.0;}
        let low=sampled_normal(uv,src[13],src[12]);
        let high=sampled_normal(uv,src[15],src[14]);
        let encoded=low+(high-low)*material[52];
        let lanes=encoded.ag*2.0-vec2(1.0);
        return lanes.x*lanes.x+lanes.y*lanes.y;
    }
    return coarse_channel(uv,1u);
}
fn shore_depth(uv:vec2<f32>,height:f32)->f32 {
    let n=src[17];
    let p=clamp(uv*f32(n)-vec2(0.5),vec2(0.0),vec2(f32(n-1u)));
    let a=vec2<u32>(p);let b=min(a+vec2(1u),vec2(n-1u));let t=fract(p);
    let words=array<u32,4>(src[src[16]+a.y*n+a.x],src[src[16]+a.y*n+b.x],
        src[src[16]+b.y*n+a.x],src[src[16]+b.y*n+b.x]);
    // Source no-boundary uploads filter to NaN. Test encoded distance bits
    // directly so backend floating-point contraction preserves that land gate.
    for(var i=0u;i<4u;i++) {if (words[i]&0x7c00u)==0x7c00u {return 0.0;}}
    let aa=unpack2x16float(words[0]);let ab=unpack2x16float(words[1]);
    let ba=unpack2x16float(words[2]);let bb=unpack2x16float(words[3]);
    let lo=aa+(ab-aa)*t.x;let hi=ba+(bb-ba)*t.x;
    let sampled=lo+(hi-lo)*t.y;
    let distance=sampled.x/(f32(n)/f32(src[3]));
    if !(distance>0.0) {return 0.0;}
    var depth=-height;
    if depth<=0.0 || sampled.y!=1.0 {depth=max(depth,0.1*distance);}
    return depth;
}
@compute @workgroup_size(8,8)
fn main(@builtin(global_invocation_id) id:vec3<u32>) {
    let output=src[2]; if id.x>=output || id.y>=output {return;}
    let uv=vec2((f32(id.x)+0.5)/f32(output),1.0-(f32(id.y)+0.5)/f32(output));
    let h=elevation(uv); let sr=src[1]; let hr=src[0];
    let cell=min(vec2<u32>(uv*f32(sr)),vec2(sr-1u));
    var sea=h<=0.0;
    if (src[7]&1u)!=0u {sea=byte_at(src[6],cell.y*sr+cell.x)==1.0;}
    var fresh=false; var level=0.0;
    if (src[7]&2u)!=0u {
        let w=min(vec2<u32>(uv*f32(hr)),vec2(hr-1u)); let encoded=src[src[5]+w.y*hr+w.x];
        level=decode(encoded); fresh=encoded!=0u && level>h;
    }
    var depth=level-h;if sea {depth=-h;}
    if src[16]!=0u {depth=shore_depth(uv,h);sea=depth>0.0;fresh=false;}
    var g=ground(uv); var rgb:vec3<f32>;
    if material[50]!=0.0 {
        let slope=surface_slope(uv);
        g=vec4(g.xyz*max(0.5,1.0-0.75*clamp((slope-0.2)*1.25,0.0,1.0)),g.a);
    }
    if sea || fresh {
        let near=clamp(0.5+max(depth,0.0)/5.0,0.0,1.0); let far=clamp(max(depth,0.0)/50.0,0.0,1.0);
        let shallow=g.xyz+near*(color(9u).xyz-g.xyz);
        var tone=shallow+far*(color(10u).xyz-shallow);
        if material[50] != 0.0 { tone=(tone-vec3(0.5))*material[49]+vec3(0.5); }
        rgb=tone*material[48]*255.0;
    } else {
        let sun=normalize(vec3(material[44],material[45],material[46]));
        var n=normal(uv);
        // The verified client shader dots its texture-space X/Z/up normal.
        // The same verified-shader flag also controls its water post-contrast.
        if material[50] != 0.0 { n=n.xzy; }
        let light=1.0+(max(dot(n,sun),0.0)-0.5)*material[47];
        rgb=((g.xyz*light-vec3(0.5))*material[49]+vec3(0.5))*material[48]*255.0;
    }
    let c=vec3<u32>(floor(clamp(rgb,vec3(0.0),vec3(255.0))+vec3(0.5)));
    dst[id.y*output+id.x]=c.x|(c.y<<8u)|(c.z<<16u)|0xff000000u;
}
