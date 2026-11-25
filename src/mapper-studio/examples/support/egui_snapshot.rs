// CPU-only widget inspection. Does not open an eframe window or exercise GPU rendering.
use eframe::egui;
#[derive(Default)]
pub struct Textures(std::collections::HashMap<egui::TextureId,egui::ColorImage>);
impl Textures {
    pub fn update(&mut self,delta:&egui::TexturesDelta) {
        for (id,changes) in &delta.set {
            for change in changes {
                let egui::ImageData::Color(image)=&change.image;
                if let Some([x,y])=change.pos {
                    let target=self.0.get_mut(id).expect("texture patch without atlas");
                    for row in 0..image.size[1] {
                        let start=(row+y)*target.size[0]+x;
                        target.pixels[start..start+image.size[0]].copy_from_slice(&image.pixels[row*image.size[0]..(row+1)*image.size[0]]);
                    }
                }else {self.0.insert(*id,image.as_ref().clone());}
            }
        }
    }
    pub fn raster(&self,ctx:&egui::Context,frame:egui::FullOutput,size:[usize;2])->egui::ColorImage {
        let mut output=egui::ColorImage::filled(size,ctx.style_of(ctx.theme()).visuals.window_fill());
        for primitive in ctx.tessellate(frame.shapes,frame.pixels_per_point) {
            let egui::epaint::Primitive::Mesh(mesh)=primitive.primitive else {continue;};
            let texture=self.0.get(&mesh.texture_id).expect("missing UI texture");
            let clip=primitive.clip_rect.intersect(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(size[0] as f32,size[1] as f32)));
            for triangle in mesh.indices.chunks_exact(3) {
                let v=triangle.iter().map(|&i|mesh.vertices[i as usize]).collect::<Vec<_>>();
                let edge=|a:egui::Pos2,b:egui::Pos2,p:egui::Pos2|(b.x-a.x)*(p.y-a.y)-(b.y-a.y)*(p.x-a.x);
                let area=edge(v[0].pos,v[1].pos,v[2].pos);
                if area.abs()<1e-6 {continue;}
                let lo=egui::pos2(v.iter().map(|v|v.pos.x).fold(f32::INFINITY,f32::min).floor().max(clip.min.x),
                    v.iter().map(|v|v.pos.y).fold(f32::INFINITY,f32::min).floor().max(clip.min.y));
                let hi=egui::pos2(v.iter().map(|v|v.pos.x).fold(f32::NEG_INFINITY,f32::max).ceil().min(clip.max.x),
                    v.iter().map(|v|v.pos.y).fold(f32::NEG_INFINITY,f32::max).ceil().min(clip.max.y));
                for y in (lo.y.max(0.) as usize)..(hi.y.max(0.) as usize).min(size[1]) {
                    for x in (lo.x.max(0.) as usize)..(hi.x.max(0.) as usize).min(size[0]) {
                        let p=egui::pos2(x as f32+0.5,y as f32+0.5);
                        let weights=[edge(v[1].pos,v[2].pos,p)/area,edge(v[2].pos,v[0].pos,p)/area,edge(v[0].pos,v[1].pos,p)/area];
                        if weights.iter().any(|w|*w<0.) {continue;}
                        let uv=(0..3).fold(egui::Vec2::ZERO,|sum,i|sum+v[i].uv.to_vec2()*weights[i]);
                        let tx=(uv.x*texture.size[0] as f32).clamp(0.,(texture.size[0]-1) as f32) as usize;
                        let ty=(uv.y*texture.size[1] as f32).clamp(0.,(texture.size[1]-1) as f32) as usize;
                        let tex=texture.pixels[ty*texture.size[0]+tx].to_array();
                        let color:[f32;4]=std::array::from_fn(|c|tex[c] as f32*(0..3).map(|i|v[i].color[c] as f32*weights[i]).sum::<f32>()/255.);
                        let old=output.pixels[y*size[0]+x].to_array();
                        let blended:[u8;4]=std::array::from_fn(|c|(color[c]+old[c] as f32*(1.-color[3]/255.)).clamp(0.,255.) as u8);
                        output.pixels[y*size[0]+x]=egui::Color32::from_rgba_premultiplied(blended[0],blended[1],blended[2],blended[3]);
                    }
                }
            }
        }
        output
    }
}
